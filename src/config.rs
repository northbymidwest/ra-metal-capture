use std::collections::HashMap;
use std::path::PathBuf;

/// Expand a leading `~` or `~/` using `$HOME`. Other paths are returned unchanged.
pub fn expand_tilde(s: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (s, home) {
        ("~", Some(home)) => home,
        (s, Some(home)) if s.starts_with("~/") => home.join(&s[2..]),
        (s, _) => PathBuf::from(s),
    }
}

/// Read the named keys from RetroArch `key = "value"` config text.
/// Missing keys are simply absent from the map. Surrounding quotes are stripped.
pub fn read_keys(text: &str, keys: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if !keys.contains(&k) {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        out.insert(k.to_string(), v.to_string());
    }
    out
}

/// How the RetroArch window is sized for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMode {
    /// Windowed, scaled up and then clamped to these maxima (points).
    Fill { max_width: u32, max_height: u32 },
    /// Windowed, exactly this size (points).
    Size { width: u32, height: u32 },
    /// Windowed, an integer multiple of the core's native resolution, unclamped.
    Scale(u32),
    /// `-f` on the command line; nothing in the config.
    Fullscreen,
}

/// The per-run appendconfig. Rendered text overrides the user's config for
/// this launch only; the user's file is never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendConfig {
    pub window: WindowMode,
    /// When a `--state` file was staged, the temp savestate dir to point RetroArch at.
    pub staged_states_dir: Option<PathBuf>,
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
            WindowMode::Fill { max_width, max_height } => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "false".into()));
                lines.push(("video_scale", "20".into()));
                lines.push(("video_window_auto_width_max", max_width.to_string()));
                lines.push(("video_window_auto_height_max", max_height.to_string()));
            }
            WindowMode::Size { width, height } => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "true".into()));
                lines.push(("video_windowed_position_width", width.to_string()));
                lines.push(("video_windowed_position_height", height.to_string()));
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
        lines
            .into_iter()
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_values() {
        let text = "video_driver = \"vulkan\"\nlibretro_directory = \"~/cores\"\n";
        let m = read_keys(text, &["libretro_directory", "video_driver"]);
        assert_eq!(m["libretro_directory"], "~/cores");
        assert_eq!(m["video_driver"], "vulkan");
    }

    #[test]
    fn skips_comments_blank_lines_and_unrequested_keys() {
        let text = "# comment\n\nfoo = \"1\"\nbar = \"2\"\n";
        let m = read_keys(text, &["bar"]);
        assert_eq!(m.len(), 1);
        assert_eq!(m["bar"], "2");
    }

    #[test]
    fn missing_key_is_absent() {
        let m = read_keys("a = \"1\"\n", &["b"]);
        assert!(m.get("b").is_none());
    }

    #[test]
    fn tolerates_unquoted_values_and_extra_whitespace() {
        let m = read_keys("  x   =   3  \n", &["x"]);
        assert_eq!(m["x"], "3");
    }

    #[test]
    fn expands_tilde_prefix() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/a/b"), PathBuf::from(format!("{home}/a/b")));
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel/~x"), PathBuf::from("rel/~x"));
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
            window: WindowMode::Fill { max_width: 2488, max_height: 1382 },
            staged_states_dir: None,
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
            window: WindowMode::Size { width: 1600, height: 1440 },
            staged_states_dir: None,
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
        };
        assert_eq!(cfg.render(), COMMON);
    }

    #[test]
    fn render_staged_states_dir() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
        };
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }
}
