//! Reading `key = "value"` files as RetroArch writes them, and the sizes
//! both backends share. The RetroArch per-run config lives in
//! `retroarch::runconfig`.

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

/// Every `key = "value"` pair in RetroArch config text. Comments and blank
/// lines are skipped; surrounding quotes are stripped.
pub fn read_all(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// A width and height: points for a RetroArch window, pixels for the
/// hosted backend's output texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl std::str::FromStr for Size {
    type Err = String;

    /// `WxH`, both non-zero, as on the command line.
    fn from_str(s: &str) -> Result<Size, String> {
        let (w, h) = s
            .split_once('x')
            .ok_or_else(|| format!("expected WxH, got {s:?}"))?;
        let width: u32 = w.parse().map_err(|_| format!("bad width in {s:?}"))?;
        let height: u32 = h.parse().map_err(|_| format!("bad height in {s:?}"))?;
        if width == 0 || height == 0 {
            return Err("width and height must be non-zero".into());
        }
        Ok(Size { width, height })
    }
}

/// How the output is sized: RetroArch's window, or the hosted backend's
/// texture (see `render::output_size` for the pixel mapping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMode {
    /// Windowed, scaled up and then clamped to this maximum (points).
    Fill { max: Size },
    /// Windowed, exactly this size (points).
    Exact(Size),
    /// Windowed, an integer multiple of the core's native resolution, unclamped.
    Scale(u32),
    /// `-f` on the command line; nothing in the config.
    Fullscreen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parses_wxh_and_rejects_zero_and_malformed() {
        assert_eq!(
            "1600x1440".parse::<Size>().unwrap(),
            Size {
                width: 1600,
                height: 1440
            }
        );
        assert!("1600".parse::<Size>().is_err());
        assert!("0x10".parse::<Size>().is_err());
        assert!("ax10".parse::<Size>().is_err());
    }

    #[test]
    fn reads_quoted_values_and_skips_comments_and_blank_lines() {
        let text = "# comment\n\nvideo_driver = \"vulkan\"\nlibretro_directory = \"~/cores\"\n";
        let m = read_all(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["libretro_directory"], "~/cores");
        assert_eq!(m["video_driver"], "vulkan");
        assert!(!m.contains_key("missing"));
    }

    #[test]
    fn tolerates_unquoted_values_and_extra_whitespace() {
        let m = read_all("  x   =   3  \n");
        assert_eq!(m["x"], "3");
    }

    #[test]
    fn read_all_returns_every_key() {
        let text =
            "# comment\nsameboy_model = \"Auto\"\n\nsameboy_border = \"Never\"\nunquoted = 3\n";
        let m = read_all(text);
        assert_eq!(m.len(), 3);
        assert_eq!(m["sameboy_model"], "Auto");
        assert_eq!(m["sameboy_border"], "Never");
        assert_eq!(m["unquoted"], "3");
    }

    #[test]
    fn expands_tilde_prefix() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/a/b"), PathBuf::from(format!("{home}/a/b")));
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel/~x"), PathBuf::from("rel/~x"));
    }
}
