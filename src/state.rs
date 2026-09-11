//! Staging and decoding RetroArch save states, and locating them by slot.

#[cfg(feature = "librashader")]
use anyhow::bail;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

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
    let stem = rom
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
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
    let mut name = stem;
    name.push(".state");
    if slot != 0 {
        name.push(slot.to_string());
    }
    dir.join(name)
}

/// Unwrap a RetroArch state file to the bytes `retro_unserialize` takes.
/// Handles the `#RZIPv1#` chunked-zlib container, the `RASTATE1` block
/// container, both together, or neither (old files are raw core data).
#[cfg(feature = "librashader")]
pub fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    let plain = if bytes.len() >= 20 && &bytes[..6] == b"#RZIPv" && bytes[7] == b'#' {
        unrzip(bytes)?
    } else {
        bytes.to_vec()
    };
    if plain.len() >= 8 && &plain[..7] == b"RASTATE" {
        rastate_mem(&plain)
    } else {
        Ok(plain)
    }
}

#[cfg(feature = "librashader")]
fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    let s = b
        .get(at..at + 4)
        .with_context(|| format!("state truncated at byte {at}"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The largest state `unrzip` will inflate or reserve room for. The total
/// size comes straight out of the file's header, so a corrupt or hostile
/// one could otherwise ask for an allocation that aborts the process
/// instead of failing. RetroArch caps its own rzip buffers at 64 MB per
/// chunk (`rzip_stream.c`); no console's save state approaches 256 MB.
#[cfg(feature = "librashader")]
const MAX_STATE_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(feature = "librashader")]
fn unrzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let header = bytes
        .get(12..20)
        .context("rzip header truncated before its size field")?;
    let claimed = u64::from_le_bytes([
        header[0], header[1], header[2], header[3], header[4], header[5], header[6], header[7],
    ]);
    if claimed > MAX_STATE_BYTES {
        bail!("the rzip header claims {claimed} bytes, past the {MAX_STATE_BYTES}-byte limit");
    }
    let total = claimed as usize;
    let mut out = Vec::with_capacity(total);
    let mut pos = 20;
    while out.len() < total {
        let len = u32_at(bytes, pos)? as usize;
        pos += 4;
        let chunk = bytes
            .get(pos..pos + len)
            .with_context(|| format!("rzip chunk at byte {pos} runs past the end"))?;
        pos += len;
        flate2::read::ZlibDecoder::new(chunk)
            .read_to_end(&mut out)
            .with_context(|| format!("inflating the rzip chunk at byte {pos}"))?;
    }
    if out.len() != total {
        bail!("rzip header promises {total} bytes, got {}", out.len());
    }
    Ok(out)
}

#[cfg(feature = "librashader")]
fn rastate_mem(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 8;
    loop {
        let tag = bytes
            .get(pos..pos + 4)
            .with_context(|| format!("state truncated at byte {pos}"))?;
        let size = u32_at(bytes, pos + 4)? as usize;
        pos += 8;
        if tag == b"END " {
            bail!("state has no MEM block");
        }
        let payload = bytes.get(pos..pos + size).with_context(|| {
            format!(
                "block {:?} at byte {pos} declares {size} bytes past the end",
                String::from_utf8_lossy(tag)
            )
        })?;
        if tag == b"MEM " {
            return Ok(payload.to_vec());
        }
        pos += size.div_ceil(8) * 8;
    }
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

    #[cfg(feature = "librashader")]
    fn rastate(blocks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = b"RASTATE\x01".to_vec();
        for (tag, payload) in blocks {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
            out.resize(out.len() + ((8 - payload.len() % 8) % 8), 0);
        }
        out.extend_from_slice(b"END \0\0\0\0");
        out
    }

    #[cfg(feature = "librashader")]
    fn rzip(plain: &[u8], chunk: usize) -> Vec<u8> {
        use std::io::Write;
        let mut out = b"#RZIPv\x01#".to_vec();
        out.extend_from_slice(&(chunk as u32).to_le_bytes());
        out.extend_from_slice(&(plain.len() as u64).to_le_bytes());
        for piece in plain.chunks(chunk) {
            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(piece).unwrap();
            let z = enc.finish().unwrap();
            out.extend_from_slice(&(z.len() as u32).to_le_bytes());
            out.extend_from_slice(&z);
        }
        out
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn decode_passes_raw_core_data_through() {
        assert_eq!(decode(b"not a container").unwrap(), b"not a container");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn decode_returns_the_mem_block_at_its_unpadded_size() {
        let s = rastate(&[(b"RPLY", b"abc"), (b"MEM ", b"0123456789")]);
        assert_eq!(decode(&s).unwrap(), b"0123456789");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn decode_inflates_rzip_chunks_first() {
        let plain = rastate(&[(b"MEM ", &[7u8; 1000])]);
        let z = rzip(&plain, 300);
        assert_eq!(decode(&z).unwrap(), vec![7u8; 1000]);
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn decode_rejects_an_rzip_header_claiming_an_absurd_size() {
        let mut absurd = b"#RZIPv\x01#".to_vec();
        absurd.extend_from_slice(&64u32.to_le_bytes());
        absurd.extend_from_slice(&u64::MAX.to_le_bytes());
        let err = decode(&absurd).unwrap_err().to_string();
        assert!(err.contains("claims"), "{err}");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn decode_rejects_end_before_mem_and_truncation() {
        let no_mem = rastate(&[(b"RPLY", b"abc")]);
        assert!(decode(&no_mem).unwrap_err().to_string().contains("MEM"));
        let mut cut = rastate(&[(b"MEM ", b"0123456789")]);
        cut.truncate(12);
        assert!(decode(&cut).is_err());
        let mut oversize = b"RASTATE\x01MEM ".to_vec();
        oversize.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&oversize).is_err());
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
