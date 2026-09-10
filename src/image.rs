//! Validating paths for RetroArch's built-in image viewer core.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extension_check() {
        assert!(is_image_path(Path::new("a.PNG")));
        assert!(is_image_path(Path::new("b.jpeg")));
        assert!(!is_image_path(Path::new("c.gbc")));
        assert!(!is_image_path(Path::new("noext")));
    }
}
