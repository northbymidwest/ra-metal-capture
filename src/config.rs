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
}
