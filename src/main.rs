//! The command line: parse arguments, turn them into a backend-neutral
//! [`Request`], pick a [`Backend`], and run it. Everything about how a
//! capture is actually made lives in the backends.
#![deny(unsafe_code)]

use anyhow::{Context, Result, bail};
use clap::Parser;
use ra_metal_capture::backend::{Backend, Interrupted, Request, Source, StateSource};
use ra_metal_capture::config::{self, Aspect, Size, WindowMode};
use ra_metal_capture::display;
use ra_metal_capture::preset::Param;
use ra_metal_capture::retroarch::{DEFAULT_APP, DEFAULT_CMD_PORT, RetroArch};
use std::path::PathBuf;

/// The most `--settle` accepts, in seconds. Above this the value is a
/// mistake, not a wait: a hosted core would run a quarter million frames.
const MAX_SETTLE: f64 = 3600.0;

fn parse_settle(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!(
            "settle must be a non-negative, finite number of seconds, got {s:?}"
        ));
    }
    if v > MAX_SETTLE {
        return Err(format!(
            "settle must be at most {MAX_SETTLE} seconds, got {s:?}"
        ));
    }
    Ok(v)
}

/// Which backend a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum BackendChoice {
    /// Renders a static image or a hosted libretro core through librashader
    /// in this process, recorded with MTLCaptureManager
    #[cfg(feature = "librashader")]
    Librashader,
    /// Launches RetroArch.app and records its presented frames with gpucapture
    Retroarch,
}

/// The backend a run uses when `--backend` is not given: the in-process
/// renderer whenever it is compiled in, since it needs no RetroArch.
#[cfg(feature = "librashader")]
const DEFAULT_BACKEND: BackendChoice = BackendChoice::Librashader;
#[cfg(not(feature = "librashader"))]
const DEFAULT_BACKEND: BackendChoice = BackendChoice::Retroarch;

/// Render a static image or a hosted libretro core through a shader preset
/// in this process and record a .gputrace; or, with --backend retroarch,
/// launch RetroArch with a ROM and save state, or a static image, and
/// capture its frames with gpucapture.
#[derive(Parser, Debug)]
#[command(
    version,
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Sub>,

    /// Shader preset (.slangp / .glslp) to render through
    #[arg(long, required = true)]
    shader: Option<PathBuf>,

    /// Output .gputrace path
    #[arg(long, required = true)]
    output: Option<PathBuf>,

    /// Replace a .gputrace bundle already at --output instead of refusing
    #[arg(long)]
    overwrite: bool,

    /// Load the ROM even if its extension is not one the core declares
    #[cfg(feature = "librashader")]
    #[arg(long, requires = "core", help_heading = "librashader backend")]
    skip_extension_check: bool,

    // An Option, not a default_value: "not given" is what lets the
    // librashader backend refuse a RetroArch-only flag (see `build`).
    #[arg(
        long,
        help_heading = "RetroArch backend",
        help = format!("RetroArch .app bundle, or the binary inside it [default: {DEFAULT_APP}]")
    )]
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
    backend: BackendChoice,

    /// Save state to load: restored into the hosted core (librashader), or
    /// staged as slot 0 for RetroArch (retroarch)
    #[arg(long, conflicts_with = "slot")]
    state: Option<PathBuf>,

    /// Save state slot N, found where RetroArch keeps it for this ROM
    #[arg(long, conflicts_with = "state")]
    slot: Option<u32>,

    /// Core options file (RetroArch `key = "value"` format); without it
    /// the core uses its built-in defaults under either backend
    #[arg(long, requires = "core", conflicts_with = "image")]
    core_options: Option<PathBuf>,

    /// retroarch.cfg, consulted only when a bare core name, --slot, or the
    /// system directory is not at RetroArch's default location; nothing
    /// else in it is used by either backend
    #[arg(
        long,
        default_value = "~/Library/Application Support/RetroArch/config/retroarch.cfg"
    )]
    config: PathBuf,

    /// Exact output size in pixels (librashader) or window size in points
    /// (retroarch), e.g. 1600x1440
    #[arg(long, value_parser = clap::value_parser!(Size), conflicts_with_all = ["scale", "fullscreen"])]
    size: Option<Size>,

    /// Integer multiple of the source's native size at its aspect, in
    /// points, like RetroArch's window scale
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

    /// Viewport aspect: native (the core's own, or the image's pixels), a
    /// ratio like 4:3, or a number like 1.3333
    #[arg(long, default_value = "native")]
    aspect: Aspect,

    /// Override a preset parameter (repeatable): the run renders a wrapper
    /// preset that references --shader with these values pinned. An
    /// unknown name is silently ignored by either backend
    #[arg(long, value_name = "NAME=VALUE")]
    param: Vec<Param>,

    /// Without a save state: emulated seconds to run a hosted core before
    /// recording (librashader), or seconds to wait before capturing (retroarch)
    #[arg(long, default_value_t = 5.0, value_parser = parse_settle, allow_negative_numbers = true)]
    settle: f64,

    /// Frames to run after loading the state. librashader: the last one is
    /// the first recorded. retroarch: the capture is armed after the last
    /// one and closes a few advances later (the tool prints how many).
    /// Ignored without --state or --slot
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    advance: u32,

    #[arg(
        long,
        help_heading = "RetroArch backend",
        help = format!(
            "UDP port for RetroArch's command interface, enabled only for this run [default: {DEFAULT_CMD_PORT}]"
        )
    )]
    cmd_port: Option<u16>,

    /// Consecutive emulated frames to record (librashader) or frame
    /// boundaries to capture (retroarch)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    frames: u32,

    /// Leave RetroArch running after the capture
    #[arg(long, help_heading = "RetroArch backend")]
    keep_running: bool,

    /// Print sizes and the core's geometry (librashader), or the command
    /// line and run config and pass -v to RetroArch (retroarch)
    #[arg(short, long)]
    verbose: bool,
}

/// Commands other than a capture. Each stands alone: the capture flags are
/// refused alongside one, and the capture's required flags are waived,
/// which is why `shader` and `output` above are `Option`s clap requires.
#[derive(clap::Subcommand, Debug, PartialEq, Eq)]
enum Sub {
    /// Re-sign a RetroArch.app ad hoc with the get-task-allow entitlement,
    /// which gpucapture needs to attach to it; the RetroArch backend cannot
    /// capture an app without it. Repeat after a RetroArch update
    Entitle {
        /// RetroArch .app bundle, or the binary inside it
        #[arg(long, default_value = DEFAULT_APP)]
        app: PathBuf,
        /// Re-sign without asking; otherwise a [Y/n] prompt precedes it
        #[arg(short, long)]
        yes: bool,
    },
}

/// Flags that only the RetroArch backend honours, as given on this command
/// line; consulted by the librashader backend to refuse them.
#[cfg(feature = "librashader")]
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
#[cfg(feature = "librashader")]
fn librashader_only_flags(cli: &Cli) -> Vec<&'static str> {
    let mut given = Vec::new();
    if cli.skip_extension_check {
        given.push("--skip-extension-check");
    }
    given
}

/// A build without the librashader backend has none of its flags.
#[cfg(not(feature = "librashader"))]
fn librashader_only_flags(_cli: &Cli) -> Vec<&'static str> {
    Vec::new()
}

#[cfg(feature = "librashader")]
fn skip_extension_check(cli: &Cli) -> bool {
    cli.skip_extension_check
}

#[cfg(not(feature = "librashader"))]
fn skip_extension_check(_cli: &Cli) -> bool {
    false
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
            // The whole visible area; the RetroArch backend trims its own
            // title bar allowance off this.
            WindowMode::Fill {
                max: display::visible_size(),
            }
        }
    }
}

/// The backend the arguments select, and the request they describe.
/// Flags meant for the other backend are refused here, naming them, so a
/// user who drops `--backend` does not get a silently different run.
fn build(cli: Cli) -> Result<(Box<dyn Backend>, Request)> {
    let backend: Box<dyn Backend> = match cli.backend {
        BackendChoice::Retroarch => {
            let only = librashader_only_flags(&cli);
            if !only.is_empty() {
                bail!(
                    "{} only appl{} to the librashader backend",
                    only.join(", "),
                    if only.len() == 1 { "ies" } else { "y" }
                );
            }
            let defaults = RetroArch::default();
            Box::new(RetroArch {
                app: cli.app.clone().unwrap_or(defaults.app),
                cmd_port: cli.cmd_port.unwrap_or(defaults.cmd_port),
                keep_running: cli.keep_running,
            })
        }
        #[cfg(feature = "librashader")]
        BackendChoice::Librashader => {
            let ignored = retroarch_only_flags(&cli);
            if !ignored.is_empty() {
                bail!(
                    "{} only appl{} to the RetroArch backend; add --backend retroarch or drop {}",
                    ignored.join(", "),
                    if ignored.len() == 1 { "ies" } else { "y" },
                    if ignored.len() == 1 { "it" } else { "them" }
                );
            }
            Box::new(ra_metal_capture::hosted::Hosted)
        }
    };

    let source = match (&cli.image, &cli.core, &cli.rom) {
        (Some(image), _, _) => Source::Image(image.clone()),
        (None, Some(core), Some(rom)) => Source::Core {
            core: core.clone(),
            rom: rom.clone(),
            state: match (&cli.state, cli.slot) {
                (Some(file), _) => Some(StateSource::File(file.clone())),
                (None, Some(n)) => Some(StateSource::Slot(n)),
                (None, None) => None,
            },
            options: cli.core_options.clone(),
            skip_extension_check: skip_extension_check(&cli),
        },
        // clap requires --image or both --core and --rom.
        _ => bail!("--image, or --core with --rom, is required"),
    };
    let (Some(shader), Some(output)) = (&cli.shader, &cli.output) else {
        // clap requires both for a capture; only a subcommand waives them.
        bail!("--shader and --output are required");
    };
    let output =
        std::path::absolute(output).with_context(|| format!("resolving {}", output.display()))?;
    let request = Request {
        source,
        shader: shader.clone(),
        params: cli.param.clone(),
        window: cli.window_mode(),
        aspect: cli.aspect,
        frames: cli.frames,
        settle: cli.settle,
        advance: cli.advance,
        output,
        overwrite: cli.overwrite,
        config: Some(config::expand_tilde(&cli.config)),
        verbose: cli.verbose,
    };
    Ok((backend, request))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Sub::Entitle { app, yes }) = cli.command {
        use ra_metal_capture::retroarch::entitle::{Outcome, prompt_on_stdin, run};
        let mut confirm = || if yes { Ok(true) } else { prompt_on_stdin(&app) };
        return match run(&app, &mut confirm)? {
            Outcome::Signed | Outcome::AlreadyEntitled => Ok(()),
            Outcome::Declined => std::process::exit(1),
        };
    }
    let (backend, request) = build(cli)?;
    backend.prepare()?;
    match backend.run(request) {
        Err(e) if e.is::<Interrupted>() => {
            eprintln!("interrupted");
            std::process::exit(130);
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rom(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec![
            "ra-metal-capture",
            "--core",
            "c",
            "--rom",
            "r",
            "--shader",
            "p",
            "--output",
            "o",
        ];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    fn parse_raw(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec!["ra-metal-capture", "--shader", "p", "--output", "o"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn entitle_subcommand_needs_no_capture_flags() {
        let cli = Cli::try_parse_from(["ra-metal-capture", "entitle"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Sub::Entitle {
                app: PathBuf::from("/Applications/RetroArch.app"),
                yes: false
            })
        );
        let cli = Cli::try_parse_from(["ra-metal-capture", "entitle", "--app", "/x/R.app", "-y"])
            .unwrap();
        assert_eq!(
            cli.command,
            Some(Sub::Entitle {
                app: PathBuf::from("/x/R.app"),
                yes: true
            })
        );
    }

    #[test]
    fn entitle_subcommand_rejects_capture_flags() {
        assert!(Cli::try_parse_from(["ra-metal-capture", "entitle", "--shader", "p"]).is_err());
        assert!(
            Cli::try_parse_from([
                "ra-metal-capture",
                "--shader",
                "p",
                "--output",
                "o",
                "entitle"
            ])
            .is_err()
        );
    }

    #[test]
    fn aspect_defaults_to_native_and_parses_ratios() {
        assert_eq!(parse_rom(&[]).unwrap().aspect, config::Aspect::Native);
        assert!(matches!(
            parse_rom(&["--aspect", "4:3"]).unwrap().aspect,
            config::Aspect::Ratio(v) if (v - 4.0 / 3.0).abs() < 1e-9
        ));
        assert!(parse_rom(&["--aspect", "0"]).is_err());
    }

    #[test]
    fn param_is_repeatable_and_validated() {
        let cli = parse_rom(&["--param", "A=1", "--param", "B=2.5"]).unwrap();
        assert_eq!(cli.param.len(), 2);
        assert_eq!(cli.param[1].name, "B");
        assert_eq!(cli.param[1].value, 2.5);
        assert!(parse_rom(&["--param", "A"]).is_err());
        assert!(parse_rom(&[]).unwrap().param.is_empty());
    }

    #[test]
    fn overwrite_is_off_by_default() {
        assert!(!parse_rom(&[]).unwrap().overwrite);
        assert!(parse_rom(&["--overwrite"]).unwrap().overwrite);
    }

    #[test]
    fn a_capture_run_has_no_subcommand() {
        assert_eq!(parse_rom(&[]).unwrap().command, None);
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
        let err = parse_rom(&["--settle", "1e9"]).unwrap_err().to_string();
        assert!(err.contains("at most"), "{err}");
        assert_eq!(parse_rom(&["--settle", "3600"]).unwrap().settle, 3600.0);
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
        let cli = parse_raw(&["--image", "fixtures/sample.png"]).unwrap();
        assert_eq!(cli.image, Some(PathBuf::from("fixtures/sample.png")));
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
        assert_eq!(DEFAULT_BACKEND, BackendChoice::Librashader);
        #[cfg(not(feature = "librashader"))]
        assert_eq!(DEFAULT_BACKEND, BackendChoice::Retroarch);
        assert_eq!(
            parse_rom(&["--backend", "retroarch"]).unwrap().backend,
            BackendChoice::Retroarch
        );
        assert!(parse_raw(&["--image", "s.png", "--backend", "bogus"]).is_err());
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_backend_is_a_value_when_compiled_in() {
        let cli = parse_raw(&["--image", "s.png", "--backend", "librashader"]).unwrap();
        assert_eq!(cli.backend, BackendChoice::Librashader);
    }

    #[cfg(not(feature = "librashader"))]
    #[test]
    fn librashader_backend_and_flags_are_absent_without_the_feature() {
        let err = parse_raw(&["--image", "s.png", "--backend", "librashader"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("retroarch"), "{err}");
        assert!(parse_rom(&["--skip-extension-check"]).is_err());
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_accepts_core_and_rom_without_image() {
        let cli = parse_raw(&["--backend", "librashader", "--core", "c", "--rom", "r"]).unwrap();
        assert_eq!(cli.backend, BackendChoice::Librashader);
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
    fn backend_only_flags_are_detected() {
        let cli = parse_rom(&["--app", "/x", "--cmd-port", "1", "--keep-running"]).unwrap();
        assert_eq!(
            retroarch_only_flags(&cli),
            ["--app", "--cmd-port", "--keep-running"]
        );
        assert!(librashader_only_flags(&cli).is_empty());
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_only_flags_are_detected() {
        let cli = parse_rom(&["--core-options", "o", "--skip-extension-check"]).unwrap();
        assert_eq!(librashader_only_flags(&cli), ["--skip-extension-check"]);
        assert!(retroarch_only_flags(&cli).is_empty());
    }

    #[test]
    fn shader_is_required_by_every_run() {
        let err = Cli::try_parse_from(["ra-metal-capture", "--image", "s.png", "--output", "o"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--shader"), "{err}");
        assert_eq!(parse_rom(&[]).unwrap().shader, Some(PathBuf::from("p")));
    }

    #[test]
    fn core_options_requires_core() {
        assert!(parse_raw(&["--image", "s.png", "--core-options", "o.opt"]).is_err());
        let cli = parse_raw(&["--core", "c", "--rom", "r", "--core-options", "o.opt"]).unwrap();
        assert_eq!(cli.core_options, Some(PathBuf::from("o.opt")));
    }

    #[test]
    fn build_describes_a_core_run_with_a_slot() {
        let cli = parse_rom(&["--backend", "retroarch", "--slot", "3", "--frames", "2"]).unwrap();
        let (_, request) = build(cli).unwrap();
        assert_eq!(
            request.source,
            Source::Core {
                core: "c".into(),
                rom: PathBuf::from("r"),
                state: Some(StateSource::Slot(3)),
                options: None,
                skip_extension_check: false,
            }
        );
        assert_eq!(request.frames, 2);
        assert!(request.output.is_absolute());
    }

    #[test]
    fn build_expands_the_config_tilde() {
        let cli = parse_rom(&["--backend", "retroarch", "--config", "~/x/retroarch.cfg"]).unwrap();
        let (_, request) = build(cli).unwrap();
        let cfg = request.config.unwrap();
        assert!(!cfg.starts_with("~"), "{}", cfg.display());
        assert!(cfg.ends_with("x/retroarch.cfg"));
    }

    #[test]
    fn build_describes_an_image_run() {
        assert!(
            parse_raw(&["--backend", "retroarch", "--image", "i.png", "--state", "s"]).is_err(),
            "clap rejects --state with --image"
        );
        let cli = parse_raw(&["--backend", "retroarch", "--image", "i.png"]).unwrap();
        let (_, request) = build(cli).unwrap();
        assert_eq!(request.source, Source::Image(PathBuf::from("i.png")));
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn build_rejects_librashader_only_flags_under_retroarch() {
        let cli = parse_rom(&["--backend", "retroarch", "--skip-extension-check"]).unwrap();
        let err = build(cli).err().expect("an error").to_string();
        assert!(err.contains("--skip-extension-check"), "{err}");
    }

    #[test]
    fn build_passes_core_options_to_either_backend() {
        #[cfg(feature = "librashader")]
        let backends = ["retroarch", "librashader"];
        #[cfg(not(feature = "librashader"))]
        let backends = ["retroarch"];
        for backend in backends {
            let cli = parse_rom(&["--backend", backend, "--core-options", "o.opt"]).unwrap();
            let (_, request) = build(cli).unwrap();
            assert!(matches!(
                request.source,
                Source::Core { options: Some(ref o), .. } if o == &PathBuf::from("o.opt")
            ));
        }
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn build_rejects_retroarch_only_flags_under_librashader() {
        let cli = parse_raw(&[
            "--image",
            "fixtures/sample.png",
            "--keep-running",
            "--app",
            "/x",
        ])
        .unwrap();
        let err = build(cli).err().expect("an error").to_string();
        assert!(err.contains("--app, --keep-running"), "{err}");
        assert!(err.contains("--backend retroarch"), "{err}");
    }
}
