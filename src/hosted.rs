//! The hosted backend: render the request's image, or a libretro core run
//! in this process, through the preset with librashader's Metal runtime,
//! and write the trace with Metal's capture API. No RetroArch process.

use crate::backend::{Backend, Request, Source, StateSource};
use crate::config::{self, Size};
use crate::layout::{DirResolver, Located, describe_tried, settle_frames};
use crate::{bundle, display, libretro, render, state};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The in-process backend. It has no configuration of its own: everything
/// it needs is in the request or RetroArch's on-disk layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hosted;

impl Backend for Hosted {
    /// Metal offers programmatic capture to a trace document only when
    /// `MTL_CAPTURE_ENABLED=1` is in the environment before it loads, so
    /// the process re-executes itself with it set unless it already is.
    /// `exec` only returns on failure, and after it the variable is `1`,
    /// so this never loops; if Metal still refuses, `render::run` says so.
    fn prepare(&self) -> Result<()> {
        use std::os::unix::process::CommandExt;
        if !needs_capture_env(std::env::var_os(render::CAPTURE_ENV).as_deref()) {
            return Ok(());
        }
        let exe = std::env::current_exe().context("locating this executable to re-exec it")?;
        let err = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .env(render::CAPTURE_ENV, "1")
            .exec();
        Err(err).with_context(|| {
            format!(
                "re-executing {} with {}=1",
                exe.display(),
                render::CAPTURE_ENV
            )
        })
    }

    fn run(&self, request: Request) -> Result<()> {
        let preset = request.shader.clone().context(
            "the librashader backend requires --shader: there is nothing to render without a preset",
        )?;
        if !preset.is_file() {
            bail!("shader preset not found at {}", preset.display());
        }
        // Refuse a bad output path before loading anything.
        bundle::prepare_output(&request.output)?;
        let screen = display::main_screen();

        let mut stdout: Option<StdoutToStderr> = None;
        let (source, warmup): (Box<dyn render::FrameSource>, u32) = match &request.source {
            Source::Image(image) => {
                validate_image(image)?;
                (Box::new(render::ImageSource::open(image)?), 0)
            }
            Source::Core {
                core: core_arg,
                rom,
                state,
                options,
                skip_extension_check,
            } => {
                libretro::refuse_zip(rom)?;
                if !rom.is_file() {
                    bail!("ROM not found at {}", rom.display());
                }
                let mut dirs = DirResolver::for_config(request.config.as_deref(), request.verbose);
                let core_path = if Path::new(core_arg).is_file() {
                    PathBuf::from(core_arg)
                } else {
                    match dirs.locate(
                        "core",
                        |d| d.libretro_dir.clone(),
                        |dir| crate::core::resolve_core(core_arg, dir).is_ok(),
                    ) {
                        Located::Found(dir) => crate::core::resolve_core(core_arg, &dir)?,
                        Located::Missing(tried) => {
                            bail!("core {core_arg} not found in {}", describe_tried(&tried))
                        }
                    }
                };
                let options = match options {
                    Some(path) => {
                        let text = std::fs::read_to_string(path)
                            .with_context(|| format!("reading core options {}", path.display()))?;
                        config::read_all(&text)
                    }
                    None => HashMap::new(),
                };
                let system_dir =
                    match dirs.locate("system directory", |d| d.system_dir.clone(), |p| p.is_dir())
                    {
                        Located::Found(dir) => dir,
                        // Nothing exists anywhere; the core gets the default path
                        // and will say so itself if it needs a file from it.
                        // `tried` always starts with the default candidate.
                        Located::Missing(tried) => match tried.first() {
                            Some(default) => default.clone(),
                            None => crate::layout::RetroArchDirs::defaults().system_dir,
                        },
                    };
                let tmp = tempfile::Builder::new()
                    .prefix("ra-metal-capture-")
                    .tempdir()
                    .context("creating temp dir")?;

                // A hosted core may print to stdout; keep that off the stream
                // this tool reports the output path on.
                stdout = Some(StdoutToStderr::redirect()?);
                // Open the core before initialising it: a core may read its
                // options as early as `retro_init`, so they travel in the Context.
                let mut core = libretro::Core::open(&core_path)?;
                let info = core.system_info();
                if !skip_extension_check && !info.accepts_extension(rom) {
                    bail!(
                        "{} does not have an extension {} accepts ({}); pass --skip-extension-check to load it anyway",
                        rom.display(),
                        info.library_name,
                        info.valid_extensions.join(", ")
                    );
                }
                core.init(libretro::Context {
                    system_dir,
                    save_dir: tmp.path().to_path_buf(),
                    options,
                })?;
                let av = core.load_game(rom)?;
                if request.verbose {
                    eprintln!(
                        "core {} ({}x{} at {:.3} fps)",
                        info.library_name, av.base.width, av.base.height, av.fps
                    );
                }

                let state_path = match state {
                    Some(StateSource::File(p)) => Some(p.clone()),
                    Some(StateSource::Slot(n)) => {
                        match dirs.locate(
                            "save state slot",
                            |d| state::slot_path(&d.states, &info.library_name, rom, *n),
                            |p| p.is_file(),
                        ) {
                            Located::Found(p) => Some(p),
                            Located::Missing(tried) => {
                                bail!("slot {n} not found at {}", describe_tried(&tried))
                            }
                        }
                    }
                    None => None,
                };
                let warmup = match state_path {
                    Some(path) => {
                        let bytes = std::fs::read(&path)
                            .with_context(|| format!("reading state {}", path.display()))?;
                        let mem = state::decode(&bytes)
                            .with_context(|| format!("decoding state {}", path.display()))?;
                        core.restore(&mem)
                            .with_context(|| format!("restoring state {}", path.display()))?;
                        request.advance - 1
                    }
                    None => settle_frames(request.settle, av.fps),
                };
                // `tmp` must outlive the core (it is the core's save dir);
                // moving it into the box alongside the core keeps it alive
                // until render::run returns.
                (Box::new(CoreWithTemp { core, _tmp: tmp }), warmup)
            }
        };

        let opts = render::RenderOptions {
            source,
            preset,
            window: request.window.clone(),
            screen,
            warmup,
            frames: request.frames,
            output: request.output.clone(),
            verbose: request.verbose,
        };
        render::run(opts)?;
        match stdout.as_mut() {
            Some(original) => original.print_line(&request.output.display().to_string())?,
            None => println!("{}", request.output.display()),
        }
        Ok(())
    }
}

/// Whether `MTL_CAPTURE_ENABLED` needs setting: Metal offers programmatic
/// capture to a trace document only when it is exactly `1` at load time.
pub fn needs_capture_env(current: Option<&OsStr>) -> bool {
    current != Some(OsStr::new("1"))
}

/// An image the `image` crate, with the features this crate enables, can
/// decode: it must carry one of those extensions and exist.
fn validate_image(image: &Path) -> Result<()> {
    if !render::ImageSource::accepts(image) {
        bail!(
            "{} does not have an image extension this backend decodes ({})",
            image.display(),
            render::ImageSource::EXTENSIONS.join(", ")
        );
    }
    if !image.is_file() {
        bail!("image not found at {}", image.display());
    }
    Ok(())
}

/// A core plus the temp dir it was told to save into.
struct CoreWithTemp {
    core: libretro::Core,
    _tmp: tempfile::TempDir,
}

impl render::FrameSource for CoreWithTemp {
    fn size(&self) -> Size {
        self.core.size()
    }
    fn next(&mut self) -> Result<&[u8]> {
        self.core.next()
    }
}

/// Points file descriptor 1 at descriptor 2 for the rest of the process,
/// keeping a hosted core's stdout chatter off the stream this tool reports
/// the output path on; the path is written through the saved original
/// descriptor instead. Descriptor 1 is deliberately never restored: C
/// stdio buffers a core's output and flushes it at process exit, which
/// would land on the real stdout after any restore.
struct StdoutToStderr {
    original: std::fs::File,
}

impl StdoutToStderr {
    fn redirect() -> Result<StdoutToStderr> {
        use std::io::Write;
        std::io::stdout().flush().context("flushing stdout")?;
        let saved = nix::unistd::dup(std::io::stdout()).context("saving stdout")?;
        nix::unistd::dup2_stdout(std::io::stderr()).context("redirecting stdout to stderr")?;
        Ok(StdoutToStderr {
            original: std::fs::File::from(saved),
        })
    }

    /// Write one line to the original stdout.
    fn print_line(&mut self, line: &str) -> Result<()> {
        use std::io::Write;
        writeln!(self.original, "{line}").context("writing the output path to stdout")?;
        self.original.flush().context("flushing stdout")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowMode;

    fn request(source: Source, shader: Option<&str>) -> Request {
        Request {
            source,
            shader: shader.map(PathBuf::from),
            window: WindowMode::Fullscreen,
            frames: 1,
            settle: 5.0,
            advance: 1,
            output: PathBuf::from("/nonexistent-dir/x.gputrace"),
            config: None,
            verbose: false,
        }
    }

    #[test]
    fn requires_a_shader_before_touching_metal() {
        let err = Hosted
            .run(request(
                Source::Image(PathBuf::from("fixtures/sample.png")),
                None,
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--shader"), "{err}");
    }

    #[test]
    fn refuses_a_zip_before_loading_anything() {
        let err = Hosted
            .run(request(
                Source::Core {
                    core: "/nonexistent/core.dylib".into(),
                    rom: PathBuf::from("/nonexistent/game.zip"),
                    state: None,
                    options: None,
                    skip_extension_check: false,
                },
                Some("Cargo.toml"),
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("zip"), "{err}");
    }

    #[test]
    fn capture_env_reexec_decision() {
        assert!(needs_capture_env(None));
        assert!(needs_capture_env(Some(OsStr::new("0"))));
        assert!(needs_capture_env(Some(OsStr::new(""))));
        assert!(!needs_capture_env(Some(OsStr::new("1"))));
    }

    #[test]
    fn validate_image_checks_extension_then_existence() {
        let err = validate_image(Path::new("Cargo.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("extension"), "{err}");
        let err = validate_image(Path::new("/nonexistent/x.png"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
        validate_image(Path::new("fixtures/sample.png")).unwrap();
    }
}
