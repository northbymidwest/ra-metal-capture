//! Checking an `--image` argument before a backend spends anything on it:
//! it must carry an extension the backend's decoder accepts, and exist.
//! Each backend has its own extension list; the check is the same.

use anyhow::{Result, bail};
use std::path::Path;

/// Whether `path` has an extension in `extensions`, case-insensitively.
pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase().as_str()))
}

/// Refuse `image` unless it has one of `extensions` and is an existing
/// file. `decoder` names, for the message, what would have decoded it.
pub fn validate(image: &Path, extensions: &[&str], decoder: &str) -> Result<()> {
    if !has_extension(image, extensions) {
        bail!(
            "{} does not have an image extension {decoder} accepts ({})",
            image.display(),
            extensions.join(", ")
        );
    }
    if !image.is_file() {
        bail!("image not found at {}", image.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_check_is_case_insensitive_and_needs_one() {
        assert!(has_extension(Path::new("a.PNG"), &["png"]));
        assert!(has_extension(Path::new("b.jpeg"), &["png", "jpeg"]));
        assert!(!has_extension(Path::new("c.gbc"), &["png"]));
        assert!(!has_extension(Path::new("noext"), &["png"]));
    }

    #[test]
    fn validate_checks_extension_then_existence() {
        let err = validate(Path::new("Cargo.toml"), &["png"], "the viewer")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("extension") && err.contains("the viewer"),
            "{err}"
        );
        let err = validate(Path::new("/nonexistent/x.png"), &["png"], "the viewer")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
        validate(Path::new("fixtures/sample.png"), &["png"], "the viewer").unwrap();
    }
}
