//! Validating paths for RetroArch's built-in image viewer core.

use crate::image_file;
use anyhow::Result;
use std::path::Path;

/// Path extensions RetroArch's built-in image viewer core's
/// `valid_extensions` accepts.
pub const EXTENSIONS: [&str; 11] = [
    "jpg", "jpeg", "png", "bmp", "psd", "tga", "gif", "hdr", "pic", "ppm", "pgm",
];

/// Whether `path` has an extension the built-in image viewer core accepts,
/// checked case-insensitively.
pub fn is_image_path(path: &Path) -> bool {
    image_file::has_extension(path, &EXTENSIONS)
}

/// `--image` for the RetroArch backend must carry an extension the image
/// viewer accepts and exist.
pub fn validate(image: &Path) -> Result<()> {
    image_file::validate(image, &EXTENSIONS, "RetroArch's image viewer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_list_is_retroarchs_not_the_image_crates() {
        assert!(is_image_path(Path::new("layers.psd")));
        assert!(!is_image_path(Path::new("x.pam")));
        let err = validate(Path::new("Cargo.toml")).unwrap_err().to_string();
        assert!(err.contains("RetroArch's image viewer"), "{err}");
    }
}
