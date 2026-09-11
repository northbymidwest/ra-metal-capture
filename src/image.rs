//! Validating paths for RetroArch's built-in image viewer core.

use anyhow::{Result, bail};
use std::path::Path;

/// Path extensions RetroArch's built-in image viewer core's
/// `valid_extensions` accepts.
pub const EXTENSIONS: [&str; 11] = [
    "jpg", "jpeg", "png", "bmp", "psd", "tga", "gif", "hdr", "pic", "ppm", "pgm",
];

/// Whether `path` has an extension the built-in image viewer core accepts,
/// checked case-insensitively.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

/// `--image` for the RetroArch backend must carry an extension the image
/// viewer accepts and exist.
pub fn validate(image: &Path) -> Result<()> {
    if !is_image_path(image) {
        bail!(
            "{} does not have an image extension RetroArch's image viewer accepts ({})",
            image.display(),
            EXTENSIONS.join(", ")
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
    fn validate_checks_extension_then_existence() {
        let err = validate(Path::new("Cargo.toml")).unwrap_err().to_string();
        assert!(err.contains("extension"), "{err}");
        let err = validate(Path::new("/nonexistent/x.png"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
        validate(Path::new("fixtures/sample.png")).unwrap();
    }

    #[test]
    fn image_extension_check() {
        assert!(is_image_path(Path::new("a.PNG")));
        assert!(is_image_path(Path::new("b.jpeg")));
        assert!(!is_image_path(Path::new("c.gbc")));
        assert!(!is_image_path(Path::new("noext")));
    }
}
