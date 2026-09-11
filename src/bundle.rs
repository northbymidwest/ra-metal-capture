//! The `.gputrace` bundle on disk: recognising one and making room for one.
//! Shared by both backends.

use anyhow::{Context, Result, bail};
use std::path::Path;

/// True when `path` looks like a bundle this tool (or Xcode) wrote: a
/// directory whose name ends in `.gputrace` and which contains an `index`.
pub fn is_gputrace_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "gputrace")
        && path.is_dir()
        && path.join("index").exists()
}

/// Make room for a new capture at `output`. A previous bundle is removed;
/// anything else that exists there is refused, so a mistyped path never
/// deletes user data.
pub fn prepare_output(output: &Path) -> Result<()> {
    if !output.exists() {
        return Ok(());
    }
    if !is_gputrace_bundle(output) {
        bail!(
            "{} exists and is not a .gputrace bundle; refusing to overwrite it",
            output.display()
        );
    }
    std::fs::remove_dir_all(output)
        .with_context(|| format!("removing stale bundle {}", output.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_a_bundle_only_with_suffix_and_index() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("a.gputrace");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("index"), b"").unwrap();
        assert!(is_gputrace_bundle(&bundle));

        let no_index = tmp.path().join("b.gputrace");
        std::fs::create_dir_all(&no_index).unwrap();
        assert!(!is_gputrace_bundle(&no_index));

        let wrong_suffix = tmp.path().join("c");
        std::fs::create_dir_all(&wrong_suffix).unwrap();
        std::fs::write(wrong_suffix.join("index"), b"").unwrap();
        assert!(!is_gputrace_bundle(&wrong_suffix));

        assert!(!is_gputrace_bundle(&tmp.path().join("missing.gputrace")));
    }

    #[test]
    fn prepare_output_refuses_a_directory_that_is_not_a_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("docs");
        std::fs::create_dir_all(&dir).unwrap();
        let err = prepare_output(&dir).unwrap_err().to_string();
        assert!(err.contains("docs"), "{err}");
        assert!(dir.exists(), "must not delete a non-bundle directory");
    }

    #[test]
    fn prepare_output_removes_a_stale_bundle_and_tolerates_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("old.gputrace");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("index"), b"").unwrap();
        prepare_output(&bundle).unwrap();
        assert!(!bundle.exists());
        prepare_output(&tmp.path().join("new.gputrace")).unwrap();
    }
}
