# libretro Core Hosting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `--backend librashader --core CORE --rom ROM [--state FILE | --slot N]` hosts the libretro core in-process, restores the RetroArch state, runs frames through the preset with librashader, and records the trace with MTLCaptureManager, with no RetroArch process.

**Architecture:** A new feature-gated `libretro` module wraps `libretro-sys` and `libloading`: `Core` loads the dylib, installs six `extern "C"` callbacks backed by process-wide statics, and yields one BGRA8 frame per `retro_run`. `render::run` takes a `FrameSource` (image or core) plus a warm-up count, so the render loop is shared. `state::decode` unwraps RetroArch's RZIP and RASTATE containers; `config::read_all` reads the per-core options file so the core renders as RetroArch would.

**Tech Stack:** libretro-sys 0.1, libloading 0.9, flate2 1 (new, behind the `librashader` feature); existing librashader, objc2-metal, image, clap, anyhow.

**Spec:** `docs/superpowers/specs/2026-09-10-libretro-core-hosting-design.md`

## Global Constraints

- `unsafe` only in `src/render/` and `src/libretro/`, every block with a `// SAFETY:` comment. Hand-written C ABI (the `extern "C"` callbacks) only in `src/libretro/`. Metal stays objc2-only.
- The libretro callbacks never unwind: no `?`, no `unwrap` on a mutex (use `lock().unwrap_or_else(|e| e.into_inner())`), no panic paths.
- ASCII only, no em dashes or en dashes (`scripts/check-ascii.sh`).
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, and `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps` clean in both feature shapes after every task. `cargo test` never loads a core or constructs a Metal device.
- Never write the user's `retroarch.cfg`, states, saves, or system directories.
- Commit trailer: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Branch from `main` at the commit that adds this plan, in a worktree.

## File map

| file | change | responsibility |
|---|---|---|
| `Cargo.toml`, `deny.toml` | modify | three optional deps under `librashader`; allow `ISC` |
| `src/state.rs` | modify | `decode`, `slot_path`, `StateDirs` |
| `src/config.rs` | modify | `read_all` |
| `src/libretro/pixels.rs` | create | pure pixel conversion |
| `src/libretro/env.rs` | create | statics and the six callbacks |
| `src/libretro/mod.rs` | create | `Core`, `Context`, `AvInfo`, `Frame`, `FrameSource` impl |
| `src/render/mod.rs` | modify | `FrameSource`, `ImageSource`, `RenderOptions.source` and `warmup`, the two-phase loop |
| `src/lib.rs` | modify | `pub mod libretro` behind the feature |
| `src/main.rs` | modify | clap rules, `run_librashader` core path |
| `README.md`, `CHANGELOG.md`, `CLAUDE.md`, spec | modify | docs, rule amendment, recorded run |

---

### Task 1: `state::decode` and `state::slot_path`

**Files:**
- Modify: `Cargo.toml`, `deny.toml`, `src/state.rs`

**Interfaces:**
- Produces: `pub fn decode(bytes: &[u8]) -> Result<Vec<u8>>`, `pub struct StateDirs { pub savestate_directory: PathBuf, pub sort_by_core: bool, pub sort_by_content: bool, pub in_content_dir: bool }`, `pub fn slot_path(dirs: &StateDirs, core_name: &str, rom: &Path, slot: u32) -> PathBuf`. Task 5 consumes all three.

- [ ] **Step 1: Dependencies**

In `Cargo.toml` `[dependencies]`, after the `image` line:

```toml
libretro-sys = { version = "0.1.1", optional = true }
libloading = { version = "0.9", optional = true }
flate2 = { version = "1", optional = true }
```

and extend the feature: `librashader = ["dep:librashader", "dep:objc2", "dep:objc2-metal", "dep:image", "dep:libretro-sys", "dep:libloading", "dep:flate2"]`.

In `deny.toml`, add `"ISC",` to the `allow` list with a comment line above it: `# ISC is libloading.` `cargo deny check licenses bans sources` must pass.

`state::decode` uses `flate2`, so the whole `decode` function and its tests are `#[cfg(feature = "librashader")]`. `slot_path` and `StateDirs` are unconditional.

- [ ] **Step 2: Write the failing tests**

Append to the `tests` module in `src/state.rs`:

```rust
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
```

Add `use std::path::PathBuf;` to the test module imports if it lacks it.

- [ ] **Step 3: Run to see them fail**

Run: `cargo test --lib state`
Expected: compile errors for `decode`, `StateDirs`, `slot_path`.

- [ ] **Step 4: Implement**

Add to `src/state.rs` (module doc becomes "Staging and decoding RetroArch save states, and locating them by slot."):

```rust
use std::path::PathBuf;

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
    let stem = rom.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
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

#[cfg(feature = "librashader")]
fn unrzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let total = u64::from_le_bytes(bytes[12..20].try_into().expect("8 bytes")) as usize;
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
        let payload = bytes
            .get(pos..pos + size)
            .with_context(|| format!("block {:?} at byte {pos} declares {size} bytes past the end", String::from_utf8_lossy(tag)))?;
        if tag == b"MEM " {
            return Ok(payload.to_vec());
        }
        pos += size.div_ceil(8) * 8;
    }
}
```

`use anyhow::{Context, Result, bail};` at the top (extend the existing import).

- [ ] **Step 5: Verify both shapes**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test --no-default-features && cargo clippy --no-default-features --all-targets -- -D warnings && cargo deny check licenses bans sources`
Expected: all pass. The three new crates are unused until Task 2; `cargo machete` reports `libretro-sys` and `libloading` until then, which is expected at this commit.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock deny.toml src/state.rs
git commit -m "Decode RetroArch RZIP and RASTATE state files and resolve slot paths

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `libretro::pixels` and `config::read_all`

**Files:**
- Create: `src/libretro/pixels.rs`, `src/libretro/mod.rs` (declaration only this task)
- Modify: `src/config.rs`, `src/lib.rs`

**Interfaces:**
- Produces: `pub fn to_bgra(format: PixelFormat, data: &[u8], width: u32, height: u32, pitch: usize) -> Vec<u8>` in `libretro::pixels`; `pub fn read_all(text: &str) -> HashMap<String, String>` in `config`. Task 3 and Task 5 consume them.

- [ ] **Step 1: Write the failing tests**

`src/libretro/pixels.rs` (whole file, tests included):

```rust
//! Converting a core's framebuffer to tightly packed BGRA8, the layout the
//! render module uploads. Pure; the video-refresh callback calls it.

use libretro_sys::PixelFormat;

fn expand5(v: u16) -> u8 {
    let v = (v & 0x1f) as u8;
    v << 3 | v >> 2
}

fn expand6(v: u16) -> u8 {
    let v = (v & 0x3f) as u8;
    v << 2 | v >> 4
}

/// `width * height * 4` bytes of BGRA8 from `height` rows of `pitch` bytes.
/// XRGB8888 rows are copied with the X byte forced opaque; the 16-bit
/// formats expand each channel by bit replication.
pub fn to_bgra(format: PixelFormat, data: &[u8], width: u32, height: u32, pitch: usize) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * pitch..];
        match format {
            PixelFormat::ARGB8888 => {
                for px in row[..w * 4].chunks_exact(4) {
                    out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
                }
            }
            PixelFormat::RGB565 => {
                for px in row[..w * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    out.extend_from_slice(&[expand5(v), expand6(v >> 5), expand5(v >> 11), 0xff]);
                }
            }
            PixelFormat::ARGB1555 => {
                for px in row[..w * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    out.extend_from_slice(&[expand5(v), expand5(v >> 5), expand5(v >> 10), 0xff]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrgb8888_copies_bgr_and_forces_alpha() {
        // Two rows of one pixel, pitch 8 (4 bytes of padding per row).
        let data = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        assert_eq!(
            to_bgra(PixelFormat::ARGB8888, &data, 1, 2, 8),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn rgb565_expands_channels() {
        // Pure red 0xF800, pure green 0x07E0, pure blue 0x001F.
        let data = [0x00, 0xF8, 0xE0, 0x07, 0x1F, 0x00];
        assert_eq!(
            to_bgra(PixelFormat::RGB565, &data, 3, 1, 6),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn argb1555_expands_channels() {
        // Pure red 0x7C00, pure green 0x03E0, pure blue 0x001F.
        let data = [0x00, 0x7C, 0xE0, 0x03, 0x1F, 0x00];
        assert_eq!(
            to_bgra(PixelFormat::ARGB1555, &data, 3, 1, 6),
            [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }
}
```

`libretro-sys` names XRGB8888 `PixelFormat::ARGB8888` and 0RGB1555 `PixelFormat::ARGB1555`; confirm against `~/.cargo/registry/src/*/libretro-sys-0.1.1/src/lib.rs` around the `pub enum PixelFormat` definition and use its exact variant names.

`src/libretro/mod.rs` for this task:

```rust
//! Hosting a libretro core in this process: loading the dylib, answering
//! its environment queries, and turning each `retro_run` into a BGRA8
//! frame for the render module. The libretro ABI carries no user pointer,
//! so the callbacks' state lives in process-wide statics (`env`), and one
//! `Core` per process is enforced. This module and `render` are the only
//! ones that allow `unsafe`; this one is also the only place with a
//! hand-written C ABI (the six callbacks a core calls back into).
#![allow(unsafe_code)]

pub mod pixels;
```

`src/lib.rs`: after the `render` declaration add

```rust
#[cfg(feature = "librashader")]
pub mod libretro;
```

`src/config.rs` tests, append:

```rust
    #[test]
    fn read_all_returns_every_key() {
        let text = "# comment\nsameboy_model = \"Auto\"\n\nsameboy_border = \"Never\"\nunquoted = 3\n";
        let m = read_all(text);
        assert_eq!(m.len(), 3);
        assert_eq!(m["sameboy_model"], "Auto");
        assert_eq!(m["sameboy_border"], "Never");
        assert_eq!(m["unquoted"], "3");
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --lib`
Expected: `read_all` not found; the pixels tests compile only once the module exists, so create the files first and expect `read_all` to be the failing symbol.

- [ ] **Step 3: Implement `read_all`**

In `src/config.rs`, refactor `read_keys` so both share one parser:

```rust
/// Every `key = "value"` pair in RetroArch config text. Comments and blank
/// lines are skipped; surrounding quotes are stripped.
pub fn read_all(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// The named keys from RetroArch `key = "value"` config text.
/// Missing keys are simply absent from the map.
pub fn read_keys(text: &str, keys: &[&str]) -> HashMap<String, String> {
    let mut all = read_all(text);
    all.retain(|k, _| keys.contains(&k.as_str()));
    all
}
```

- [ ] **Step 4: Verify both shapes**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test --no-default-features && cargo clippy --no-default-features --all-targets -- -D warnings`
Expected: all pass; the existing `read_keys` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/libretro/mod.rs src/libretro/pixels.rs src/lib.rs src/config.rs
git commit -m "Add libretro pixel conversion and a whole-file config reader

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `libretro::Core` and the environment callbacks

**Files:**
- Create: `src/libretro/env.rs`
- Modify: `src/libretro/mod.rs`

**Interfaces:**
- Consumes: `pixels::to_bgra` (Task 2).
- Produces: `Core`, `Context`, `AvInfo`, `Frame` as in the spec, and `Core: FrameSource` in Task 4 (the trait lands there; this task's `Core` exposes `run_frame`). Task 5 consumes `Core::load`, `system_info`, `load_game`, `restore`.

No unit test can load a core. The deliverable is that both shapes compile and lint clean, `cargo machete` is clean, and the design invariants hold on reading: callbacks never unwind, frames are copied inside the callback, one core per process.

- [ ] **Step 1: `src/libretro/env.rs`**

```rust
//! The frontend state a core reaches through its callbacks, and the six
//! `extern "C"` callbacks themselves. libretro passes no user pointer, so
//! this is process-wide; `Core::load` claims it and `Drop` releases it.

use super::pixels::to_bgra;
use crate::config::Size;
use libretro_sys::{PixelFormat, Variable};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::sync::{Mutex, MutexGuard};

/// One emulated frame, tightly packed BGRA8, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub size: Size,
    pub bgra: Vec<u8>,
}

/// What the callbacks read and write.
#[derive(Default)]
pub struct Shared {
    /// Set by `Core::load`; a second load while this is true is refused.
    pub claimed: bool,
    pub system_dir: Option<CString>,
    pub save_dir: Option<CString>,
    pub options: HashMap<String, CString>,
    pub pixel_format: PixelFormat,
    pub asked_for_hw_render: bool,
    pub frame: Option<Frame>,
}

impl Default for PixelFormat {
    fn default() -> Self {
        // libretro's default when a core never sets one.
        PixelFormat::ARGB1555
    }
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    claimed: false,
    system_dir: None,
    save_dir: None,
    options: HashMap::new(),
    pixel_format: PixelFormat::ARGB1555,
    asked_for_hw_render: false,
    frame: None,
});

/// The shared state, recovering from a poisoned lock so a callback can
/// never panic on entry.
pub fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The environment callback. Answers exactly what a software-rendered core
/// needs and `false` to everything else; see the spec's table.
pub unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    use libretro_sys::*;
    // The experimental and private flag bits do not change the command.
    let cmd = cmd & 0xffff;
    let mut s = shared();
    // SAFETY: each arm casts `data` to the type libretro documents for that
    // command and only when that command was requested; every pointer the
    // core hands over is valid for the duration of the call.
    unsafe {
        match cmd {
            ENVIRONMENT_GET_CAN_DUPE => {
                *(data as *mut bool) = true;
                true
            }
            ENVIRONMENT_GET_SYSTEM_DIRECTORY => match &s.system_dir {
                Some(d) => {
                    *(data as *mut *const c_char) = d.as_ptr();
                    true
                }
                None => false,
            },
            ENVIRONMENT_GET_SAVE_DIRECTORY => match &s.save_dir {
                Some(d) => {
                    *(data as *mut *const c_char) = d.as_ptr();
                    true
                }
                None => false,
            },
            ENVIRONMENT_SET_PIXEL_FORMAT => {
                let raw = *(data as *const c_uint);
                let format = match raw {
                    0 => PixelFormat::ARGB1555,
                    1 => PixelFormat::ARGB8888,
                    2 => PixelFormat::RGB565,
                    _ => return false,
                };
                s.pixel_format = format;
                true
            }
            ENVIRONMENT_SET_VARIABLES
            | ENVIRONMENT_SET_CORE_OPTIONS
            | ENVIRONMENT_SET_CORE_OPTIONS_INTL => true,
            ENVIRONMENT_GET_VARIABLE => {
                let var = &mut *(data as *mut Variable);
                if var.key.is_null() {
                    return false;
                }
                let key = CStr::from_ptr(var.key).to_string_lossy();
                match s.options.get(key.as_ref()) {
                    Some(v) => {
                        var.value = v.as_ptr();
                        true
                    }
                    None => {
                        var.value = std::ptr::null();
                        false
                    }
                }
            }
            ENVIRONMENT_GET_VARIABLE_UPDATE => {
                *(data as *mut bool) = false;
                true
            }
            ENVIRONMENT_SET_HW_RENDER => {
                s.asked_for_hw_render = true;
                false
            }
            _ => false,
        }
    }
}

pub unsafe extern "C" fn video_refresh(data: *const c_void, width: c_uint, height: c_uint, pitch: usize) {
    if data.is_null() {
        // A dupe: the core is telling us the previous frame still stands.
        return;
    }
    let mut s = shared();
    let rows = height as usize;
    // SAFETY: libretro guarantees `data` points at `height` rows of `pitch`
    // bytes for the duration of this call; the slice is only read here.
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, rows * pitch) };
    let bgra = to_bgra(s.pixel_format, bytes, width, height, pitch);
    s.frame = Some(Frame {
        size: Size { width, height },
        bgra,
    });
}

pub unsafe extern "C" fn audio_sample(_left: i16, _right: i16) {}

pub unsafe extern "C" fn audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    frames
}

pub unsafe extern "C" fn input_poll() {}

pub unsafe extern "C" fn input_state(_port: c_uint, _device: c_uint, _index: c_uint, _id: c_uint) -> i16 {
    0
}
```

If `libretro-sys` 0.1.1 lacks `ENVIRONMENT_SET_CORE_OPTIONS` (53) or `ENVIRONMENT_SET_CORE_OPTIONS_INTL` (54), define them as local `const` items with those values and a comment naming `libretro.h`. `impl Default for PixelFormat` is only legal if `libretro-sys` does not already provide one and the orphan rule allows it; it does not (foreign trait on foreign type), so instead keep the static's initialiser explicit as shown and remove that `impl` block. The `#[derive(Default)]` on `Shared` then needs replacing with a manual `Default` or, simpler, drop the derive: the static is the only instance.

- [ ] **Step 2: `src/libretro/mod.rs`**

Replace the file with:

```rust
//! Hosting a libretro core in this process: loading the dylib, answering
//! its environment queries, and turning each `retro_run` into a BGRA8
//! frame for the render module. The libretro ABI carries no user pointer,
//! so the callbacks' state lives in process-wide statics (`env`), and one
//! `Core` per process is enforced. This module and `render` are the only
//! ones that allow `unsafe`; this one is also the only place with a
//! hand-written C ABI (the six callbacks a core calls back into).
#![allow(unsafe_code)]

mod env;
pub mod pixels;

pub use env::Frame;

use crate::config::Size;
use anyhow::{Context as _, Result, bail};
use libretro_sys::{CoreAPI, GameInfo, SystemAvInfo, SystemInfo as RawSystemInfo};
use libloading::Library;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};

/// What the core is told about its surroundings.
pub struct Context {
    /// `system_directory`: BIOS and boot ROMs.
    pub system_dir: PathBuf,
    /// Where the core may write saves. A temp dir; nothing there is kept.
    pub save_dir: PathBuf,
    /// Core options, key to value, from RetroArch's per-core `.opt` file.
    pub options: HashMap<String, String>,
}

/// `retro_get_system_info`, the parts this tool uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    /// The core's own name, which RetroArch uses for per-core directories.
    pub library_name: String,
    pub need_fullpath: bool,
}

/// `retro_get_system_av_info`, the parts this tool uses.
#[derive(Debug, Clone, PartialEq)]
pub struct AvInfo {
    pub base: Size,
    pub max: Size,
    pub aspect_ratio: f32,
    pub fps: f64,
}

/// A loaded core. Dropping it unloads the game and deinitialises the core.
pub struct Core {
    api: CoreAPI,
    // Held for the lifetime of `api`'s function pointers; never read.
    _lib: Library,
    loaded: bool,
}

/// Resolve every `retro_*` symbol into a `CoreAPI`.
///
/// SAFETY: each symbol is looked up by the name libretro.h gives it and
/// cast to the signature libretro.h declares for it.
unsafe fn resolve(lib: &Library) -> Result<CoreAPI> {
    macro_rules! sym {
        ($name:literal) => {{
            let s = unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                .with_context(|| format!("core lacks {}", $name))?;
            *s
        }};
    }
    Ok(CoreAPI {
        retro_set_environment: sym!("retro_set_environment"),
        retro_set_video_refresh: sym!("retro_set_video_refresh"),
        retro_set_audio_sample: sym!("retro_set_audio_sample"),
        retro_set_audio_sample_batch: sym!("retro_set_audio_sample_batch"),
        retro_set_input_poll: sym!("retro_set_input_poll"),
        retro_set_input_state: sym!("retro_set_input_state"),
        retro_init: sym!("retro_init"),
        retro_deinit: sym!("retro_deinit"),
        retro_api_version: sym!("retro_api_version"),
        retro_get_system_info: sym!("retro_get_system_info"),
        retro_get_system_av_info: sym!("retro_get_system_av_info"),
        retro_set_controller_port_device: sym!("retro_set_controller_port_device"),
        retro_reset: sym!("retro_reset"),
        retro_run: sym!("retro_run"),
        retro_serialize_size: sym!("retro_serialize_size"),
        retro_serialize: sym!("retro_serialize"),
        retro_unserialize: sym!("retro_unserialize"),
        retro_cheat_reset: sym!("retro_cheat_reset"),
        retro_cheat_set: sym!("retro_cheat_set"),
        retro_load_game: sym!("retro_load_game"),
        retro_load_game_special: sym!("retro_load_game_special"),
        retro_unload_game: sym!("retro_unload_game"),
        retro_get_region: sym!("retro_get_region"),
        retro_get_memory_data: sym!("retro_get_memory_data"),
        retro_get_memory_size: sym!("retro_get_memory_size"),
    })
}

fn cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("{} contains a NUL byte", path.display()))
}

impl Core {
    /// dlopen `dylib`, resolve its API, install the callbacks, and call
    /// `retro_init`. Only one `Core` may exist per process.
    pub fn load(dylib: &Path, ctx: Context) -> Result<Core> {
        {
            let mut s = env::shared();
            if s.claimed {
                bail!("a libretro core is already loaded in this process");
            }
            s.claimed = true;
            s.system_dir = Some(cstring(&ctx.system_dir)?);
            s.save_dir = Some(cstring(&ctx.save_dir)?);
            s.options = ctx
                .options
                .into_iter()
                .filter_map(|(k, v)| CString::new(v).ok().map(|v| (k, v)))
                .collect();
            s.frame = None;
            s.asked_for_hw_render = false;
        }
        // SAFETY: loading a libretro core runs its constructors, which the
        // API contract keeps side-effect free until retro_init.
        let lib = unsafe { Library::new(dylib) }
            .with_context(|| format!("loading core {}", dylib.display()))?;
        // SAFETY: see `resolve`.
        let api = unsafe { resolve(&lib) }?;
        // SAFETY: the libretro contract: set every callback, then init.
        unsafe {
            (api.retro_set_environment)(env::environment);
            (api.retro_set_video_refresh)(env::video_refresh);
            (api.retro_set_audio_sample)(env::audio_sample);
            (api.retro_set_audio_sample_batch)(env::audio_sample_batch);
            (api.retro_set_input_poll)(env::input_poll);
            (api.retro_set_input_state)(env::input_state);
            (api.retro_init)();
        }
        Ok(Core {
            api,
            _lib: lib,
            loaded: false,
        })
    }

    pub fn system_info(&self) -> SystemInfo {
        let mut raw = RawSystemInfo {
            library_name: std::ptr::null(),
            library_version: std::ptr::null(),
            valid_extensions: std::ptr::null(),
            need_fullpath: false,
            block_extract: false,
        };
        // SAFETY: the core fills the struct with pointers to static strings.
        unsafe { (self.api.retro_get_system_info)(&mut raw) };
        let name = if raw.library_name.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null library_name is a NUL-terminated static string.
            unsafe { CStr::from_ptr(raw.library_name) }.to_string_lossy().into_owned()
        };
        SystemInfo {
            library_name: name,
            need_fullpath: raw.need_fullpath,
        }
    }

    /// `retro_load_game` with the ROM's bytes and path, then the AV info.
    pub fn load_game(&mut self, rom: &Path) -> Result<AvInfo> {
        if rom.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            bail!(
                "{} is a zip; this backend takes the extracted ROM (RetroArch extracts archives itself)",
                rom.display()
            );
        }
        let bytes = std::fs::read(rom).with_context(|| format!("reading {}", rom.display()))?;
        let path = cstring(rom)?;
        let info = GameInfo {
            path: path.as_ptr(),
            data: bytes.as_ptr() as *const c_void,
            size: bytes.len(),
            meta: std::ptr::null(),
        };
        // SAFETY: `info`'s pointers outlive the call; the core copies what it keeps.
        let ok = unsafe { (self.api.retro_load_game)(&info) };
        if !ok {
            let hw = env::shared().asked_for_hw_render;
            if hw {
                bail!(
                    "the core asked for hardware rendering, which this backend does not provide; only software-rendered cores are supported"
                );
            }
            bail!("the core refused {}", rom.display());
        }
        self.loaded = true;
        let mut av = SystemAvInfo {
            geometry: libretro_sys::GameGeometry {
                base_width: 0,
                base_height: 0,
                max_width: 0,
                max_height: 0,
                aspect_ratio: 0.0,
            },
            timing: libretro_sys::SystemTiming {
                fps: 0.0,
                sample_rate: 0.0,
            },
        };
        // SAFETY: valid after a successful retro_load_game.
        unsafe { (self.api.retro_get_system_av_info)(&mut av) };
        Ok(AvInfo {
            base: Size {
                width: av.geometry.base_width,
                height: av.geometry.base_height,
            },
            max: Size {
                width: av.geometry.max_width,
                height: av.geometry.max_height,
            },
            aspect_ratio: av.geometry.aspect_ratio,
            fps: av.timing.fps,
        })
    }

    /// `retro_unserialize`. The message on failure names both sizes, since
    /// a size mismatch is the usual cause (wrong core version or ROM).
    pub fn restore(&mut self, state: &[u8]) -> Result<()> {
        // SAFETY: `state` is a valid slice for the call.
        let ok = unsafe { (self.api.retro_unserialize)(state.as_ptr() as *const c_void, state.len()) };
        if !ok {
            // SAFETY: valid after load_game.
            let expected = unsafe { (self.api.retro_serialize_size)() };
            bail!(
                "the core rejected the save state ({} bytes; the core serializes {expected} bytes)",
                state.len()
            );
        }
        Ok(())
    }

    /// Run one emulated frame and return what the core drew. On a dupe the
    /// previous frame is returned; before any frame has arrived it is an error.
    pub fn run_frame(&mut self) -> Result<Frame> {
        // SAFETY: valid after load_game; the callbacks copy everything out.
        unsafe { (self.api.retro_run)() };
        env::shared()
            .frame
            .clone()
            .context("the core ran a frame but delivered no video")
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: mirrors load: unload if loaded, then deinit, once.
        unsafe {
            if self.loaded {
                (self.api.retro_unload_game)();
            }
            (self.api.retro_deinit)();
        }
        let mut s = env::shared();
        s.claimed = false;
        s.frame = None;
    }
}
```

Check `libretro-sys`'s `SystemInfo` field names and the exact `Library::get` signature (`get<T>(&self, symbol: &[u8]) -> Result<Symbol<T>>`; dereferencing the `Symbol` yields the fn pointer). `run_frame` returns an owned `Frame` (a clone of the shared one) rather than a borrow, which keeps the `MutexGuard` out of the public API; at core resolutions the copy is negligible.

- [ ] **Step 3: Verify both shapes**

Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test && RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`, then the same four with `--no-default-features`, then `cargo machete` (now clean) and `./scripts/check-ascii.sh`.
Expected: all pass. Fix compile errors against the crate source, and record each adjustment in the commit body.

- [ ] **Step 4: Commit**

```bash
git add src/libretro/mod.rs src/libretro/env.rs
git commit -m "Host a libretro core in-process behind libretro-sys and libloading

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `render::FrameSource` and the two-phase loop

**Files:**
- Modify: `src/render/mod.rs`, `src/libretro/mod.rs`, `src/main.rs` (only the `RenderOptions` construction)

**Interfaces:**
- Produces: `pub trait FrameSource { fn size(&self) -> Size; fn next(&mut self) -> Result<Vec<u8>>; }`, `pub struct ImageSource` with `pub fn open(path: &Path) -> Result<ImageSource>`, `RenderOptions { source: Box<dyn FrameSource>, preset, window, screen, warmup: u32, frames, output, verbose }`, and `impl FrameSource for libretro::Core`. Task 5 consumes them.

- [ ] **Step 1: Write the failing test**

Append to `src/render/mod.rs` tests:

```rust
    #[test]
    fn image_source_yields_the_same_frame_every_time() {
        let mut src = ImageSource::open(Path::new("sample.png")).unwrap();
        assert_eq!(
            src.size(),
            Size {
                width: 160,
                height: 144
            }
        );
        let a = src.next().unwrap();
        let b = src.next().unwrap();
        assert_eq!(a.len(), 160 * 144 * 4);
        assert_eq!(a, b);
        assert_eq!(a[3], 0xff, "opaque alpha");
    }
```

with `use std::path::Path;` in the test module.

- [ ] **Step 2: Run to see it fail**

Run: `cargo test --lib render`
Expected: `ImageSource` not found.

- [ ] **Step 3: Implement**

In `src/render/mod.rs`:

Replace `decode_bgra` and the `image` field with:

```rust
/// Where frames come from: a decoded image (the same bytes forever) or a
/// running libretro core (one emulated frame per call).
pub trait FrameSource {
    /// Size of every frame this source yields.
    fn size(&self) -> Size;
    /// The next frame as tightly packed BGRA8 rows, top row first,
    /// exactly `size().width * size().height * 4` bytes.
    fn next(&mut self) -> Result<Vec<u8>>;
}

/// A static image, decoded once.
pub struct ImageSource {
    size: Size,
    bgra: Vec<u8>,
}

impl ImageSource {
    /// Decode `path` with the `image` crate into BGRA8.
    pub fn open(path: &Path) -> Result<ImageSource> {
        let img = image::open(path)
            .with_context(|| format!("decoding {}", path.display()))?
            .into_rgba8();
        let (width, height) = img.dimensions();
        let mut bgra = img.into_raw();
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        Ok(ImageSource {
            size: Size { width, height },
            bgra,
        })
    }
}

impl FrameSource for ImageSource {
    fn size(&self) -> Size {
        self.size
    }
    fn next(&mut self) -> Result<Vec<u8>> {
        Ok(self.bgra.clone())
    }
}
```

`RenderOptions`: replace `pub image: PathBuf` with `pub source: Box<dyn FrameSource>` (doc: "Where frames come from; image mode uses `ImageSource`, core hosting a `libretro::Core`.") and add `pub warmup: u32` (doc: "Frames rendered through the chain before the capture starts, so history passes see real prior frames in the first recorded one. Image mode uses 0.").

`run`: take `opts: RenderOptions` by value (the source is mutated), and restructure the middle:

```rust
pub fn run(mut opts: RenderOptions) -> Result<()> {
    prepare_output(&opts.output)?;
    let image_size = opts.source.size();
    let size = output_size(&opts.window, image_size, &opts.screen);
    if size.width == 0 || size.height == 0 {
        bail!(...unchanged...);
    }
    if opts.verbose {
        eprintln!(
            "source {}x{} -> output {}x{} px, {} warm-up + {} recorded frame(s)",
            image_size.width, image_size.height, size.width, size.height, opts.warmup, opts.frames
        );
    }

    let device = ...; let queue = ...;
    let input = new_texture(&device, image_size, MTLTextureUsage::ShaderRead, "input")?;
    let output = new_texture(...)?;
    let mut chain = ...; let viewport = ...;

    let upload = |bytes: &[u8]| -> Result<()> {
        if bytes.len() != image_size.width as usize * image_size.height as usize * 4 {
            bail!("frame source yielded {} bytes for {}x{}", bytes.len(), image_size.width, image_size.height);
        }
        let region = ...as before...;
        let pixels = NonNull::new(bytes.as_ptr().cast_mut().cast()).context("frame buffer is null")?;
        // SAFETY: `bytes` holds exactly width * height * 4 bytes of BGRA8 with rows of width * 4 bytes (checked above), matching `region` and the row stride, and Metal copies them before returning.
        unsafe { input.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, pixels, image_size.width as usize * 4) };
        Ok(())
    };
    let mut render_one = |frame_count: usize| -> Result<()> {
        let cmd = queue.commandBuffer().context("creating a Metal command buffer")?;
        chain.frame(&input, &viewport, &cmd, frame_count, None).with_context(|| format!("rendering frame {frame_count}"))?;
        cmd.commit();
        cmd.waitUntilCompleted();
        Ok(())
    };

    let mut count = 0usize;
    for _ in 0..opts.warmup {
        upload(&opts.source.next()?)?;
        render_one(count)?;
        count += 1;
    }
    let trace = Trace::start(&device, &opts.output)?;
    for _ in 0..opts.frames {
        upload(&opts.source.next()?)?;
        render_one(count)?;
        count += 1;
    }
    trace.finish();
    ...index check unchanged...
}
```

If the borrow checker objects to the two closures borrowing `input`, `chain`, and `queue` at once, inline them as two small `fn`s taking those as parameters. Keep the existing SAFETY comments' substance.

In `src/libretro/mod.rs` add:

```rust
impl crate::render::FrameSource for Core {
    fn size(&self) -> Size {
        self.base
    }
    fn next(&mut self) -> Result<Vec<u8>> {
        let frame = self.run_frame()?;
        if frame.size != self.base {
            bail!(
                "the core changed its frame size to {}x{} (base geometry is {}x{}); mid-run geometry changes are not supported",
                frame.size.width, frame.size.height, self.base.width, self.base.height
            );
        }
        Ok(frame.bgra)
    }
}
```

which needs `Core` to remember `base: Size` from `load_game` (add the field, set it there, and make `run_frame`'s doc say the size must match). A `Core` that has not loaded a game has `base` zero and `next` fails on the size check.

In `src/main.rs`, `run_librashader` now builds `render::RenderOptions { source: Box::new(render::ImageSource::open(image)?), preset, window, screen, warmup: 0, frames, output, verbose }` and calls `render::run(opts)`.

- [ ] **Step 4: Verify both shapes**

Run: the full check list from Task 3 Step 3.
Expected: all pass, including the new `image_source` test and the existing `output_size` tests.

- [ ] **Step 5: Commit**

```bash
git add src/render/mod.rs src/libretro/mod.rs src/main.rs
git commit -m "Feed the render loop from a frame source with warm-up frames

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: CLI: `--backend librashader --core`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: the user-facing behaviour in the spec's CLI section.

- [ ] **Step 1: Write the failing tests**

Append to `src/main.rs` tests:

```rust
    #[test]
    fn librashader_accepts_core_and_rom_without_image() {
        let cli = parse_raw(&["--backend", "librashader", "--core", "c", "--rom", "r", "--shader", "p"]).unwrap();
        assert_eq!(cli.backend, Backend::Librashader);
        assert!(cli.image.is_none());
        assert!(parse_raw(&["--backend", "librashader", "--core", "c", "--rom", "r", "--image", "i"]).is_err());
        assert!(parse_raw(&["--backend", "librashader", "--core", "c", "--rom", "r", "--state", "s", "--slot", "1"]).is_err());
        assert!(parse_raw(&["--backend", "librashader"]).is_err(), "needs image or core+rom");
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_core_path_refuses_a_zip_before_loading_anything() {
        let cli = parse_raw(&[
            "--backend", "librashader", "--core", "/nonexistent/core.dylib",
            "--rom", "/nonexistent/game.zip", "--shader", "Cargo.toml",
        ])
        .unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("zip"), "{err}");
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --bin ra-metal-capture`
Expected: the first test fails (clap still requires `--image` with `--backend`); the second fails because the zip check does not exist yet.

- [ ] **Step 3: Implement**

`Cli`:
- `backend`: drop `requires = "image"` and the `conflicts_with_all`; add `conflicts_with_all = ["app", "cmd_port", "keep_running"]` is NOT done (those are ignored, per the spec), so the field becomes `#[arg(long, value_enum, default_value_t = Backend::Retroarch)]`.
- `core` and `rom` keep `required_unless_present = "image"`, `conflicts_with = "image"`, and their mutual `requires`.
- `image` keeps its conflicts.

`run_librashader` becomes:

```rust
#[cfg(feature = "librashader")]
fn run_librashader(cli: &Cli) -> Result<()> {
    let preset = cli.shader.clone().context(
        "--backend librashader requires --shader: there is nothing to render without a preset",
    )?;
    if !preset.is_file() {
        bail!("shader preset not found at {}", preset.display());
    }
    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let window = cli.window_mode();
    let screen = display::main_screen();

    let (source, warmup): (Box<dyn render::FrameSource>, u32) = if let Some(image) = &cli.image {
        validate_image(image)?;
        (Box::new(render::ImageSource::open(image)?), 0)
    } else {
        let rom = cli.rom.clone().context("--rom is required with --core")?;
        if rom.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            bail!(
                "{} is a zip; this backend takes the extracted ROM (RetroArch extracts archives itself)",
                rom.display()
            );
        }
        if !rom.is_file() {
            bail!("ROM not found at {}", rom.display());
        }
        let cfg = RetroArchDirs::read(&cli.config)?;
        let core_arg = cli.core.as_deref().context("--core is required without --image")?;
        let core_path = core::resolve_core(core_arg, &cfg.libretro_dir)?;
        let tmp = tempfile::Builder::new()
            .prefix("ra-metal-capture-")
            .tempdir()
            .context("creating temp dir")?;

        // Load the core first: its library name picks the options file and the slot directory.
        let mut core = libretro::Core::load(
            &core_path,
            libretro::Context {
                system_dir: cfg.system_dir.clone(),
                save_dir: tmp.path().to_path_buf(),
                options: HashMap::new(),
            },
        )?;
        let info = core.system_info();
        let options = cfg.core_options(&info.library_name)?;
        core.set_options(options);
        let av = core.load_game(&rom)?;
        if cli.verbose {
            eprintln!(
                "core {} ({}x{} at {:.3} fps)",
                info.library_name, av.base.width, av.base.height, av.fps
            );
        }

        let state_path = match (&cli.state, cli.slot) {
            (Some(p), _) => Some(p.clone()),
            (None, Some(n)) => Some(state::slot_path(&cfg.states, &info.library_name, &rom, n)),
            (None, None) => None,
        };
        let warmup = match state_path {
            Some(path) => {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading state {}", path.display()))?;
                let mem = state::decode(&bytes)
                    .with_context(|| format!("decoding state {}", path.display()))?;
                core.restore(&mem)
                    .with_context(|| format!("restoring state {}", path.display()))?;
                cli.advance - 1
            }
            None => (cli.settle * av.fps).round() as u32,
        };
        // `tmp` must outlive the core (it is the core's save dir); moving it
        // into the box alongside the core keeps it alive until render::run returns.
        (Box::new(CoreWithTemp { core, _tmp: tmp }), warmup)
    };

    let opts = render::RenderOptions {
        source,
        preset,
        window,
        screen,
        warmup,
        frames: cli.frames,
        output: output.clone(),
        verbose: cli.verbose,
    };
    render::run(opts)?;
    println!("{}", output.display());
    Ok(())
}

/// A core plus the temp dir it was told to save into.
#[cfg(feature = "librashader")]
struct CoreWithTemp {
    core: libretro::Core,
    _tmp: tempfile::TempDir,
}

#[cfg(feature = "librashader")]
impl render::FrameSource for CoreWithTemp {
    fn size(&self) -> Size {
        self.core.size()
    }
    fn next(&mut self) -> Result<Vec<u8>> {
        self.core.next()
    }
}
```

This needs three small additions:

- `libretro::Core::set_options(&mut self, options: HashMap<String, String>)`: writes the `CString` map into `env::shared()`. `Core::load` still takes `Context` for the directories; make `Context.options` the initial map (empty here) and `set_options` the way to replace it after `system_info`. Put `set_options` next to `load`.
- `RetroArchDirs` in `src/main.rs`:

```rust
/// The RetroArch config keys the librashader core path reads.
#[cfg(feature = "librashader")]
struct RetroArchDirs {
    libretro_dir: PathBuf,
    system_dir: PathBuf,
    config_dir: PathBuf,
    states: state::StateDirs,
}

#[cfg(feature = "librashader")]
impl RetroArchDirs {
    fn read(path: &Path) -> Result<RetroArchDirs> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let keys = config::read_all(&text);
        let dir = |key: &str, default: &str| {
            keys.get(key)
                .map(|s| config::expand_tilde(s))
                .unwrap_or_else(|| config::expand_tilde(default))
        };
        let flag = |key: &str, default: bool| keys.get(key).map(|v| v == "true").unwrap_or(default);
        Ok(RetroArchDirs {
            libretro_dir: dir("libretro_directory", "~/Library/Application Support/RetroArch/cores"),
            system_dir: dir("system_directory", "~/Library/Application Support/RetroArch/system"),
            config_dir: dir("rgui_config_directory", "~/Library/Application Support/RetroArch/config"),
            states: state::StateDirs {
                savestate_directory: dir("savestate_directory", "~/Library/Application Support/RetroArch/states"),
                sort_by_core: flag("sort_savestates_enable", true),
                sort_by_content: flag("sort_savestates_by_content_enable", false),
                in_content_dir: flag("savestates_in_content_dir", false),
            },
        })
    }

    /// `<config dir>/<core name>/<core name>.opt`, or empty when absent.
    fn core_options(&self, library_name: &str) -> Result<HashMap<String, String>> {
        let path = self.config_dir.join(library_name).join(format!("{library_name}.opt"));
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(config::read_all(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e).with_context(|| format!("reading core options {}", path.display())),
        }
    }
}
```

  The RetroArch path in `run` keeps its own `libretro_directory` read as today; do not merge the two, the RetroArch path has no use for the rest.
- `Core::size(&self) -> Size` (returns `base`) so `CoreWithTemp` can delegate without reaching the trait through the field; the `FrameSource for Core` impl from Task 4 delegates to the same methods. `CoreWithTemp::next` calls the trait method on the inner core, so `src/main.rs` needs `use ra_metal_capture::render::FrameSource;` under the feature cfg.

The zip check in `run_librashader` runs before `RetroArchDirs::read`, so the test with a nonexistent config still hits it; `parse_raw` uses the default `--config`, which exists on this machine but the zip check comes first either way.

Update the `Cli` doc comment and the `--backend` doc string: "Renderer for image mode or core hosting: retroarch (default) or librashader".

- [ ] **Step 4: Verify both shapes**

Run: the full check list from Task 3 Step 3, plus `./scripts/check-ascii.sh` and `typos`.
Expected: all pass in both shapes. In the no-default-features shape `run_librashader` still bails with the "this build has no librashader backend" message for any librashader invocation, including `--core`.

Smoke (no GPU, no core loaded): `cargo run -q -- --backend librashader --core sameboy --rom /nonexistent/x.gbc --shader Cargo.toml --output x.gputrace` fails with "ROM not found"; with `--rom x.zip` it fails with the zip message.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/libretro/mod.rs
git commit -m "Host a libretro core under --backend librashader

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Real run, README, CHANGELOG, CLAUDE.md, spec

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `CLAUDE.md`, `docs/superpowers/specs/2026-09-10-libretro-core-hosting-design.md`

- [ ] **Step 1: Real runs**

Extract the Link's Awakening DX ROM from its zip into a scratch directory (RetroArch's state for it lives under `~/Documents/RetroArch/states/SameBoy/`). Then:

```bash
cargo run -q -- --backend librashader --core sameboy \
  --rom "<scratch>/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc" \
  --state "$HOME/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --frames 2 --output <scratch>/ladx-ls.gputrace -v
```

Expected: stderr shows the core line (`SameBoy (160x144 at 59.728 fps)`), `source 160x144 -> output WxH px, 0 warm-up + 2 recorded frame(s)`, the output path on stdout, exit 0, `index` present, no RetroArch process. Record `du -sh` and wall time. If a tool that summarises gputrace command buffers is available (`gpudebug` was used for the image backend), confirm two command buffers.

Then the same with `--slot 0` instead of `--state` (must resolve to the same file), and once with `--advance 30` to see a later frame. Then the settle path: no state, default `--settle 5` (about 299 warm-up frames), which should record a frame past the boot logo.

Finally the RetroArch-backend regression: the README's existing `--core sameboy --rom ... --state ...` example still works.

If `retro_load_game` or `retro_unserialize` fails, debug with `superpowers:systematic-debugging`; the spike's environment answers are the baseline that is known to work for SameBoy.

- [ ] **Step 2: README**

Usage: after the librashader image example, add:

```
A libretro core can be hosted the same way, with no RetroArch process:

```
ra-metal-capture \
  --backend librashader \
  --core sameboy \
  --rom "path/to/game.gbc" \
  --state "path/to/game.state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --output /tmp/game-ls.gputrace
```
```

Options table: `--backend` row becomes "renderer: `retroarch` (default) or `librashader`; `librashader` requires `--shader` and takes either `--image` or `--core` with `--rom`; ignores `--app`, `--cmd-port`, `--keep-running`". `--state`, `--slot`, `--advance`, `--settle` rows gain a clause for the librashader meaning per the spec's table.

Image mode's Backends section gains a `### Cores` subsection:

```
Under `--backend librashader`, `--core` and `--rom` load the libretro core
into this process instead of launching RetroArch. The core runs headless:
no window, no input (every button reads as released, which is what makes
the run repeatable), no audio. `--state` restores a RetroArch save state
(RetroArch's compressed and container formats are both read); `--slot N`
finds it the way RetroArch names it under `savestate_directory`. The
core's options come from RetroArch's per-core options file, so it renders
the same frame RetroArch would. `--advance N` then runs N frames, the last
of which is recorded, matching the RetroArch backend; without a state,
`--settle` seconds of frames run first. Every frame before the recorded
one still passes through the preset, so history-dependent passes see real
prior frames, and `--frames N` records N consecutive emulated frames.

Only software-rendered cores are supported; a core that asks for an
OpenGL or Vulkan context is refused. Zipped ROMs must be extracted first.
Cores that need BIOS files read them from `system_directory`.
```

- [ ] **Step 3: CHANGELOG**

Add `## Unreleased` above `## 0.3.0 - 2026-09-10`:

```
## Unreleased

### Added

- `--backend librashader --core CORE --rom ROM [--state FILE | --slot N]`:
  host a software-rendered libretro core in this process, restore a
  RetroArch save state, and record emulated frames through the shader
  preset with no RetroArch process. `--frames N` records N consecutive
  emulated frames; `--advance` and `--settle` keep their meanings.
- `libretro` module (feature `librashader`): `Core`, `Context`, `AvInfo`,
  `Frame`, `pixels::to_bgra`.
- `render::FrameSource` and `render::ImageSource`.
- `state::decode`, `state::slot_path`, `state::StateDirs`; `config::read_all`.

### Changed

- Breaking library API: `render::RenderOptions.image` is replaced by
  `source: Box<dyn FrameSource>` and the struct gains `warmup: u32`;
  `render::run` takes `RenderOptions` by value.
- `--backend librashader` no longer requires `--image`.
- New optional dependencies behind the `librashader` feature:
  `libretro-sys`, `libloading`, `flate2`; `deny.toml` allows ISC.
```

- [ ] **Step 4: CLAUDE.md and spec**

CLAUDE.md "Rules that are easy to miss": change the unsafe bullet to "`unsafe` is allowed only inside `src/render/` and `src/libretro/`, each block with a `// SAFETY:` comment. The six `extern "C"` libretro callbacks in `src/libretro/env.rs` are the only hand-written C ABI; they must never unwind." Add to Architecture one paragraph on `libretro` (Core, the statics, one core per process, FrameSource) and update the `render` paragraph for the two-phase loop.

Spec: add a dated "Verified" paragraph under "Testing" with the bundle size, wall time, and which of the four runs in Step 1 passed.

- [ ] **Step 5: Lint and commit**

Run: `./scripts/check-ascii.sh && typos && cargo fmt --check && cargo test && cargo test --no-default-features`
Expected: clean.

```bash
git add README.md CHANGELOG.md CLAUDE.md docs/superpowers/specs/2026-09-10-libretro-core-hosting-design.md
git commit -m "Document libretro core hosting and record the SameBoy run

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

Then `superpowers:finishing-a-development-branch`. Release as 0.4.0 through `RELEASING.md`.
