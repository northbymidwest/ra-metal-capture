#![deny(unsafe_code)]

use anyhow::{Context, Result, bail};
use clap::Parser;
use ra_metal_capture::config::{self, AppendConfig, PausedConfig, Size, WindowMode};
use ra_metal_capture::image::{EXTENSIONS, is_image_path};
use ra_metal_capture::launch::{LaunchPlan, build_command};
#[cfg(feature = "librashader")]
use ra_metal_capture::render;
use ra_metal_capture::{app, capture, core, display, remote, state};
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

/// Which renderer image mode uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Backend {
    /// RetroArch's built-in image viewer, recorded with gpucapture
    Retroarch,
    /// librashader's Metal runtime in this process, recorded with MTLCaptureManager
    Librashader,
}

/// Launch RetroArch with a ROM and save state, or a static image, plus a
/// shader preset, then capture frames to a .gputrace with gpucapture; or
/// render a static image through librashader in-process.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// RetroArch .app bundle, or the binary inside it
    #[arg(long, default_value = "/Applications/RetroArch.app")]
    app: PathBuf,

    /// Path to a libretro .dylib, or a bare name resolved in libretro_directory
    #[arg(
        long,
        required_unless_present = "image",
        conflicts_with = "image",
        requires = "rom"
    )]
    core: Option<String>,

    /// Content file to load
    #[arg(
        long,
        required_unless_present = "image",
        conflicts_with = "image",
        requires = "core"
    )]
    rom: Option<PathBuf>,

    /// Static image to capture via RetroArch's image viewer; replaces --core and --rom
    #[arg(long, conflicts_with_all = ["core", "rom", "state", "slot", "advance"])]
    image: Option<PathBuf>,

    /// Renderer for --image: retroarch (default) or librashader
    #[arg(long, value_enum, default_value_t = Backend::Retroarch, requires = "image", conflicts_with_all = ["core", "rom", "state", "slot", "advance"])]
    backend: Backend,

    /// Save state file to load at launch (staged as slot 0 in a temp dir)
    #[arg(long, conflicts_with = "slot")]
    state: Option<PathBuf>,

    /// Slot to load from the configured savestate directory
    #[arg(long, conflicts_with = "state")]
    slot: Option<u32>,

    /// Shader preset (.slangp / .glslp)
    #[arg(long)]
    shader: Option<PathBuf>,

    /// retroarch.cfg to base the run on
    #[arg(long, default_value_os_t = default_config())]
    config: PathBuf,

    /// Exact window size in points, e.g. 1600x1440
    #[arg(long, value_parser = parse_size, conflicts_with_all = ["scale", "fullscreen"])]
    size: Option<Size>,

    /// Integer scale of the core's native resolution
    #[arg(
        long,
        conflicts_with_all = ["size", "fullscreen"],
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    scale: Option<u32>,

    /// Launch fullscreen (-f)
    #[arg(long, conflicts_with_all = ["size", "scale"])]
    fullscreen: bool,

    /// Seconds to wait before capturing when no state is given
    #[arg(long, default_value_t = 5.0, value_parser = parse_settle)]
    settle: f64,

    /// Frame advances after loading the state, before the capture is armed
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    advance: u32,

    /// UDP port for RetroArch's command interface (enabled only for this run)
    #[arg(long, default_value_t = 55355)]
    cmd_port: u16,

    /// Number of frame boundaries to capture
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    frames: u32,

    /// Output .gputrace path
    #[arg(long)]
    output: PathBuf,

    /// Leave RetroArch running after the capture
    #[arg(long)]
    keep_running: bool,

    /// Print the command line and appendconfig, and pass -v to RetroArch
    #[arg(short, long)]
    verbose: bool,
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
    let image = cli.image.as_deref().context("--backend requires --image")?;
    validate_image(image)?;
    let preset = cli.shader.clone().context(
        "--backend librashader requires --shader: there is nothing to render without a preset",
    )?;
    if !preset.is_file() {
        bail!("shader preset not found at {}", preset.display());
    }
    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let opts = render::RenderOptions {
        image: image.to_path_buf(),
        preset,
        window: cli.window_mode(),
        screen: display::main_screen(),
        frames: cli.frames,
        output: output.clone(),
        verbose: cli.verbose,
    };
    render::run(&opts)?;
    println!("{}", output.display());
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    if cli.backend == Backend::Librashader {
        return run_librashader(&cli);
    }
    let binary = app::resolve_binary(&cli.app)?;

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
        remote::probe_free(cli.cmd_port)?;
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
                    port: cli.cmd_port,
                    slot,
                }),
                Some(dir),
            )
        }
        (None, Some(slot)) => (
            Some(PausedConfig {
                port: cli.cmd_port,
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
        assert_eq!(cli.app, PathBuf::from("/Applications/RetroArch.app"));
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
        assert_eq!(cli.cmd_port, 55355);
        assert!(parse_rom(&["--advance", "0"]).is_err());
        assert_eq!(parse_rom(&["--advance", "12"]).unwrap().advance, 12);
        assert_eq!(parse_rom(&["--cmd-port", "60000"]).unwrap().cmd_port, 60000);
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
    fn backend_defaults_to_retroarch_and_requires_image() {
        assert_eq!(parse_rom(&[]).unwrap().backend, Backend::Retroarch);
        assert!(parse_rom(&["--backend", "librashader"]).is_err());
        assert!(parse_rom(&["--backend", "retroarch"]).is_err());
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
