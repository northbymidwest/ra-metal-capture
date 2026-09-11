//! The per-run `retroarch.cfg` that RetroArch is launched on (`-c`). It is
//! written from scratch for each run and holds only what this tool sets;
//! RetroArch fills everything else from its compiled defaults, so the
//! user's own config is never read for the run and never written.

use crate::config::WindowMode;
use crate::state::StateDirs;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Whether `path` can be written into a `retroarch.cfg` value unchanged:
/// the format quotes values with `"` and has no escape for one, and a
/// newline would start a new key.
pub fn is_config_safe(path: &Path) -> bool {
    let s = path.to_string_lossy();
    !s.contains('"') && !s.contains('\n') && !s.contains('\r')
}

/// Configuration for paused capture mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausedConfig {
    /// UDP port for the command interface, enabled for this run only.
    pub port: u16,
    /// The slot RetroArch loads on `LOAD_STATE`.
    pub slot: u32,
    /// Where RetroArch looks for that slot: a temp dir holding a staged
    /// `--state` file, or the user's states layout for `--slot`.
    pub states: StateDirs,
}

/// The per-run config. Rendered text is the whole config RetroArch runs
/// on; every key not listed takes RetroArch's compiled default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    pub window: WindowMode,
    /// Where cores find BIOS and other system files.
    pub system_dir: PathBuf,
    /// A per-run save directory. The launch also passes `--sram-mode
    /// noload-nosave`, which stops RetroArch touching `.srm`/`.rtc` files
    /// at all; this directory is the second layer, in case a RetroArch
    /// ignores the flag, and it must exist, since RetroArch falls back to
    /// its default directory when a configured one is missing.
    pub savefile_dir: PathBuf,
    /// The core options file: a copy of `--core-options`, or empty so the
    /// core runs on its built-in defaults.
    pub core_options: PathBuf,
    /// Configuration for paused capture mode; `None` means the settle flow
    /// is used instead, and no command port is enabled.
    pub paused: Option<PausedConfig>,
    /// Whether this run loads a static image through RetroArch's built-in
    /// image viewer core rather than a game core.
    pub image_viewer: bool,
}

impl RunConfig {
    /// The config text. Fails if a path it would write cannot be carried
    /// by a `retroarch.cfg` value (see [`is_config_safe`]).
    pub fn render(&self) -> Result<String> {
        let mut paths = vec![&self.system_dir, &self.savefile_dir, &self.core_options];
        if let Some(paused) = &self.paused {
            paths.push(&paused.states.savestate_directory);
        }
        if let Some(bad) = paths.into_iter().find(|p| !is_config_safe(p)) {
            bail!(
                "{} contains a quote or newline, which a retroarch.cfg value cannot carry",
                bad.display()
            );
        }
        let flag = |b: bool| if b { "true" } else { "false" }.to_string();
        let mut lines: Vec<(&str, String)> = vec![
            ("config_save_on_exit", "false".into()),
            ("video_driver", "vulkan".into()),
            ("video_shader_enable", "true".into()),
            ("system_directory", self.system_dir.display().to_string()),
            (
                "savefile_directory",
                self.savefile_dir.display().to_string(),
            ),
            ("sort_savefiles_enable", "false".into()),
            ("sort_savefiles_by_content_enable", "false".into()),
            ("savefiles_in_content_dir", "false".into()),
            ("savestate_auto_save", "false".into()),
            ("savestate_auto_load", "false".into()),
            ("global_core_options", "true".into()),
            ("core_options_path", self.core_options.display().to_string()),
            ("game_specific_options", "false".into()),
            ("auto_overrides_enable", "false".into()),
            ("auto_remaps_enable", "false".into()),
            ("auto_shaders_enable", "false".into()),
            ("history_list_enable", "false".into()),
            ("pause_nonactive", "false".into()),
            ("menu_show_load_content_animation", "false".into()),
            ("video_font_enable", "false".into()),
        ];
        match &self.window {
            WindowMode::Fill { max } => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "false".into()));
                lines.push(("video_scale", "20".into()));
                lines.push(("video_window_auto_width_max", max.width.to_string()));
                lines.push(("video_window_auto_height_max", max.height.to_string()));
            }
            WindowMode::Exact(size) => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "true".into()));
                lines.push(("video_windowed_position_width", size.width.to_string()));
                lines.push(("video_windowed_position_height", size.height.to_string()));
            }
            WindowMode::Scale(n) => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "false".into()));
                lines.push(("video_scale", n.to_string()));
                lines.push(("video_window_auto_width_max", "0".into()));
                lines.push(("video_window_auto_height_max", "0".into()));
                lines.push(("video_fullscreen_x", "0".into()));
                lines.push(("video_fullscreen_y", "0".into()));
            }
            WindowMode::Fullscreen => {}
        }
        if let Some(paused) = &self.paused {
            let states = &paused.states;
            lines.push((
                "savestate_directory",
                states.savestate_directory.display().to_string(),
            ));
            lines.push(("sort_savestates_enable", flag(states.sort_by_core)));
            lines.push((
                "sort_savestates_by_content_enable",
                flag(states.sort_by_content),
            ));
            lines.push(("savestates_in_content_dir", flag(states.in_content_dir)));
            lines.push(("network_cmd_enable", "true".into()));
            lines.push(("network_cmd_port", paused.port.to_string()));
            lines.push(("state_slot", paused.slot.to_string()));
        }
        if self.image_viewer {
            lines.push(("builtin_imageviewer_enable", "true".into()));
        }
        Ok(lines
            .into_iter()
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Size;

    #[test]
    fn config_safe_rejects_quotes_and_newlines() {
        assert!(is_config_safe(Path::new("/tmp/ra-metal-capture-abc")));
        assert!(!is_config_safe(Path::new("/tmp/a\"b")));
        assert!(!is_config_safe(Path::new("/tmp/a\nb")));
    }

    /// The keys every run sets, before the window mode's.
    const COMMON: &str = "config_save_on_exit = \"false\"\n\
video_driver = \"vulkan\"\n\
video_shader_enable = \"true\"\n\
system_directory = \"/sys\"\n\
savefile_directory = \"/tmp/x/saves\"\n\
sort_savefiles_enable = \"false\"\n\
sort_savefiles_by_content_enable = \"false\"\n\
savefiles_in_content_dir = \"false\"\n\
savestate_auto_save = \"false\"\n\
savestate_auto_load = \"false\"\n\
global_core_options = \"true\"\n\
core_options_path = \"/tmp/x/core-options.cfg\"\n\
game_specific_options = \"false\"\n\
auto_overrides_enable = \"false\"\n\
auto_remaps_enable = \"false\"\n\
auto_shaders_enable = \"false\"\n\
history_list_enable = \"false\"\n\
pause_nonactive = \"false\"\n\
menu_show_load_content_animation = \"false\"\n\
video_font_enable = \"false\"\n";

    fn base(window: WindowMode) -> RunConfig {
        RunConfig {
            window,
            system_dir: PathBuf::from("/sys"),
            savefile_dir: PathBuf::from("/tmp/x/saves"),
            core_options: PathBuf::from("/tmp/x/core-options.cfg"),
            paused: None,
            image_viewer: false,
        }
    }

    fn staged_states() -> StateDirs {
        StateDirs {
            savestate_directory: PathBuf::from("/tmp/x/states"),
            sort_by_core: false,
            sort_by_content: false,
            in_content_dir: false,
        }
    }

    #[test]
    fn render_fill_mode() {
        let cfg = base(WindowMode::Fill {
            max: Size {
                width: 2488,
                height: 1382,
            },
        });
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"20\"\n\
video_window_auto_width_max = \"2488\"\n\
video_window_auto_height_max = \"1382\"\n"
        );
        assert_eq!(cfg.render().unwrap(), expected);
    }

    #[test]
    fn render_size_mode() {
        let cfg = base(WindowMode::Exact(Size {
            width: 1600,
            height: 1440,
        }));
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"true\"\n\
video_windowed_position_width = \"1600\"\n\
video_windowed_position_height = \"1440\"\n"
        );
        assert_eq!(cfg.render().unwrap(), expected);
    }

    #[test]
    fn render_scale_mode() {
        let cfg = base(WindowMode::Scale(4));
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"4\"\n\
video_window_auto_width_max = \"0\"\n\
video_window_auto_height_max = \"0\"\n\
video_fullscreen_x = \"0\"\n\
video_fullscreen_y = \"0\"\n"
        );
        assert_eq!(cfg.render().unwrap(), expected);
    }

    #[test]
    fn render_fullscreen_adds_nothing() {
        assert_eq!(base(WindowMode::Fullscreen).render().unwrap(), COMMON);
    }

    #[test]
    fn render_paused_with_a_staged_state() {
        let mut cfg = base(WindowMode::Fullscreen);
        cfg.paused = Some(PausedConfig {
            port: 55355,
            slot: 0,
            states: staged_states(),
        });
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n\
network_cmd_enable = \"true\"\n\
network_cmd_port = \"55355\"\n\
state_slot = \"0\"\n"
        );
        assert_eq!(cfg.render().unwrap(), expected);
    }

    #[test]
    fn render_paused_with_a_slot_in_the_users_layout() {
        let mut cfg = base(WindowMode::Fullscreen);
        cfg.paused = Some(PausedConfig {
            port: 60000,
            slot: 3,
            states: StateDirs {
                savestate_directory: PathBuf::from("/Users/u/Documents/RetroArch/states"),
                sort_by_core: true,
                sort_by_content: false,
                in_content_dir: false,
            },
        });
        let expected = format!(
            "{COMMON}savestate_directory = \"/Users/u/Documents/RetroArch/states\"\n\
sort_savestates_enable = \"true\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n\
network_cmd_enable = \"true\"\n\
network_cmd_port = \"60000\"\n\
state_slot = \"3\"\n"
        );
        assert_eq!(cfg.render().unwrap(), expected);
    }

    #[test]
    fn render_image_viewer_key_last() {
        let mut cfg = base(WindowMode::Fullscreen);
        cfg.image_viewer = true;
        assert_eq!(
            cfg.render().unwrap(),
            format!("{COMMON}builtin_imageviewer_enable = \"true\"\n")
        );
    }

    #[test]
    fn render_refuses_a_path_the_format_cannot_carry() {
        let mut cfg = base(WindowMode::Fullscreen);
        cfg.system_dir = PathBuf::from("/sys\"tem");
        let err = cfg.render().unwrap_err().to_string();
        assert!(err.contains("quote"), "{err}");
        let mut cfg = base(WindowMode::Fullscreen);
        cfg.paused = Some(PausedConfig {
            port: 1,
            slot: 0,
            states: StateDirs {
                savestate_directory: PathBuf::from("/st\nates"),
                ..staged_states()
            },
        });
        assert!(cfg.render().is_err());
    }
}
