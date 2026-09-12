//! The RetroArch backend: launch a RetroArch.app with the request's core,
//! content, state, and preset, and record its presented frames with Apple's
//! `gpucapture(1)`. RetroArch runs on a config written for the run, never
//! the user's; the user's `retroarch.cfg` is consulted only to find a core,
//! a state slot, or the system directory that is not in the default place.
//! The submodules are its parts: locating the app, assembling the command
//! line and run config, driving the capture, and the UDP command interface.

pub mod app;
pub mod capture;
pub mod entitle;
pub mod image;
pub mod launch;
pub mod remote;
pub mod runconfig;

use crate::backend::{Backend, Request, Source, StateSource};
use crate::config;
use crate::layout::{DirResolver, describe_tried};
use crate::{display, preset, state};
use anyhow::{Context, Result, anyhow, bail};
use launch::{LaunchPlan, build_command};
use runconfig::{PausedConfig, RunConfig};
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
        let mut dirs = DirResolver::for_config(request.config.as_deref(), request.verbose);
        let (core, content, state, options) = match &request.source {
            Source::Image(path) => {
                image::validate(path)?;
                (None, path.clone(), None, None)
            }
            Source::Core {
                core: core_arg,
                rom,
                state,
                options,
                skip_extension_check,
            } => {
                if *skip_extension_check {
                    bail!("--skip-extension-check only applies to the librashader backend");
                }
                if !rom.is_file() {
                    bail!("ROM not found at {}", rom.display());
                }
                (
                    Some(dirs.core(core_arg)?),
                    rom.clone(),
                    state.clone(),
                    options.clone(),
                )
            }
        };

        if !request.shader.is_file() {
            bail!("shader preset not found at {}", request.shader.display());
        }
        let binary = app::resolve_binary(&self.app)?;

        if state.is_some() {
            // Guard against the run config's network_cmd_port already
            // belonging to somebody else's RetroArch before we launch ours.
            remote::probe_free(self.cmd_port)?;
        }

        let tmp = tempfile::Builder::new()
            .prefix("ra-metal-capture-")
            .tempdir()
            .context("creating temp dir")?;

        // The core's options: the user's file copied in, or an empty file so
        // the core runs on its built-in defaults, as a hosted core does.
        let core_options = tmp.path().join("core-options.cfg");
        match &options {
            Some(path) => {
                std::fs::copy(path, &core_options)
                    .with_context(|| format!("copying core options {}", path.display()))?;
            }
            None => std::fs::write(&core_options, "")
                .with_context(|| format!("writing {}", core_options.display()))?,
        }

        // RetroArch silently falls back to its default save directory when
        // the configured one does not exist, so make it before the launch.
        let savefile_dir = tmp.path().join("saves");
        std::fs::create_dir_all(&savefile_dir)
            .with_context(|| format!("creating {}", savefile_dir.display()))?;

        let paused = match state {
            Some(StateSource::File(state_file)) => {
                let dir = tmp.path().join("states");
                let slot = state::stage(&state_file, &content, &dir)?;
                Some(PausedConfig {
                    port: self.cmd_port,
                    slot,
                    states: state::StateDirs {
                        savestate_directory: dir,
                        sort_by_core: false,
                        sort_by_content: false,
                        in_content_dir: false,
                    },
                })
            }
            // RetroArch finds the slot itself, under the core's own name;
            // it only needs the states directory and how it is sorted.
            Some(StateSource::Slot(slot)) => {
                let layout = dirs
                    .locate_layout(
                        "states directory",
                        |d| d.states.savestate_directory.clone(),
                        |p| p.is_dir(),
                    )
                    .map_err(|tried| {
                        anyhow!("states directory not found at {}", describe_tried(&tried))
                    })?;
                Some(PausedConfig {
                    port: self.cmd_port,
                    slot,
                    states: layout.states.clone(),
                })
            }
            None => None,
        };

        let run_config = RunConfig {
            window: display::for_retroarch_window(request.window.clone()),
            aspect: request.aspect,
            system_dir: dirs.system_dir(),
            savefile_dir,
            core_options,
            paused: paused.clone(),
            image_viewer: matches!(request.source, Source::Image(_)),
        };
        let rendered = run_config.render()?;
        let config_path = tmp.path().join("retroarch.cfg");
        std::fs::write(&config_path, &rendered)
            .with_context(|| format!("writing {}", config_path.display()))?;

        let shader = if request.params.is_empty() {
            request.shader.clone()
        } else {
            let original = std::path::absolute(&request.shader)
                .with_context(|| format!("resolving {}", request.shader.display()))?;
            let wrapper = preset::write_override_preset(&original, &request.params, tmp.path())?;
            if request.verbose {
                eprintln!("parameter overrides: {}", wrapper.display());
            }
            wrapper
        };
        let plan = LaunchPlan {
            binary,
            core,
            content,
            shader,
            config: config_path,
            fullscreen: request.window == config::WindowMode::Fullscreen,
            verbose: request.verbose,
        };
        let cmd = build_command(&plan);

        if request.verbose {
            eprintln!("run config:\n{rendered}");
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
            app: self.app.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowMode;

    fn request(source: Source) -> Request {
        Request {
            source,
            shader: PathBuf::from("/nonexistent/p.slangp"),
            params: vec![],
            window: WindowMode::Fullscreen,
            aspect: crate::config::Aspect::Native,
            frames: 1,
            settle: 5.0,
            advance: 1,
            output: PathBuf::from("/tmp/x.gputrace"),
            config: None,
            verbose: false,
        }
    }

    #[test]
    fn rejects_the_extension_override_before_launching() {
        // The app path does not exist either; the request is refused before
        // the app is resolved, so the message is about the override.
        let backend = RetroArch {
            app: PathBuf::from("/nonexistent/RetroArch.app"),
            ..RetroArch::default()
        };
        let err = backend
            .run(request(Source::Core {
                core: "c".into(),
                rom: PathBuf::from("r"),
                state: None,
                options: None,
                skip_extension_check: true,
            }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--skip-extension-check"), "{err}");
    }

    #[test]
    fn defaults_name_the_stock_app_and_port() {
        let d = RetroArch::default();
        assert_eq!(d.app, PathBuf::from("/Applications/RetroArch.app"));
        assert_eq!(d.cmd_port, 55355);
        assert!(!d.keep_running);
    }
}
