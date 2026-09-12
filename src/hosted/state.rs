//! Decoding a RetroArch save state to the bytes a hosted core's
//! `retro_unserialize` takes: the `#RZIPv` chunked-zlib container and the
//! `RASTATE` block container RetroArch wraps them in.

use anyhow::{Context, Result, bail};

/// Unwrap a RetroArch state file to the bytes `retro_unserialize` takes.
/// Handles the `#RZIPv` + version + `#` chunked-zlib container (version 1
/// only), the `RASTATE` + version block container, both together, or
/// neither (old files are raw core data).
pub fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    let plain = if bytes.len() >= 20 && &bytes[..6] == b"#RZIPv" && bytes[7] == b'#' {
        if bytes[6] != 1 {
            bail!(
                "rzip container version {} is not supported (only 1)",
                bytes[6]
            );
        }
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
const MAX_STATE_BYTES: u64 = 256 * 1024 * 1024;

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
        // Inflate at most one byte past what the header still allows, so
        // a chunk that lies about its size cannot allocate far beyond the
        // cap before the length check below notices.
        let room = (total - out.len() + 1) as u64;
        flate2::read::ZlibDecoder::new(chunk)
            .take(room)
            .read_to_end(&mut out)
            .with_context(|| format!("inflating the rzip chunk at byte {pos}"))?;
        if out.len() > total {
            bail!("an rzip chunk inflates past the {total} bytes the header promises");
        }
    }
    if out.len() != total {
        bail!("rzip header promises {total} bytes, got {}", out.len());
    }
    Ok(out)
}

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

    #[test]
    fn decode_passes_raw_core_data_through() {
        assert_eq!(decode(b"not a container").unwrap(), b"not a container");
    }

    #[test]
    fn decode_returns_the_mem_block_at_its_unpadded_size() {
        let s = rastate(&[(b"RPLY", b"abc"), (b"MEM ", b"0123456789")]);
        assert_eq!(decode(&s).unwrap(), b"0123456789");
    }

    #[test]
    fn decode_inflates_rzip_chunks_first() {
        let plain = rastate(&[(b"MEM ", &[7u8; 1000])]);
        let z = rzip(&plain, 300);
        assert_eq!(decode(&z).unwrap(), vec![7u8; 1000]);
    }

    #[test]
    fn decode_rejects_an_rzip_header_claiming_an_absurd_size() {
        let mut absurd = b"#RZIPv\x01#".to_vec();
        absurd.extend_from_slice(&64u32.to_le_bytes());
        absurd.extend_from_slice(&u64::MAX.to_le_bytes());
        let err = decode(&absurd).unwrap_err().to_string();
        assert!(err.contains("claims"), "{err}");
    }

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

    #[test]
    fn decode_rejects_an_unknown_rzip_version() {
        let mut v2 = b"#RZIPv\x02#".to_vec();
        v2.extend_from_slice(&64u32.to_le_bytes());
        v2.extend_from_slice(&10u64.to_le_bytes());
        let err = decode(&v2).unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
    }

    #[test]
    fn decode_rejects_a_chunk_that_inflates_past_the_promised_total() {
        use std::io::Write;
        let mut lying = b"#RZIPv\x01#".to_vec();
        lying.extend_from_slice(&4096u32.to_le_bytes());
        lying.extend_from_slice(&100u64.to_le_bytes());
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&[0u8; 1000]).unwrap();
        let z = enc.finish().unwrap();
        lying.extend_from_slice(&(z.len() as u32).to_le_bytes());
        lying.extend_from_slice(&z);
        let err = decode(&lying).unwrap_err().to_string();
        assert!(err.contains("inflates past"), "{err}");
    }
}
