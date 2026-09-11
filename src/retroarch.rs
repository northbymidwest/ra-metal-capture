//! The RetroArch backend: launch a RetroArch.app with the request's core,
//! content, state, and preset, and record its presented frames with Apple's
//! `gpucapture(1)`.

use crate::backend::{Backend, Request, Source, StateSource};
use crate::config::{self, AppendConfig, PausedConfig};
use crate::launch::{LaunchPlan, build_command};
use crate::layout::{DirResolver, Located, describe_tried};
use crate::{app, capture, core, image, remote, state};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

/// How to reach RetroArch: the app to launch and the command port to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetroArch {
    /// A `.app` bundle or the binary inside it.
    pub app: PathBuf,
    /// UDP port for RetroArch's command interface, enabled for this run only.
    pub cmd_port: u16,
    /// Leave RetroArch running after the capture.
    pub keep_running: bool,
}

impl Default for RetroArch {
    fn default() -> Self {
        RetroArch {
            app: PathBuf::from("/Applications/RetroArch.app"),
            cmd_port: 55355,
            keep_running: false,
        }
    }
}

impl Backend for RetroArch {
    fn run(&self, request: Request) -> Result<()> {
        let (core, content, state) = match &request.source {
            Source::Image(path) => {
                image::validate(path)?;
                (None, path.clone(), None)
            }
            Source::Core {
                core: core_arg,
                rom,
                state,
                options,
                skip_extension_check,
            } => {
                if options.is_some() {
                    bail!("core options are not supported by the RetroArch backend");
                }
                if *skip_extension_check {
                    bail!("the extension check is not part of the RetroArch backend");
                }
                if !rom.is_file() {
                    bail!("ROM not found at {}", rom.display());
                }
                let core_path = resolve_core(core_arg, &request.config, request.verbose)?;
                (Some(core_path), rom.clone(), state.clone())
            }
        };

        if let Some(shader) = &request.shader
            && !shader.is_file()
        {
            bail!("shader preset not found at {}", shader.display());
        }
        let binary = app::resolve_binary(&self.app)?;

        if state.is_some() {
            // Guard against the appendconfig's network_cmd_port already
            // belonging to somebody else's RetroArch before we launch ours.
            remote::probe_free(self.cmd_port)?;
        }

        let tmp = tempfile::Builder::new()
            .prefix("ra-metal-capture-")
            .tempdir()
            .context("creating temp dir")?;
        if !config::is_config_safe(tmp.path()) {
            bail!(
                "temp dir {} contains a quote or newline, which a retroarch.cfg value cannot carry; set TMPDIR to a plain path",
                tmp.path().display()
            );
        }

        let (paused, staged_states_dir) = match state {
            Some(StateSource::File(state_file)) => {
                let dir = tmp.path().join("states");
                let slot = state::stage(&state_file, &content, &dir)?;
                (
                    Some(PausedConfig {
                        port: self.cmd_port,
                        slot,
                    }),
                    Some(dir),
                )
            }
            Some(StateSource::Slot(slot)) => (
                Some(PausedConfig {
                    port: self.cmd_port,
                    slot,
                }),
                None,
            ),
            None => (None, None),
        };

        let append = AppendConfig {
            window: request.window.clone(),
            staged_states_dir,
            paused,
            image_viewer: matches!(request.source, Source::Image(_)),
        };
        let appendconfig = tmp.path().join("append.cfg");
        std::fs::write(&appendconfig, append.render())
            .with_context(|| format!("writing {}", appendconfig.display()))?;

        let plan = LaunchPlan {
            binary,
            core,
            content,
            shader: request.shader.clone(),
            appendconfig,
            fullscreen: request.window == config::WindowMode::Fullscreen,
            verbose: request.verbose,
        };
        let cmd = build_command(&plan);

        if request.verbose {
            eprintln!("appendconfig:\n{}", append.render());
            eprintln!("command: {}", cmd.display());
        }

        let trigger = match paused {
            Some(p) => capture::Trigger::Paused {
                port: p.port,
                advance: request.advance,
            },
            None => capture::Trigger::Settle(Duration::from_secs_f64(request.settle)),
        };
        let opts = capture::CaptureOptions {
            trigger,
            frames: request.frames,
            output: request.output.clone(),
            keep_running: self.keep_running,
            ready_timeout: Duration::from_secs(30),
            log_path: tmp.path().join("retroarch.log"),
        };
        capture::run(&cmd, &opts)?;

        if self.keep_running {
            let kept = tmp.keep();
            eprintln!("kept {} for the running RetroArch", kept.display());
        }

        println!("{}", request.output.display());
        Ok(())
    }
}

/// A core path as given, or a bare name found in RetroArch's cores
/// directory: the default location first, `retroarch.cfg`'s
/// `libretro_directory` only if the default has no such core.
fn resolve_core(core_arg: &str, config_path: &std::path::Path, verbose: bool) -> Result<PathBuf> {
    if std::path::Path::new(core_arg).is_file() {
        return Ok(PathBuf::from(core_arg));
    }
    let mut dirs = DirResolver::for_config(config_path, verbose);
    match dirs.locate(
        "core",
        |d| d.libretro_dir.clone(),
        |dir| core::resolve_core(core_arg, dir).is_ok(),
    ) {
        Located::Found(dir) => core::resolve_core(core_arg, &dir),
        Located::Missing(tried) => {
            bail!("core {core_arg} not found in {}", describe_tried(&tried))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowMode;

    fn request(source: Source) -> Request {
        Request {
            source,
            shader: None,
            window: WindowMode::Fullscreen,
            frames: 1,
            settle: 5.0,
            advance: 1,
            output: PathBuf::from("/tmp/x.gputrace"),
            config: PathBuf::from("/nonexistent/retroarch.cfg"),
            verbose: false,
        }
    }

    #[test]
    fn rejects_core_options_and_the_extension_override_before_launching() {
        // The app path does not exist either; the request is refused before
        // the app is resolved, so the message is about the options.
        let backend = RetroArch {
            app: PathBuf::from("/nonexistent/RetroArch.app"),
            ..RetroArch::default()
        };
        let err = backend
            .run(request(Source::Core {
                core: "c".into(),
                rom: PathBuf::from("r"),
                state: None,
                options: Some(PathBuf::from("o.opt")),
                skip_extension_check: false,
            }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("core options"), "{err}");
    }

    #[test]
    fn defaults_name_the_stock_app_and_port() {
        let d = RetroArch::default();
        assert_eq!(d.app, PathBuf::from("/Applications/RetroArch.app"));
        assert_eq!(d.cmd_port, 55355);
        assert!(!d.keep_running);
    }
}
