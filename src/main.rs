#![deny(unsafe_code)]

use anyhow::{Context, Result, bail};
use clap::Parser;
use ra_metal_capture::config::{self, AppendConfig, PausedConfig, Size, WindowMode};
use ra_metal_capture::image::{EXTENSIONS, is_image_path};
use ra_metal_capture::launch::{LaunchPlan, build_command};
#[cfg(feature = "librashader")]
use ra_metal_capture::libretro;
#[cfg(feature = "librashader")]
use ra_metal_capture::render;
use ra_metal_capture::{app, capture, core, display, remote, state};
#[cfg(feature = "librashader")]
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn default_config() -> PathBuf {
    config::expand_tilde("~/Library/Application Support/RetroArch/config/retroarch.cfg")
}

fn parse_size(s: &str) -> std::result::Result<Size, String> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| format!("expected WxH, got {s:?}"))?;
    let w: u32 = w.parse().map_err(|_| format!("bad width in {s:?}"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad height in {s:?}"))?;
    if w == 0 || h == 0 {
        return Err("width and height must be non-zero".into());
    }
    Ok(Size {
        width: w,
        height: h,
    })
}

fn parse_settle(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!(
            "settle must be a non-negative, finite number of seconds, got {s:?}"
        ));
    }
    Ok(v)
}

/// Which renderer a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Backend {
    /// RetroArch's built-in image viewer, recorded with gpucapture
    Retroarch,
    /// librashader's Metal runtime in this process, recorded with MTLCaptureManager
    Librashader,
}

/// The backend a run uses when `--backend` is not given: the in-process
/// renderer whenever it is compiled in, since it needs no RetroArch.
#[cfg(feature = "librashader")]
const DEFAULT_BACKEND: Backend = Backend::Librashader;
#[cfg(not(feature = "librashader"))]
const DEFAULT_BACKEND: Backend = Backend::Retroarch;

/// Launch RetroArch with a ROM and save state, or a static image, plus a
/// shader preset, then capture frames to a .gputrace with gpucapture; or
/// render a static image or a hosted libretro core through librashader
/// in-process.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// RetroArch .app bundle, or the binary inside it (retroarch backend
    /// only; default /Applications/RetroArch.app)
    #[arg(long)]
    app: Option<PathBuf>,

    /// Path to a libretro .dylib, or a bare name resolved in RetroArch's
    /// cores directory; or use --image instead
    #[arg(
        long,
        required_unless_present = "image",
        conflicts_with = "image",
        requires = "rom"
    )]
    core: Option<String>,

    /// Content file to load: handed to the hosted core (librashader) or
    /// to RetroArch (retroarch); or use --image instead
    #[arg(
        long,
        required_unless_present = "image",
        conflicts_with = "image",
        requires = "core"
    )]
    rom: Option<PathBuf>,

    /// Static image to render through the preset (librashader) or to show
    /// in RetroArch's image viewer (retroarch); replaces --core and --rom
    #[arg(long, conflicts_with_all = ["core", "rom", "state", "slot", "advance"])]
    image: Option<PathBuf>,

    /// Renderer: librashader renders in this process and needs no RetroArch
    /// (the default when compiled in); retroarch launches RetroArch.app
    #[arg(long, value_enum, default_value_t = DEFAULT_BACKEND)]
    backend: Backend,

    /// Save state to load: restored into the hosted core (librashader), or
    /// staged as slot 0 for RetroArch (retroarch)
    #[arg(long, conflicts_with = "slot")]
    state: Option<PathBuf>,

    /// Save state slot N, found where RetroArch keeps it for this ROM
    #[arg(long, conflicts_with = "state")]
    slot: Option<u32>,

    /// Core options file (RetroArch `key = "value"` format) for a hosted
    /// core; without it the core uses its built-in defaults (librashader only)
    #[arg(long, requires = "core", conflicts_with = "image")]
    core_options: Option<PathBuf>,

    /// Load the ROM even if its extension is not one the core declares
    /// (librashader only)
    #[arg(long, requires = "core")]
    skip_extension_check: bool,

    /// Shader preset (.slangp / .glslp); required by the librashader
    /// backend, passed to RetroArch as --set-shader by the other
    #[arg(long)]
    shader: Option<PathBuf>,

    /// retroarch.cfg: read only when a default RetroArch path is missing
    /// (librashader), or as the base config for the run (retroarch)
    #[arg(long, default_value_os_t = default_config())]
    config: PathBuf,

    /// Exact output size in pixels (librashader) or window size in points
    /// (retroarch), e.g. 1600x1440
    #[arg(long, value_parser = parse_size, conflicts_with_all = ["scale", "fullscreen"])]
    size: Option<Size>,

    /// Integer multiple of the source's native size
    #[arg(
        long,
        conflicts_with_all = ["size", "fullscreen"],
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    scale: Option<u32>,

    /// Render at the main display's full pixel size (librashader) or launch
    /// RetroArch fullscreen (retroarch)
    #[arg(long, conflicts_with_all = ["size", "scale"])]
    fullscreen: bool,

    /// Without a save state: emulated seconds to run a hosted core before
    /// recording (librashader), or seconds to wait before capturing (retroarch)
    #[arg(long, default_value_t = 5.0, value_parser = parse_settle, allow_negative_numbers = true)]
    settle: f64,

    /// Frames to run after loading the state; the last one is the first
    /// recorded (librashader) or the one captured (retroarch)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    advance: u32,

    /// UDP port for RetroArch's command interface, enabled only for this
    /// run (retroarch only; default 55355)
    #[arg(long)]
    cmd_port: Option<u16>,

    /// Consecutive emulated frames to record (librashader) or frame
    /// boundaries to capture (retroarch)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    frames: u32,

    /// Output .gputrace path
    #[arg(long)]
    output: PathBuf,

    /// Leave RetroArch running after the capture (retroarch only)
    #[arg(long)]
    keep_running: bool,

    /// Print sizes and the core's geometry (librashader), or the command
    /// line and appendconfig and pass -v to RetroArch (retroarch)
    #[arg(short, long)]
    verbose: bool,
}

/// Flags that only the RetroArch backend honours, as given on this command line.
#[cfg_attr(not(feature = "librashader"), allow(dead_code))]
fn retroarch_only_flags(cli: &Cli) -> Vec<&'static str> {
    let mut given = Vec::new();
    if cli.app.is_some() {
        given.push("--app");
    }
    if cli.cmd_port.is_some() {
        given.push("--cmd-port");
    }
    if cli.keep_running {
        given.push("--keep-running");
    }
    given
}

/// Flags that only the librashader backend honours, as given on this command line.
fn librashader_only_flags(cli: &Cli) -> Vec<&'static str> {
    let mut given = Vec::new();
    if cli.core_options.is_some() {
        given.push("--core-options");
    }
    if cli.skip_extension_check {
        given.push("--skip-extension-check");
    }
    given
}

impl Cli {
    fn window_mode(&self) -> WindowMode {
        if self.fullscreen {
            WindowMode::Fullscreen
        } else if let Some(size) = self.size {
            WindowMode::Exact(size)
        } else if let Some(n) = self.scale {
            WindowMode::Scale(n)
        } else {
            display::fill_mode(display::visible_size())
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(feature = "librashader")]
    if cli.backend == Backend::Librashader {
        reexec_with_capture_env()?;
    }
    run(cli)
}

/// Whether `MTL_CAPTURE_ENABLED` needs setting: Metal offers programmatic
/// capture to a trace document only when it is exactly `1` at load time.
///
/// Only called from `reexec_with_capture_env` (feature-gated); without the
/// "librashader" feature it is exercised solely by its own test.
#[cfg_attr(not(feature = "librashader"), allow(dead_code))]
fn needs_capture_env(current: Option<&OsStr>) -> bool {
    current != Some(OsStr::new("1"))
}

/// Replace this process with itself, same arguments, plus
/// `MTL_CAPTURE_ENABLED=1`, unless that is already set. `exec` only returns
/// on failure. After the re-exec the variable is `1`, so this never loops;
/// if Metal still refuses, `render::run` fails with a message naming it.
#[cfg(feature = "librashader")]
fn reexec_with_capture_env() -> Result<()> {
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

/// `--image` must carry an extension the image viewer accepts and exist.
fn validate_image(image: &Path) -> Result<()> {
    if !is_image_path(image) {
        bail!(
            "{} does not have an image extension the image viewer accepts ({})",
            image.display(),
            EXTENSIONS.join(", ")
        );
    }
    if !image.is_file() {
        bail!("image not found at {}", image.display());
    }
    Ok(())
}

#[cfg(not(feature = "librashader"))]
fn run_librashader(_cli: &Cli) -> Result<()> {
    bail!(
        "this build has no librashader backend; reinstall with the \"librashader\" \
         feature (it is on by default)"
    )
}

#[cfg(feature = "librashader")]
fn run_librashader(cli: &Cli) -> Result<()> {
    let ignored = retroarch_only_flags(cli);
    if !ignored.is_empty() {
        bail!(
            "{} only appl{} to the RetroArch backend; add --backend retroarch or drop {}",
            ignored.join(", "),
            if ignored.len() == 1 { "ies" } else { "y" },
            if ignored.len() == 1 { "it" } else { "them" }
        );
    }
    let preset = cli.shader.clone().context(
        "--backend librashader requires --shader: there is nothing to render without a preset",
    )?;
    if !preset.is_file() {
        bail!("shader preset not found at {}", preset.display());
    }
    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let window = cli.window_mode();
    let screen = display::main_screen();

    let mut stdout: Option<StdoutToStderr> = None;
    let (source, warmup): (Box<dyn render::FrameSource>, u32) = if let Some(image) = &cli.image {
        validate_image(image)?;
        (Box::new(render::ImageSource::open(image)?), 0)
    } else {
        let rom = cli.rom.clone().context("--rom is required with --core")?;
        if rom
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            bail!(
                "{} is a zip; this backend takes the extracted ROM (RetroArch extracts archives itself)",
                rom.display()
            );
        }
        if !rom.is_file() {
            bail!("ROM not found at {}", rom.display());
        }
        let core_arg = cli
            .core
            .as_deref()
            .context("--core is required without --image")?;
        let mut dirs = DirResolver::for_config(&cli.config, cli.verbose);
        let core_path = if Path::new(core_arg).is_file() {
            PathBuf::from(core_arg)
        } else {
            let hit = dirs.locate(
                "core",
                |d| d.libretro_dir.clone(),
                |dir| core::resolve_core(core_arg, dir).is_ok(),
            );
            match hit.found {
                Some(dir) => core::resolve_core(core_arg, &dir)?,
                None => bail!(
                    "core {core_arg} not found in {}",
                    describe_tried(&hit.tried)
                ),
            }
        };
        let options = match &cli.core_options {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading core options {}", path.display()))?;
                config::read_all(&text)
            }
            None => HashMap::new(),
        };
        let system_dir = {
            let hit = dirs.locate("system directory", |d| d.system_dir.clone(), |p| p.is_dir());
            hit.found.unwrap_or_else(|| hit.tried[0].clone())
        };
        let tmp = tempfile::Builder::new()
            .prefix("ra-metal-capture-")
            .tempdir()
            .context("creating temp dir")?;

        // A hosted core may print to stdout; keep that off the stream this
        // tool reports the output path on, for as long as the core lives.
        stdout = Some(StdoutToStderr::redirect()?);
        // Open the core before initialising it: a core may read its options
        // as early as `retro_init`, so they travel in the Context.
        let mut core = libretro::Core::open(&core_path)?;
        let info = core.system_info();
        if !cli.skip_extension_check && !info.accepts_extension(&rom) {
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
        let av = core.load_game(&rom)?;
        if cli.verbose {
            eprintln!(
                "core {} ({}x{} at {:.3} fps)",
                info.library_name, av.base.width, av.base.height, av.fps
            );
        }

        let state_path = match (&cli.state, cli.slot) {
            (Some(p), _) => Some(p.clone()),
            (None, Some(n)) => {
                let hit = dirs.locate(
                    "save state slot",
                    |d| state::slot_path(&d.states, &info.library_name, &rom, n),
                    |p| p.is_file(),
                );
                match hit.found {
                    Some(p) => Some(p),
                    None => bail!("slot {n} not found at {}", describe_tried(&hit.tried)),
                }
            }
            (None, None) => None,
        };
        let warmup = match state_path {
            Some(path) => {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading state {}", path.display()))?;
                let mem = state::decode(&bytes)
                    .with_context(|| format!("decoding state {}", path.display()))?;
                core.restore(&mem)
                    .with_context(|| format!("restoring state {}", path.display()))?;
                cli.advance - 1
            }
            None => settle_frames(cli.settle, av.fps),
        };
        // `tmp` must outlive the core (it is the core's save dir); moving it
        // into the box alongside the core keeps it alive until render::run returns.
        (Box::new(CoreWithTemp { core, _tmp: tmp }), warmup)
    };

    let opts = render::RenderOptions {
        source,
        preset,
        window,
        screen,
        warmup,
        frames: cli.frames,
        output: output.clone(),
        verbose: cli.verbose,
    };
    render::run(opts)?;
    match stdout.as_mut() {
        Some(original) => original.print_line(&output.display().to_string())?,
        None => println!("{}", output.display()),
    }
    Ok(())
}

/// Warm-up frames for `--settle` seconds at the core's own frame rate. A
/// core that reports no frame rate is run at 60 fps, RetroArch's fallback,
/// so the settle is a wait rather than nothing.
#[cfg(feature = "librashader")]
fn settle_frames(settle: f64, fps: f64) -> u32 {
    let fps = if fps > 0.0 { fps } else { 60.0 };
    (settle * fps).round() as u32
}

/// A core plus the temp dir it was told to save into.
#[cfg(feature = "librashader")]
struct CoreWithTemp {
    core: libretro::Core,
    _tmp: tempfile::TempDir,
}

/// Points file descriptor 1 at descriptor 2 for the rest of the process,
/// keeping a hosted core's stdout chatter off the stream this tool reports
/// the output path on; the path is written through the saved original
/// descriptor instead. Descriptor 1 is deliberately never restored: C
/// stdio buffers a core's output and flushes it at process exit, which
/// would land on the real stdout after any restore.
#[cfg(feature = "librashader")]
struct StdoutToStderr {
    original: std::fs::File,
}

#[cfg(feature = "librashader")]
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

#[cfg(feature = "librashader")]
impl render::FrameSource for CoreWithTemp {
    fn size(&self) -> Size {
        self.core.size()
    }
    fn next(&mut self) -> Result<Vec<u8>> {
        self.core.next()
    }
}

/// Where RetroArch keeps the things a hosted core needs. The values come
/// from RetroArch's Darwin platform driver: user documents under
/// `~/Documents/RetroArch`, hidden app data under
/// `~/Library/Application Support/RetroArch`.
#[cfg(feature = "librashader")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct RetroArchDirs {
    libretro_dir: PathBuf,
    system_dir: PathBuf,
    states: state::StateDirs,
}

#[cfg(feature = "librashader")]
impl RetroArchDirs {
    /// RetroArch's macOS defaults, with states sorted into per-core folders.
    fn defaults() -> RetroArchDirs {
        RetroArchDirs {
            libretro_dir: config::expand_tilde("~/Library/Application Support/RetroArch/cores"),
            system_dir: config::expand_tilde("~/Documents/RetroArch/system"),
            states: state::StateDirs {
                savestate_directory: config::expand_tilde("~/Documents/RetroArch/states"),
                sort_by_core: true,
                sort_by_content: false,
                in_content_dir: false,
            },
        }
    }

    /// The directories a `retroarch.cfg` names, with the defaults filling
    /// any key it leaves out.
    fn from_config(text: &str) -> RetroArchDirs {
        let keys = config::read_all(text);
        let base = RetroArchDirs::defaults();
        let dir = |key: &str, default: PathBuf| {
            keys.get(key)
                .map(|s| config::expand_tilde(s))
                .unwrap_or(default)
        };
        let flag = |key: &str, default: bool| keys.get(key).map(|v| v == "true").unwrap_or(default);
        RetroArchDirs {
            libretro_dir: dir("libretro_directory", base.libretro_dir),
            system_dir: dir("system_directory", base.system_dir),
            states: state::StateDirs {
                savestate_directory: dir("savestate_directory", base.states.savestate_directory),
                sort_by_core: flag("sort_savestates_enable", base.states.sort_by_core),
                sort_by_content: flag(
                    "sort_savestates_by_content_enable",
                    base.states.sort_by_content,
                ),
                in_content_dir: flag("savestates_in_content_dir", base.states.in_content_dir),
            },
        }
    }
}

/// What [`DirResolver::locate`] found: the first candidate that exists, and
/// every candidate it tried, defaults first.
#[cfg(feature = "librashader")]
struct Located {
    found: Option<PathBuf>,
    tried: Vec<PathBuf>,
}

/// Resolves paths inferred from RetroArch's layout: the defaults are tried
/// first, and `retroarch.cfg` is parsed (once, lazily) only when a default
/// candidate does not exist. A machine without RetroArch therefore works
/// with the default layout and never needs a config file.
#[cfg(feature = "librashader")]
struct DirResolver<'a> {
    defaults: RetroArchDirs,
    /// Reads the config text, or `None` when there is no config file.
    load_config: Box<dyn FnOnce() -> Option<String> + 'a>,
    /// `None` until first needed; then the parsed config, or `None` if absent.
    config: Option<Option<RetroArchDirs>>,
    verbose: bool,
}

#[cfg(feature = "librashader")]
impl<'a> DirResolver<'a> {
    fn new(
        defaults: RetroArchDirs,
        load_config: impl FnOnce() -> Option<String> + 'a,
        verbose: bool,
    ) -> DirResolver<'a> {
        DirResolver {
            defaults,
            load_config: Box::new(load_config),
            config: None,
            verbose,
        }
    }

    /// The resolver for the `retroarch.cfg` at `path`, absent or not.
    fn for_config(path: &'a Path, verbose: bool) -> DirResolver<'a> {
        DirResolver::new(
            RetroArchDirs::defaults(),
            move || std::fs::read_to_string(path).ok(),
            verbose,
        )
    }

    fn locate(
        &mut self,
        what: &str,
        pick: impl Fn(&RetroArchDirs) -> PathBuf,
        exists: impl Fn(&Path) -> bool,
    ) -> Located {
        let candidate = pick(&self.defaults);
        if exists(&candidate) {
            return Located {
                found: Some(candidate),
                tried: vec![],
            };
        }
        let mut tried = vec![candidate];
        if self.config.is_none() {
            let loader = std::mem::replace(&mut self.load_config, Box::new(|| None));
            self.config = Some(loader().map(|text| RetroArchDirs::from_config(&text)));
        }
        if let Some(Some(cfg)) = &self.config {
            let candidate = pick(cfg);
            if candidate != tried[0] && exists(&candidate) {
                if self.verbose {
                    eprintln!(
                        "{what}: not at the default {}; using {} from retroarch.cfg",
                        tried[0].display(),
                        candidate.display()
                    );
                }
                return Located {
                    found: Some(candidate),
                    tried,
                };
            }
            if candidate != tried[0] {
                tried.push(candidate);
            }
        }
        Located { found: None, tried }
    }
}

#[cfg(feature = "librashader")]
fn describe_tried(tried: &[PathBuf]) -> String {
    tried
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

fn run(cli: Cli) -> Result<()> {
    if cli.backend == Backend::Librashader {
        return run_librashader(&cli);
    }
    let only = librashader_only_flags(&cli);
    if !only.is_empty() {
        bail!(
            "{} only appl{} to the librashader backend",
            only.join(", "),
            if only.len() == 1 { "ies" } else { "y" }
        );
    }
    let app_path = cli
        .app
        .clone()
        .unwrap_or_else(|| PathBuf::from("/Applications/RetroArch.app"));
    let cmd_port = cli.cmd_port.unwrap_or(55355);
    let binary = app::resolve_binary(&app_path)?;

    let cfg_text = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("reading {}", cli.config.display()))?;
    let keys = config::read_keys(&cfg_text, &["libretro_directory"]);
    let libretro_dir = keys
        .get("libretro_directory")
        .map(|s| config::expand_tilde(s))
        .unwrap_or_else(|| config::expand_tilde("~/Library/Application Support/RetroArch/cores"));

    let (core, content) = if let Some(image) = &cli.image {
        validate_image(image)?;
        (None, image.clone())
    } else {
        let core_arg = cli
            .core
            .as_deref()
            .context("--core is required without --image")?;
        let rom = cli
            .rom
            .clone()
            .context("--rom is required without --image")?;
        let core = core::resolve_core(core_arg, &libretro_dir)?;
        if !rom.is_file() {
            bail!("ROM not found at {}", rom.display());
        }
        (Some(core), rom)
    };

    if let Some(shader) = &cli.shader
        && !shader.is_file()
    {
        bail!("shader preset not found at {}", shader.display());
    }

    if cli.state.is_some() || cli.slot.is_some() {
        // Guard against the appendconfig's network_cmd_port already
        // belonging to somebody else's RetroArch before we launch ours.
        remote::probe_free(cmd_port)?;
    }

    let tmp = tempfile::Builder::new()
        .prefix("ra-metal-capture-")
        .tempdir()
        .context("creating temp dir")?;

    let (paused, staged_states_dir) = match (&cli.state, cli.slot) {
        (Some(state_file), _) => {
            let dir = tmp.path().join("states");
            let slot = state::stage(state_file, &content, &dir)?;
            (
                Some(PausedConfig {
                    port: cmd_port,
                    slot,
                }),
                Some(dir),
            )
        }
        (None, Some(slot)) => (
            Some(PausedConfig {
                port: cmd_port,
                slot,
            }),
            None,
        ),
        (None, None) => (None, None),
    };

    let append = AppendConfig {
        window: cli.window_mode(),
        staged_states_dir,
        paused,
        image_viewer: cli.image.is_some(),
    };
    let appendconfig = tmp.path().join("append.cfg");
    std::fs::write(&appendconfig, append.render())
        .with_context(|| format!("writing {}", appendconfig.display()))?;

    let plan = LaunchPlan {
        binary,
        core,
        content,
        shader: cli.shader.clone(),
        appendconfig,
        fullscreen: cli.fullscreen,
        verbose: cli.verbose,
    };
    let cmd = build_command(&plan);

    if cli.verbose {
        eprintln!("appendconfig:\n{}", append.render());
        eprintln!("command: {}", cmd.display());
    }

    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let trigger = match paused {
        Some(p) => capture::Trigger::Paused {
            port: p.port,
            advance: cli.advance,
        },
        None => capture::Trigger::Settle(Duration::from_secs_f64(cli.settle)),
    };
    let opts = capture::CaptureOptions {
        trigger,
        frames: cli.frames,
        output: output.clone(),
        keep_running: cli.keep_running,
        ready_timeout: Duration::from_secs(30),
        log_path: tmp.path().join("retroarch.log"),
    };
    capture::run(&cmd, &opts)?;

    if cli.keep_running {
        let kept = tmp.keep();
        eprintln!("kept {} for the running RetroArch", kept.display());
    }

    println!("{}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_rom(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec![
            "ra-metal-capture",
            "--core",
            "c",
            "--rom",
            "r",
            "--output",
            "o",
        ];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    fn parse_raw(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec!["ra-metal-capture", "--output", "o"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn minimal_args_parse_with_defaults() {
        let cli = parse_rom(&[]).unwrap();
        assert!(cli.app.is_none());
        assert_eq!(cli.settle, 5.0);
        assert_eq!(cli.frames, 1);
        assert!(cli.state.is_none() && cli.slot.is_none());
    }

    #[test]
    fn state_and_slot_conflict() {
        assert!(parse_rom(&["--state", "s", "--slot", "1"]).is_err());
    }

    #[test]
    fn window_modes_conflict_pairwise() {
        assert!(parse_rom(&["--size", "1x1", "--scale", "2"]).is_err());
        assert!(parse_rom(&["--size", "1x1", "--fullscreen"]).is_err());
        assert!(parse_rom(&["--scale", "2", "--fullscreen"]).is_err());
    }

    #[test]
    fn size_parses_and_maps_to_window_mode() {
        let cli = parse_rom(&["--size", "1600x1440"]).unwrap();
        assert_eq!(
            cli.window_mode(),
            WindowMode::Exact(Size {
                width: 1600,
                height: 1440
            })
        );
        assert!(parse_rom(&["--size", "1600"]).is_err());
        assert!(parse_rom(&["--size", "0x10"]).is_err());
    }

    #[test]
    fn scale_and_fullscreen_map_to_window_modes() {
        assert_eq!(
            parse_rom(&["--scale", "4"]).unwrap().window_mode(),
            WindowMode::Scale(4)
        );
        assert_eq!(
            parse_rom(&["--fullscreen"]).unwrap().window_mode(),
            WindowMode::Fullscreen
        );
    }

    #[test]
    fn scale_rejects_zero() {
        assert!(parse_rom(&["--scale", "0"]).is_err());
        assert_eq!(parse_rom(&["--scale", "1"]).unwrap().scale, Some(1));
    }

    #[test]
    fn settle_rejects_negative_and_nan() {
        assert!(parse_rom(&["--settle=-1"]).is_err());
        let err = parse_rom(&["--settle", "-3"]).unwrap_err().to_string();
        assert!(err.contains("non-negative"), "{err}");
        assert!(parse_rom(&["--settle=nan"]).is_err());
        assert_eq!(parse_rom(&["--settle", "2.5"]).unwrap().settle, 2.5);
    }

    #[test]
    fn frames_rejects_zero() {
        assert!(parse_rom(&["--frames", "0"]).is_err());
        assert_eq!(parse_rom(&["--frames", "1"]).unwrap().frames, 1);
    }

    #[test]
    fn advance_and_cmd_port_defaults_and_validation() {
        let cli = parse_rom(&[]).unwrap();
        assert_eq!(cli.advance, 1);
        assert!(cli.cmd_port.is_none());
        assert!(parse_rom(&["--advance", "0"]).is_err());
        assert_eq!(parse_rom(&["--advance", "12"]).unwrap().advance, 12);
        assert_eq!(
            parse_rom(&["--cmd-port", "60000"]).unwrap().cmd_port,
            Some(60000)
        );
    }

    #[test]
    fn image_mode_parses_alone_and_conflicts_with_emulator_flags() {
        let cli = parse_raw(&["--image", "sample.png"]).unwrap();
        assert_eq!(cli.image, Some(PathBuf::from("sample.png")));
        assert!(cli.core.is_none() && cli.rom.is_none());
        assert!(parse_raw(&["--image", "s.png", "--core", "c"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--rom", "r"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--state", "x"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--slot", "1"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--advance", "2"]).is_err());
    }

    #[test]
    fn core_and_rom_are_required_together_without_image() {
        assert!(parse_raw(&[]).is_err());
        assert!(parse_raw(&["--core", "c"]).is_err());
        assert!(parse_raw(&["--rom", "r"]).is_err());
        assert!(parse_raw(&["--core", "c", "--rom", "r"]).is_ok());
    }

    #[test]
    fn backend_defaults_to_librashader_when_compiled_in() {
        assert_eq!(parse_rom(&[]).unwrap().backend, DEFAULT_BACKEND);
        #[cfg(feature = "librashader")]
        assert_eq!(DEFAULT_BACKEND, Backend::Librashader);
        #[cfg(not(feature = "librashader"))]
        assert_eq!(DEFAULT_BACKEND, Backend::Retroarch);
        assert_eq!(
            parse_rom(&["--backend", "retroarch"]).unwrap().backend,
            Backend::Retroarch
        );
        let cli = parse_raw(&[
            "--image",
            "s.png",
            "--shader",
            "p.slangp",
            "--backend",
            "librashader",
        ])
        .unwrap();
        assert_eq!(cli.backend, Backend::Librashader);
        assert!(parse_raw(&["--image", "s.png", "--backend", "bogus"]).is_err());
    }

    #[test]
    fn librashader_accepts_core_and_rom_without_image() {
        let cli = parse_raw(&[
            "--backend",
            "librashader",
            "--core",
            "c",
            "--rom",
            "r",
            "--shader",
            "p",
        ])
        .unwrap();
        assert_eq!(cli.backend, Backend::Librashader);
        assert!(cli.image.is_none());
        assert!(
            parse_raw(&[
                "--backend",
                "librashader",
                "--core",
                "c",
                "--rom",
                "r",
                "--image",
                "i"
            ])
            .is_err()
        );
        assert!(
            parse_raw(&[
                "--backend",
                "librashader",
                "--core",
                "c",
                "--rom",
                "r",
                "--state",
                "s",
                "--slot",
                "1"
            ])
            .is_err()
        );
        assert!(
            parse_raw(&["--backend", "librashader"]).is_err(),
            "needs image or core+rom"
        );
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_core_path_refuses_a_zip_before_loading_anything() {
        let cli = parse_raw(&[
            "--backend",
            "librashader",
            "--core",
            "/nonexistent/core.dylib",
            "--rom",
            "/nonexistent/game.zip",
            "--shader",
            "Cargo.toml",
        ])
        .unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("zip"), "{err}");
    }

    #[test]
    fn backend_only_flags_are_detected() {
        let cli = parse_rom(&["--app", "/x", "--cmd-port", "1", "--keep-running"]).unwrap();
        assert_eq!(
            retroarch_only_flags(&cli),
            ["--app", "--cmd-port", "--keep-running"]
        );
        assert!(librashader_only_flags(&cli).is_empty());
        let cli = parse_rom(&["--core-options", "o", "--skip-extension-check"]).unwrap();
        assert_eq!(
            librashader_only_flags(&cli),
            ["--core-options", "--skip-extension-check"]
        );
        assert!(retroarch_only_flags(&cli).is_empty());
    }

    #[test]
    fn retroarch_path_rejects_librashader_only_flags_before_anything_else() {
        let cli = parse_rom(&["--backend", "retroarch", "--core-options", "o.opt"]).unwrap();
        let err = run(cli).unwrap_err().to_string();
        assert!(err.contains("--core-options"), "{err}");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_path_rejects_retroarch_only_flags_before_anything_else() {
        let cli = parse_raw(&["--image", "sample.png", "--keep-running", "--app", "/x"]).unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("--app, --keep-running"), "{err}");
        assert!(err.contains("--backend retroarch"), "{err}");
    }

    #[test]
    fn core_options_requires_core() {
        assert!(parse_raw(&["--image", "s.png", "--core-options", "o.opt"]).is_err());
        let cli = parse_raw(&["--core", "c", "--rom", "r", "--core-options", "o.opt"]).unwrap();
        assert_eq!(cli.core_options, Some(PathBuf::from("o.opt")));
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn retroarch_dirs_from_config_overrides_only_named_keys() {
        let text =
            "savestate_directory = \"/elsewhere/states\"\nsort_savestates_enable = \"false\"\n";
        let d = RetroArchDirs::from_config(text);
        let base = RetroArchDirs::defaults();
        assert_eq!(d.libretro_dir, base.libretro_dir);
        assert_eq!(d.system_dir, base.system_dir);
        assert_eq!(
            d.states.savestate_directory,
            PathBuf::from("/elsewhere/states")
        );
        assert!(!d.states.sort_by_core);
        assert!(!d.states.sort_by_content);
        assert!(!d.states.in_content_dir);
        assert!(base.states.sort_by_core);
    }

    #[cfg(feature = "librashader")]
    fn fake_dirs(system: &str) -> RetroArchDirs {
        RetroArchDirs {
            libretro_dir: PathBuf::from("/d/cores"),
            system_dir: PathBuf::from(system),
            states: RetroArchDirs::defaults().states,
        }
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn resolver_takes_the_default_without_reading_the_config() {
        use std::cell::Cell;
        let loaded = Cell::new(false);
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || {
                loaded.set(true);
                Some("system_directory = \"/c/system\"".into())
            },
            false,
        );
        let hit = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/d/system"),
        );
        assert_eq!(hit.found.as_deref(), Some(Path::new("/d/system")));
        assert!(hit.tried.is_empty());
        assert!(
            !loaded.get(),
            "config must not be read when the default exists"
        );
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn resolver_falls_back_to_the_config_and_reads_it_once() {
        use std::cell::Cell;
        let loads = Cell::new(0);
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || {
                loads.set(loads.get() + 1);
                Some("system_directory = \"/c/system\"".into())
            },
            false,
        );
        let hit = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/c/system"),
        );
        assert_eq!(hit.found.as_deref(), Some(Path::new("/c/system")));
        assert_eq!(hit.tried, vec![PathBuf::from("/d/system")]);
        let again = r.locate(
            "system",
            |d| d.system_dir.clone(),
            |p| p == Path::new("/c/system"),
        );
        assert_eq!(again.found.as_deref(), Some(Path::new("/c/system")));
        assert_eq!(loads.get(), 1, "config parsed once");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn resolver_reports_every_candidate_when_nothing_exists() {
        let mut r = DirResolver::new(
            fake_dirs("/d/system"),
            || Some("system_directory = \"/c/system\"".into()),
            false,
        );
        let hit = r.locate("system", |d| d.system_dir.clone(), |_| false);
        assert!(hit.found.is_none());
        assert_eq!(
            hit.tried,
            vec![PathBuf::from("/d/system"), PathBuf::from("/c/system")]
        );
        assert_eq!(describe_tried(&hit.tried), "/d/system or /c/system");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn resolver_without_a_config_file_tries_the_default_only() {
        let mut r = DirResolver::new(fake_dirs("/d/system"), || None, false);
        let hit = r.locate("system", |d| d.system_dir.clone(), |_| false);
        assert!(hit.found.is_none());
        assert_eq!(hit.tried, vec![PathBuf::from("/d/system")]);
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn settle_frames_uses_sixty_fps_when_the_core_reports_none() {
        assert_eq!(settle_frames(5.0, 59.728), 299);
        assert_eq!(settle_frames(5.0, 0.0), 300);
        assert_eq!(settle_frames(0.0, 59.728), 0);
    }

    #[test]
    fn capture_env_reexec_decision() {
        use std::ffi::OsStr;
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
        validate_image(Path::new("sample.png")).unwrap();
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_backend_requires_a_shader_before_touching_metal() {
        let cli = parse_raw(&["--image", "sample.png", "--backend", "librashader"]).unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("--shader"), "{err}");
    }

    #[cfg(not(feature = "librashader"))]
    #[test]
    fn librashader_backend_is_refused_without_the_feature() {
        let cli = parse_raw(&["--image", "sample.png", "--backend", "librashader"]).unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("librashader"), "{err}");
    }
}
