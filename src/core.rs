//! Resolving a libretro core argument to a `.dylib` path.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolve a core argument. An existing file path is used as-is. Otherwise
/// `<libretro_dir>/<arg>` and `<libretro_dir>/<arg>_libretro.dylib` are tried.
pub fn resolve_core(arg: &str, libretro_dir: &Path) -> Result<PathBuf> {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return Ok(direct);
    }
    let candidates = [
        libretro_dir.join(arg),
        libretro_dir.join(format!("{arg}_libretro.dylib")),
    ];
    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Ok(found.clone());
    }
    bail!(
        "core {arg:?} not found; tried {} and {}",
        candidates[0].display(),
        candidates[1].display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn existing_path_is_used_directly() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("x_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(
            resolve_core(core.to_str().unwrap(), Path::new("/nowhere")).unwrap(),
            core
        );
    }

    #[test]
    fn bare_name_gets_libretro_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("sameboy_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(resolve_core("sameboy", tmp.path()).unwrap(), core);
    }

    #[test]
    fn file_name_in_dir_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("sameboy_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(
            resolve_core("sameboy_libretro.dylib", tmp.path()).unwrap(),
            core
        );
    }

    #[test]
    fn unknown_core_errors_listing_what_was_tried() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_core("nope", tmp.path()).unwrap_err().to_string();
        assert!(err.contains("nope_libretro.dylib"), "{err}");
    }
}
