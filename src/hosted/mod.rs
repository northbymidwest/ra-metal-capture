//! The hosted backend: render the request's image, or a libretro core run
//! in this process, through the preset with librashader's Metal runtime,
//! and write the trace with Metal's capture API. No RetroArch process.

pub mod libretro;
pub mod render;

use crate::backend::{Backend, Request, Source, StateSource};
use crate::config::{self, Size};
use crate::layout::{DirResolver, Located, describe_tried, settle_frames};
use crate::{bundle, display, image_file, interrupt, state};
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
        let Request {
            source,
            shader: preset,
            window,
            aspect,
            frames,
            settle,
            advance,
            output,
            config,
            verbose,
        } = request;
        if !preset.is_file() {
            bail!("shader preset not found at {}", preset.display());
        }
        // Refuse a bad output path before loading anything.
        bundle::prepare_output(&output)?;
        interrupt::install();
        let screen = display::main_screen();

        let mut stdout: Option<StdoutToStderr> = None;
        let (source, warmup): (Box<dyn render::FrameSource>, u32) = match source {
            Source::Image(image) => {
                validate_image(&image)?;
                (Box::new(render::ImageSource::open(&image)?), 0)
            }
            Source::Core {
                core,
                rom,
                state,
                options,
                skip_extension_check,
            } => {
                libretro::refuse_zip(&rom)?;
                if !rom.is_file() {
                    bail!("ROM not found at {}", rom.display());
                }
                let mut dirs = DirResolver::for_config(config.as_deref(), verbose);
                let run = CoreRun {
                    core_path: dirs.core(&core)?,
                    rom,
                    state,
                    options: read_options(options.as_deref())?,
                    skip_extension_check,
                    system_dir: dirs.system_dir(),
                    settle,
                    advance,
                    verbose,
                };
                // A hosted core may print to stdout; keep that off the stream
                // this tool reports the output path on.
                stdout = Some(StdoutToStderr::redirect()?);
                let (core, warmup) = boot_core(run, &mut dirs)?;
                (Box::new(core), warmup)
            }
        };

        render::run(render::RenderOptions {
            source,
            preset,
            window,
            aspect,
            screen,
            warmup,
            frames,
            output: output.clone(),
            verbose,
        })?;
        match stdout.as_mut() {
            Some(original) => original.print_line(&output.display().to_string())?,
            None => println!("{}", output.display()),
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
    image_file::validate(image, &render::ImageSource::EXTENSIONS, "this backend")
}

/// The `--core-options` file as a map, or no options at all.
fn read_options(path: Option<&Path>) -> Result<HashMap<String, String>> {
    match path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading core options {}", path.display()))?;
            Ok(config::read_all(&text))
        }
        None => Ok(HashMap::new()),
    }
}

/// A core run once its request has been resolved against RetroArch's
/// layout: the core to load, what to feed it, and how long to run it.
struct CoreRun {
    core_path: PathBuf,
    rom: PathBuf,
    state: Option<StateSource>,
    options: HashMap<String, String>,
    skip_extension_check: bool,
    system_dir: PathBuf,
    settle: f64,
    advance: u32,
    verbose: bool,
}

/// Open the core, check the ROM against it, find and decode the state,
/// then boot: `init`, `load_game`, `restore`. Everything that can be
/// refused is refused before `init`, so a bad slot or extension fails
/// before the core has printed a line. Returns the running core and the
/// number of warm-up frames to run before recording.
fn boot_core(run: CoreRun, dirs: &mut DirResolver) -> Result<(CoreWithTemp, u32)> {
    // Open the core before initialising it: a core may read its options
    // as early as `retro_init`, so they travel in the Context.
    let mut core = libretro::Core::open(&run.core_path)?;
    let info = core.system_info();
    if !run.skip_extension_check && !info.accepts_extension(&run.rom) {
        bail!(
            "{} does not have an extension {} accepts ({}); pass --skip-extension-check to load it anyway",
            run.rom.display(),
            info.library_name,
            info.valid_extensions.join(", ")
        );
    }
    let state_path = match &run.state {
        Some(StateSource::File(p)) => Some(p.clone()),
        Some(StateSource::Slot(n)) => match dirs.locate(
            "save state slot",
            |d| state::slot_path(&d.states, &info.library_name, &run.rom, *n),
            |p| p.is_file(),
        ) {
            Located::Found(p) => Some(p),
            Located::Missing(tried) => {
                bail!("slot {n} not found at {}", describe_tried(&tried))
            }
        },
        None => None,
    };
    let mem = match &state_path {
        Some(path) => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading state {}", path.display()))?;
            Some(
                state::decode(&bytes)
                    .with_context(|| format!("decoding state {}", path.display()))?,
            )
        }
        None => None,
    };

    let tmp = tempfile::Builder::new()
        .prefix("ra-metal-capture-")
        .tempdir()
        .context("creating temp dir")?;
    core.init(libretro::Context {
        system_dir: run.system_dir,
        save_dir: tmp.path().to_path_buf(),
        options: run.options,
    })?;
    let av = core.load_game(&run.rom, &info)?;
    if run.verbose {
        eprintln!(
            "core {} ({}x{}, aspect {}, at {:.3} fps)",
            info.library_name,
            av.base.width,
            av.base.height,
            if av.aspect_ratio > 0.0 {
                format!("{:.4}", av.aspect_ratio)
            } else {
                "not reported".to_string()
            },
            av.fps
        );
    }
    let warmup = match (mem, state_path) {
        (Some(mem), Some(path)) => {
            core.restore(&mem)
                .with_context(|| format!("restoring state {}", path.display()))?;
            run.advance.saturating_sub(1)
        }
        _ => settle_frames(run.settle, av.fps),
    };
    // `tmp` must outlive the core (it is the core's save dir); it travels
    // with the core so it lives until render::run returns.
    Ok((CoreWithTemp { core, _tmp: tmp }, warmup))
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
    fn aspect_ratio(&self) -> f64 {
        self.core.aspect_ratio()
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

    fn request(source: Source, shader: &str) -> Request {
        Request {
            source,
            shader: PathBuf::from(shader),
            window: WindowMode::Fullscreen,
            aspect: config::Aspect::Native,
            frames: 1,
            settle: 5.0,
            advance: 1,
            output: PathBuf::from("/tmp/x.gputrace"),
            config: None,
            verbose: false,
        }
    }

    #[test]
    fn refuses_a_missing_shader_before_touching_metal() {
        let err = Hosted
            .run(request(
                Source::Image(PathBuf::from("fixtures/sample.png")),
                "/nonexistent/p.slangp",
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("shader preset not found"), "{err}");
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
                "Cargo.toml",
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
    fn hosted_image_list_is_the_image_crates_not_retroarchs() {
        assert!(
            validate_image(Path::new("Cargo.toml"))
                .unwrap_err()
                .to_string()
                .contains("this backend")
        );
        assert!(render::ImageSource::accepts(Path::new("x.pam")));
        assert!(!render::ImageSource::accepts(Path::new("x.psd")));
        validate_image(Path::new("fixtures/sample.png")).unwrap();
    }

    #[test]
    fn read_options_is_empty_without_a_file_and_fails_on_a_missing_one() {
        assert!(read_options(None).unwrap().is_empty());
        let err = read_options(Some(Path::new("/nonexistent/x.opt")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("core options"), "{err}");
    }
}
