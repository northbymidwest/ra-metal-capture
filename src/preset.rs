//! Shader preset parameter overrides, shared by both backends.
//!
//! Slang and GLSL shaders declare tunable values with `#pragma parameter`,
//! and a preset can pin them with `NAME = "VALUE"` lines. Rather than edit
//! the user's preset, a run with `--param` writes a "simple preset" into
//! its temp dir: a `#reference` to the original followed by the overrides.
//! Both librashader and RetroArch resolve such a file, with everything
//! else (passes, textures, other parameters) coming from the original.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// One `--param NAME=VALUE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: f64,
}

impl std::str::FromStr for Param {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, value) = s
            .split_once('=')
            .ok_or_else(|| format!("expected NAME=VALUE, got {s:?}"))?;
        let name = name.trim();
        if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == '=') {
            return Err(format!("bad parameter name in {s:?}"));
        }
        let value: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("bad value in {s:?}; expected a number"))?;
        if !value.is_finite() {
            return Err(format!("value in {s:?} must be finite"));
        }
        Ok(Param {
            name: name.to_string(),
            value,
        })
    }
}

/// Write `dir/override.<ext>` referencing `original` (which should be an
/// absolute path, since the reference is resolved relative to the wrapper)
/// and pinning `params`, and return its path. The preset format quotes
/// values with `"` and has no escape, so a path carrying a quote or a
/// newline is refused.
pub fn write_override_preset(original: &Path, params: &[Param], dir: &Path) -> Result<PathBuf> {
    let shown = original.to_string_lossy();
    if shown.contains('"') || shown.contains('\n') || shown.contains('\r') {
        bail!(
            "{} contains a quote or newline, which a preset reference cannot carry",
            original.display()
        );
    }
    let ext = original
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("slangp");
    let path = dir.join(format!("override.{ext}"));
    let mut text = format!("#reference \"{shown}\"\n");
    for p in params {
        text.push_str(&format!("{} = \"{}\"\n", p.name, p.value));
    }
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn param_parses_name_and_value() {
        let p: Param = "CRT_GAMMA=2.4".parse().unwrap();
        assert_eq!(p.name, "CRT_GAMMA");
        assert_eq!(p.value, 2.4);
        let p: Param = " scanline_weight = -0.5 ".parse().unwrap();
        assert_eq!(p.name, "scanline_weight");
        assert_eq!(p.value, -0.5);
    }

    #[test]
    fn param_rejects_missing_parts_and_bad_values() {
        for bad in ["=1", "a b=1", "a=x", "a", "a=", "a=nan", "a=inf", "a=b=1"] {
            assert!(bad.parse::<Param>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn override_preset_references_the_original_and_lists_the_values() {
        let tmp = tempfile::tempdir().unwrap();
        let params = [
            Param {
                name: "CRT_GAMMA".into(),
                value: 2.4,
            },
            Param {
                name: "MASK".into(),
                value: 0.0,
            },
        ];
        let path =
            write_override_preset(Path::new("/shaders/crt/royale.slangp"), &params, tmp.path())
                .unwrap();
        assert_eq!(path, tmp.path().join("override.slangp"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#reference \"/shaders/crt/royale.slangp\"\nCRT_GAMMA = \"2.4\"\nMASK = \"0\"\n"
        );
    }

    #[test]
    fn override_preset_keeps_a_glslp_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_override_preset(Path::new("/s/x.glslp"), &[], tmp.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "override.glslp");
    }

    #[test]
    fn override_preset_refuses_a_path_the_format_cannot_carry() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write_override_preset(Path::new("/s/a\"b.slangp"), &[], tmp.path()).is_err());
        assert!(write_override_preset(Path::new("/s/a\nb.slangp"), &[], tmp.path()).is_err());
    }
}
