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

/// The viewport's aspect ratio, shared by both backends: the source's own
/// (a core's reported aspect, an image's pixels), or one given on the
/// command line as a float or a `W:H` pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aspect {
    /// The core's `aspect_ratio` from its AV info, or the image's pixel
    /// aspect; RetroArch's "core provided" setting.
    Native,
    /// Width over height, always finite and positive.
    Ratio(f64),
}

impl Aspect {
    /// The ratio to use, given the source's native one.
    pub fn ratio_or(&self, native: f64) -> f64 {
        match self {
            Aspect::Native => native,
            Aspect::Ratio(r) => *r,
        }
    }
}

impl std::str::FromStr for Aspect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("native") {
            return Ok(Aspect::Native);
        }
        let positive = |part: &str| -> Result<f64, String> {
            let v: f64 = part
                .parse()
                .map_err(|_| format!("expected native, a number, or W:H, got {s:?}"))?;
            if !v.is_finite() || v <= 0.0 {
                return Err(format!("aspect must be finite and positive, got {s:?}"));
            }
            Ok(v)
        };
        match s.split_once(':') {
            Some((w, h)) => Ok(Aspect::Ratio(positive(w)? / positive(h)?)),
            None => Ok(Aspect::Ratio(positive(s)?)),
        }
    }
}

impl std::fmt::Display for Aspect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Aspect::Native => f.write_str("native"),
            Aspect::Ratio(r) => write!(f, "{r:.4}"),
        }
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
    fn aspect_parses_native_floats_and_ratios() {
        assert_eq!("native".parse::<Aspect>().unwrap(), Aspect::Native);
        assert_eq!("Native".parse::<Aspect>().unwrap(), Aspect::Native);
        assert_eq!("1.5".parse::<Aspect>().unwrap(), Aspect::Ratio(1.5));
        let r = "4:3".parse::<Aspect>().unwrap();
        assert!(
            matches!(r, Aspect::Ratio(v) if (v - 4.0 / 3.0).abs() < 1e-9),
            "{r:?}"
        );
    }

    #[test]
    fn aspect_rejects_zero_negative_and_malformed() {
        for bad in [
            "0", "-1", "4:0", "0:3", "abc", "4:3:2", "", "nan", "inf", "-4:3",
        ] {
            assert!(bad.parse::<Aspect>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn aspect_ratio_or_uses_native_only_for_native() {
        assert_eq!(Aspect::Native.ratio_or(1.25), 1.25);
        assert_eq!(Aspect::Ratio(2.0).ratio_or(1.25), 2.0);
    }

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
