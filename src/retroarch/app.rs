//! Resolving the RetroArch binary to exec from an `.app` bundle path.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolve a RetroArch `.app` bundle, or a bare executable, to the binary to exec.
pub fn resolve_binary(app: &Path) -> Result<PathBuf> {
    if app.extension().is_some_and(|e| e == "app") && app.is_dir() {
        let bin = app.join("Contents/MacOS/RetroArch");
        if bin.is_file() {
            return Ok(bin);
        }
        bail!("{} has no Contents/MacOS/RetroArch", app.display());
    }
    if app.is_file() {
        return Ok(app.to_path_buf());
    }
    bail!("RetroArch app not found at {}", app.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn app_bundle_resolves_to_inner_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("RetroArch.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/RetroArch"), b"").unwrap();
        assert_eq!(
            resolve_binary(&app).unwrap(),
            app.join("Contents/MacOS/RetroArch")
        );
    }

    #[test]
    fn bare_binary_is_returned_as_is() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("RetroArch");
        fs::write(&bin, b"").unwrap();
        assert_eq!(resolve_binary(&bin).unwrap(), bin);
    }

    #[test]
    fn app_bundle_without_binary_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("RetroArch.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        let err = resolve_binary(&app).unwrap_err().to_string();
        assert!(err.contains("Contents/MacOS/RetroArch"), "{err}");
    }

    #[test]
    fn missing_path_errors() {
        let err = resolve_binary(Path::new("/nonexistent/RetroArch.app"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
    }
}
