#![deny(unsafe_code)]

//! The command line: parse arguments, turn them into a backend-neutral
//! [`Request`], pick a [`Backend`], and run it. Everything about how a
//! capture is actually made lives in the backends.

use anyhow::{Context, Result, bail};
use clap::Parser;
use ra_metal_capture::backend::{Backend, Request, Source, StateSource};
use ra_metal_capture::config::{self, Size, WindowMode};
use ra_metal_capture::display;
use ra_metal_capture::retroarch::RetroArch;
use std::path::PathBuf;

fn default_config() -> PathBuf {
    config::expand_tilde("~/Library/Application Support/RetroArch/config/retroarch.cfg")
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

/// Which backend a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum BackendChoice {
    /// Launches RetroArch.app and records its presented frames with gpucapture
    Retroarch,
    /// Renders a static image or a hosted libretro core through librashader
    /// in this process, recorded with MTLCaptureManager
    Librashader,
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
    backend: BackendChoice,

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
    #[arg(long, value_parser = clap::value_parser!(Size), conflicts_with_all = ["scale", "fullscreen"])]
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
            hosted_backend()?
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
            skip_extension_check: cli.skip_extension_check,
        },
        // clap requires --image or both --core and --rom.
        _ => bail!("--image, or --core with --rom, is required"),
    };
    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let request = Request {
        source,
        shader: cli.shader.clone(),
        window: cli.window_mode(),
        frames: cli.frames,
        settle: cli.settle,
        advance: cli.advance,
        output,
        config: Some(cli.config.clone()),
        verbose: cli.verbose,
    };
    Ok((backend, request))
}

#[cfg(feature = "librashader")]
fn hosted_backend() -> Result<Box<dyn Backend>> {
    Ok(Box::new(ra_metal_capture::hosted::Hosted))
}

#[cfg(not(feature = "librashader"))]
fn hosted_backend() -> Result<Box<dyn Backend>> {
    bail!(
        "this build has no librashader backend; reinstall with the \"librashader\" \
         feature (it is on by default)"
    )
}

fn main() -> Result<()> {
    let (backend, request) = build(Cli::parse())?;
    backend.prepare()?;
    backend.run(request)
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
        let cli = parse_raw(&[
            "--image",
            "s.png",
            "--shader",
            "p.slangp",
            "--backend",
            "librashader",
        ])
        .unwrap();
        assert_eq!(cli.backend, BackendChoice::Librashader);
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
    fn build_describes_an_image_run() {
        assert!(
            parse_raw(&["--backend", "retroarch", "--image", "i.png", "--state", "s"]).is_err(),
            "clap rejects --state with --image"
        );
        let cli = parse_raw(&["--backend", "retroarch", "--image", "i.png"]).unwrap();
        let (_, request) = build(cli).unwrap();
        assert_eq!(request.source, Source::Image(PathBuf::from("i.png")));
    }

    #[test]
    fn build_rejects_librashader_only_flags_under_retroarch() {
        let cli = parse_rom(&["--backend", "retroarch", "--core-options", "o.opt"]).unwrap();
        let err = build(cli).err().expect("an error").to_string();
        assert!(err.contains("--core-options"), "{err}");
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

    #[cfg(not(feature = "librashader"))]
    #[test]
    fn build_refuses_librashader_without_the_feature() {
        let cli =
            parse_raw(&["--image", "fixtures/sample.png", "--backend", "librashader"]).unwrap();
        let err = build(cli).err().expect("an error").to_string();
        assert!(err.contains("librashader"), "{err}");
    }
}
