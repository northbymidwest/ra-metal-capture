use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Copy a save state into `states_dir` under the name RetroArch expects for
/// this ROM's slot 0 (`<rom stem>.state`). Returns that slot number.
pub fn stage(state: &Path, rom: &Path, states_dir: &Path) -> Result<u32> {
    let stem = rom
        .file_stem()
        .with_context(|| format!("ROM path {} has no file name", rom.display()))?;
    fs::create_dir_all(states_dir).with_context(|| format!("creating {}", states_dir.display()))?;
    let dest = states_dir.join(format!("{}.state", stem.to_string_lossy()));
    fs::copy(state, &dest)
        .with_context(|| format!("copying state {} to {}", state.display(), dest.display()))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_state_as_slot_zero_named_after_rom_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("anything.state3");
        fs::write(&state, b"STATE").unwrap();
        let rom = Path::new("/roms/Zelda (U) [!].gb");
        let states_dir = tmp.path().join("states");

        let slot = stage(&state, rom, &states_dir).unwrap();

        assert_eq!(slot, 0);
        let dest = states_dir.join("Zelda (U) [!].state");
        assert_eq!(fs::read(dest).unwrap(), b"STATE");
    }

    #[test]
    fn missing_state_file_errors_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = stage(
            Path::new("/nonexistent/x.state"),
            Path::new("/roms/a.gb"),
            tmp.path(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/nonexistent/x.state"), "{err}");
    }
}
