//! The per-run appendconfig that overrides the user's `retroarch.cfg` for
//! one launch. The user's file is never written.

use crate::config::WindowMode;
use std::path::{Path, PathBuf};

/// Whether `path` can be written into a `retroarch.cfg` value unchanged:
/// the format quotes values with `"` and has no escape for one, and a
/// newline would start a new key.
pub fn is_config_safe(path: &Path) -> bool {
    let s = path.to_string_lossy();
    !s.contains('"') && !s.contains('\n') && !s.contains('\r')
}

/// Configuration for paused capture mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PausedConfig {
    pub port: u16,
    pub slot: u32,
}

/// The per-run appendconfig. Rendered text overrides the user's config for
/// this launch only; the user's file is never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendConfig {
    pub window: WindowMode,
    /// When a `--state` file was staged, the temp savestate dir to point RetroArch at.
    pub staged_states_dir: Option<PathBuf>,
    /// Configuration for paused capture mode; `None` means the settle flow
    /// is used instead, and no command port is enabled in the appendconfig.
    pub paused: Option<PausedConfig>,
    /// Whether this run loads a static image through RetroArch's built-in
    /// image viewer core rather than a game core.
    pub image_viewer: bool,
}

impl AppendConfig {
    pub fn render(&self) -> String {
        let mut lines: Vec<(&str, String)> = vec![
            ("config_save_on_exit", "false".into()),
            ("savestate_auto_save", "false".into()),
            ("savestate_auto_load", "false".into()),
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
        if let Some(dir) = &self.staged_states_dir {
            lines.push(("savestate_directory", dir.display().to_string()));
            lines.push(("sort_savestates_enable", "false".into()));
            lines.push(("sort_savestates_by_content_enable", "false".into()));
            lines.push(("savestates_in_content_dir", "false".into()));
        }
        if let Some(paused) = &self.paused {
            lines.push(("network_cmd_enable", "true".into()));
            lines.push(("network_cmd_port", paused.port.to_string()));
            lines.push(("state_slot", paused.slot.to_string()));
        }
        if self.image_viewer {
            lines.push(("builtin_imageviewer_enable", "true".into()));
        }
        lines
            .into_iter()
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect()
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

    const COMMON: &str = "config_save_on_exit = \"false\"\n\
savestate_auto_save = \"false\"\n\
savestate_auto_load = \"false\"\n\
pause_nonactive = \"false\"\n\
menu_show_load_content_animation = \"false\"\n\
video_font_enable = \"false\"\n";

    #[test]
    fn render_fill_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Fill {
                max: Size {
                    width: 2488,
                    height: 1382,
                },
            },
            staged_states_dir: None,
            paused: None,
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"20\"\n\
video_window_auto_width_max = \"2488\"\n\
video_window_auto_height_max = \"1382\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_size_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Exact(Size {
                width: 1600,
                height: 1440,
            }),
            staged_states_dir: None,
            paused: None,
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"true\"\n\
video_windowed_position_width = \"1600\"\n\
video_windowed_position_height = \"1440\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_scale_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Scale(4),
            staged_states_dir: None,
            paused: None,
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"4\"\n\
video_window_auto_width_max = \"0\"\n\
video_window_auto_height_max = \"0\"\n\
video_fullscreen_x = \"0\"\n\
video_fullscreen_y = \"0\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_fullscreen_adds_nothing() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
            paused: None,
            image_viewer: false,
        };
        assert_eq!(cfg.render(), COMMON);
    }

    #[test]
    fn render_fill_mode_with_staged_states_dir() {
        let cfg = AppendConfig {
            window: WindowMode::Fill {
                max: Size {
                    width: 2488,
                    height: 1382,
                },
            },
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
            paused: None,
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"20\"\n\
video_window_auto_width_max = \"2488\"\n\
video_window_auto_height_max = \"1382\"\n\
savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_staged_states_dir() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
            paused: None,
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_paused_keys_after_staged_states() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
            paused: Some(PausedConfig {
                port: 55355,
                slot: 0,
            }),
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n\
network_cmd_enable = \"true\"\n\
network_cmd_port = \"55355\"\n\
state_slot = \"0\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_paused_with_slot_and_no_staging() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
            paused: Some(PausedConfig {
                port: 60000,
                slot: 3,
            }),
            image_viewer: false,
        };
        let expected = format!(
            "{COMMON}network_cmd_enable = \"true\"\n\
network_cmd_port = \"60000\"\n\
state_slot = \"3\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_image_viewer_key_last() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
            paused: None,
            image_viewer: true,
        };
        assert_eq!(
            cfg.render(),
            format!("{COMMON}builtin_imageviewer_enable = \"true\"\n")
        );
    }
}
