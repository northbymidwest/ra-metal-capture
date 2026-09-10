mod app;
mod capture;
mod config;
mod core;
mod display;
mod launch;
mod state;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

use config::{AppendConfig, Size, WindowMode};
use launch::{LaunchPlan, build_command};

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
    Ok(Size { width: w, height: h })
}

fn parse_settle(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("settle must be a non-negative, finite number of seconds, got {s:?}"));
    }
    Ok(v)
}

/// Launch RetroArch with a ROM, save state and shader preset, then capture
/// frames to a .gputrace with gpucapture.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// RetroArch .app bundle, or the binary inside it
    #[arg(long, default_value = "/Applications/RetroArch.app")]
    app: PathBuf,

    /// Path to a libretro .dylib, or a bare name resolved in libretro_directory
    #[arg(long)]
    core: String,

    /// Content file to load
    #[arg(long)]
    rom: PathBuf,

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
    #[arg(long, conflicts_with_all = ["size", "fullscreen"])]
    scale: Option<u32>,

    /// Launch fullscreen (-f)
    #[arg(long, conflicts_with_all = ["size", "scale"])]
    fullscreen: bool,

    /// Seconds to wait after RetroArch is capturable before capturing
    #[arg(long, default_value_t = 5.0, value_parser = parse_settle)]
    settle: f64,

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
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    let binary = app::resolve_binary(&cli.app)?;

    let cfg_text = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("reading {}", cli.config.display()))?;
    let keys = config::read_keys(&cfg_text, &["libretro_directory"]);
    let libretro_dir = keys
        .get("libretro_directory")
        .map(|s| config::expand_tilde(s))
        .unwrap_or_else(|| config::expand_tilde("~/Library/Application Support/RetroArch/cores"));
    let core = core::resolve_core(&cli.core, &libretro_dir)?;

    if !cli.rom.is_file() {
        bail!("ROM not found at {}", cli.rom.display());
    }
    if let Some(shader) = &cli.shader
        && !shader.is_file()
    {
        bail!("shader preset not found at {}", shader.display());
    }

    let tmp = tempfile::Builder::new()
        .prefix("retroarch-capture-")
        .tempdir()
        .context("creating temp dir")?;

    let (slot, staged_states_dir) = match (&cli.state, cli.slot) {
        (Some(state_file), _) => {
            let dir = tmp.path().join("states");
            let slot = state::stage(state_file, &cli.rom, &dir)?;
            (Some(slot), Some(dir))
        }
        (None, Some(n)) => (Some(n), None),
        (None, None) => (None, None),
    };

    let append = AppendConfig {
        window: cli.window_mode(),
        staged_states_dir,
    };
    let appendconfig = tmp.path().join("append.cfg");
    std::fs::write(&appendconfig, append.render())
        .with_context(|| format!("writing {}", appendconfig.display()))?;

    let plan = LaunchPlan {
        binary,
        core,
        rom: cli.rom.clone(),
        slot,
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
    let opts = capture::CaptureOptions {
        settle: Duration::from_secs_f64(cli.settle),
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

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec!["retroarch-capture", "--core", "c", "--rom", "r", "--output", "o"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn minimal_args_parse_with_defaults() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.app, PathBuf::from("/Applications/RetroArch.app"));
        assert_eq!(cli.settle, 5.0);
        assert_eq!(cli.frames, 1);
        assert!(cli.state.is_none() && cli.slot.is_none());
    }

    #[test]
    fn state_and_slot_conflict() {
        assert!(parse(&["--state", "s", "--slot", "1"]).is_err());
    }

    #[test]
    fn window_modes_conflict_pairwise() {
        assert!(parse(&["--size", "1x1", "--scale", "2"]).is_err());
        assert!(parse(&["--size", "1x1", "--fullscreen"]).is_err());
        assert!(parse(&["--scale", "2", "--fullscreen"]).is_err());
    }

    #[test]
    fn size_parses_and_maps_to_window_mode() {
        let cli = parse(&["--size", "1600x1440"]).unwrap();
        assert_eq!(cli.window_mode(), WindowMode::Exact(Size { width: 1600, height: 1440 }));
        assert!(parse(&["--size", "1600"]).is_err());
        assert!(parse(&["--size", "0x10"]).is_err());
    }

    #[test]
    fn scale_and_fullscreen_map_to_window_modes() {
        assert_eq!(parse(&["--scale", "4"]).unwrap().window_mode(), WindowMode::Scale(4));
        assert_eq!(parse(&["--fullscreen"]).unwrap().window_mode(), WindowMode::Fullscreen);
    }

    #[test]
    fn settle_rejects_negative_and_nan() {
        assert!(parse(&["--settle=-1"]).is_err());
        assert!(parse(&["--settle=nan"]).is_err());
        assert_eq!(parse(&["--settle", "2.5"]).unwrap().settle, 2.5);
    }

    #[test]
    fn frames_rejects_zero() {
        assert!(parse(&["--frames", "0"]).is_err());
        assert_eq!(parse(&["--frames", "1"]).unwrap().frames, 1);
    }
}
