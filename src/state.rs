//! Staging RetroArch save states and locating them by slot. Decoding one
//! for a hosted core is `hosted::state`.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

/// Copy a save state into `states_dir` under the name RetroArch expects for
/// this ROM's slot 0 (`<rom stem>.state`).
pub fn stage(state: &Path, rom: &Path, states_dir: &Path) -> Result<()> {
    let stem = rom
        .file_stem()
        .with_context(|| format!("ROM path {} has no file name", rom.display()))?;
    fs::create_dir_all(states_dir).with_context(|| format!("creating {}", states_dir.display()))?;
    let dest = states_dir.join(format!("{}.state", stem.to_string_lossy()));
    fs::copy(state, &dest)
        .with_context(|| format!("copying state {} to {}", state.display(), dest.display()))?;
    Ok(())
}

/// Where RetroArch keeps states and how it sorts them, from `retroarch.cfg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDirs {
    pub savestate_directory: PathBuf,
    /// `sort_savestates_enable`: a subdirectory named after the core.
    pub sort_by_core: bool,
    /// `sort_savestates_by_content_enable`: a subdirectory named after the ROM's directory.
    pub sort_by_content: bool,
    /// `savestates_in_content_dir`: next to the ROM, overriding the above.
    pub in_content_dir: bool,
}

/// The file RetroArch reads for `--slot N` of this ROM under this core:
/// `<dir>/<rom stem>.state` for slot 0, `<dir>/<rom stem>.stateN` otherwise.
pub fn slot_path(dirs: &StateDirs, core_name: &str, rom: &Path, slot: u32) -> PathBuf {
    let dir = if dirs.in_content_dir {
        rom.parent().map(Path::to_path_buf).unwrap_or_default()
    } else if dirs.sort_by_core {
        dirs.savestate_directory.join(core_name)
    } else if dirs.sort_by_content {
        let content_dir = rom
            .parent()
            .and_then(Path::file_name)
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        dirs.savestate_directory.join(content_dir)
    } else {
        dirs.savestate_directory.clone()
    };
    dir.join(slot_file_name(rom, slot))
}

/// The slot file's name: `<rom stem>.state` for slot 0, `<rom stem>.stateN` otherwise.
fn slot_file_name(rom: &Path, slot: u32) -> std::ffi::OsString {
    let mut name = rom
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".state");
    if slot != 0 {
        name.push(slot.to_string());
    }
    name
}

/// Where `--slot N` of this ROM is, without knowing the core's name: next
/// to the ROM when states live in the content directory, else in the
/// states directory itself or any one of its immediate subdirectories
/// (per-core and per-content sorting both add one level). `None` when no
/// such file exists anywhere it could be.
pub fn find_slot(dirs: &StateDirs, rom: &Path, slot: u32) -> Option<PathBuf> {
    let name = slot_file_name(rom, slot);
    let mut candidates = Vec::new();
    if dirs.in_content_dir {
        candidates.push(rom.parent().map(Path::to_path_buf).unwrap_or_default());
    } else {
        candidates.push(dirs.savestate_directory.clone());
        if let Ok(entries) = fs::read_dir(&dirs.savestate_directory) {
            candidates.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    candidates
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn copies_state_as_slot_zero_named_after_rom_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("anything.state3");
        fs::write(&state, b"STATE").unwrap();
        let rom = Path::new("/roms/Zelda (U) [!].gb");
        let states_dir = tmp.path().join("states");

        stage(&state, rom, &states_dir).unwrap();

        let dest = states_dir.join("Zelda (U) [!].state");
        assert_eq!(fs::read(dest).unwrap(), b"STATE");
    }

    #[test]
    fn find_slot_searches_the_layout_and_one_level_of_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sameboy")).unwrap();
        fs::write(tmp.path().join("sameboy/game.state3"), b"").unwrap();
        fs::write(tmp.path().join("game.state"), b"").unwrap();
        let dirs = StateDirs {
            savestate_directory: tmp.path().to_path_buf(),
            sort_by_core: true,
            sort_by_content: false,
            in_content_dir: false,
        };
        let rom = Path::new("/roms/game.gbc");
        assert_eq!(
            find_slot(&dirs, rom, 3),
            Some(tmp.path().join("sameboy/game.state3"))
        );
        assert_eq!(
            find_slot(&dirs, rom, 0),
            Some(tmp.path().join("game.state"))
        );
        assert_eq!(find_slot(&dirs, rom, 4), None);
    }

    #[test]
    fn find_slot_looks_next_to_the_rom_when_states_live_in_the_content_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let rom = tmp.path().join("game.gbc");
        fs::write(tmp.path().join("game.state1"), b"").unwrap();
        let dirs = StateDirs {
            savestate_directory: PathBuf::from("/nonexistent"),
            sort_by_core: false,
            sort_by_content: false,
            in_content_dir: true,
        };
        assert_eq!(
            find_slot(&dirs, &rom, 1),
            Some(tmp.path().join("game.state1"))
        );
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

    fn dirs(sort_by_core: bool, sort_by_content: bool, in_content_dir: bool) -> StateDirs {
        StateDirs {
            savestate_directory: PathBuf::from("/states"),
            sort_by_core,
            sort_by_content,
            in_content_dir,
        }
    }

    #[test]
    fn slot_path_follows_retroarch_naming() {
        let rom = Path::new("/roms/gbc/zelda.gbc");
        assert_eq!(
            slot_path(&dirs(true, false, false), "SameBoy", rom, 0),
            PathBuf::from("/states/SameBoy/zelda.state")
        );
        assert_eq!(
            slot_path(&dirs(true, false, false), "SameBoy", rom, 3),
            PathBuf::from("/states/SameBoy/zelda.state3")
        );
        assert_eq!(
            slot_path(&dirs(false, false, false), "SameBoy", rom, 0),
            PathBuf::from("/states/zelda.state")
        );
        assert_eq!(
            slot_path(&dirs(false, true, false), "SameBoy", rom, 0),
            PathBuf::from("/states/gbc/zelda.state")
        );
        assert_eq!(
            slot_path(&dirs(true, true, true), "SameBoy", rom, 0),
            PathBuf::from("/roms/gbc/zelda.state")
        );
    }
}
