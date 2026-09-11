//! The frontend state a core reaches through its callbacks, and the six
//! `extern "C"` callbacks themselves. libretro passes no user pointer, so
//! this is process-wide; `Core::open` claims it and `Drop` releases it.
//!
//! Nothing here may unwind: a panic crossing back into the core's C frames
//! is undefined behaviour. So the callbacks avoid `?`, `unwrap`, `expect`,
//! and any indexing that is not proved in range first, and they recover
//! from a poisoned lock rather than panicking on it.

use super::pixels::to_bgra;
use crate::config::Size;
use libretro_sys::{PixelFormat, Variable};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// One emulated frame, tightly packed BGRA8, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub size: Size,
    pub bgra: Vec<u8>,
}

/// What the callbacks read and write.
pub struct Shared {
    /// Set by `Core::open`; a second open while this is true is refused.
    pub claimed: bool,
    pub system_dir: Option<CString>,
    pub save_dir: Option<CString>,
    pub options: HashMap<String, CString>,
    pub pixel_format: PixelFormat,
    pub asked_for_hw_render: bool,
    pub frame: Option<Frame>,
}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            claimed: false,
            system_dir: None,
            save_dir: None,
            options: HashMap::new(),
            // libretro's default when a core never sets one. `PixelFormat`
            // is a foreign type, so it cannot carry its own `Default`.
            pixel_format: PixelFormat::ARGB1555,
            asked_for_hw_render: false,
            frame: None,
        }
    }
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(|| Mutex::new(Shared::default()));

/// The shared state, recovering from a poisoned lock so a callback can
/// never panic on entry.
pub fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bytes per pixel in a core's framebuffer for each format `to_bgra` reads.
fn bytes_per_pixel(format: PixelFormat) -> usize {
    match format {
        PixelFormat::ARGB8888 => 4,
        PixelFormat::RGB565 | PixelFormat::ARGB1555 => 2,
    }
}

// `libretro-sys` 0.1.1 predates these four; the values are from libretro.h
// (RETRO_ENVIRONMENT_SET_CORE_OPTIONS, _INTL, _V2 and _V2_INTL).
const ENVIRONMENT_SET_CORE_OPTIONS: c_uint = 53;
const ENVIRONMENT_SET_CORE_OPTIONS_INTL: c_uint = 54;
const ENVIRONMENT_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;

// The two flag bits libretro.h can set on a command value:
// RETRO_ENVIRONMENT_EXPERIMENTAL marks a command whose number is otherwise
// one of the documented ones, and RETRO_ENVIRONMENT_PRIVATE marks a
// frontend's own command, whose low bits mean nothing here.
const ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;
const ENVIRONMENT_PRIVATE: c_uint = 0x20000;

/// The environment callback. Answers exactly what a software-rendered core
/// needs and `false` to everything else.
///
/// # Safety
///
/// `data` must be the pointer type libretro.h documents for `cmd`, valid
/// for the duration of the call. Only a libretro core may call this.
pub unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    use libretro_sys::{
        ENVIRONMENT_GET_CAN_DUPE, ENVIRONMENT_GET_SAVE_DIRECTORY, ENVIRONMENT_GET_SYSTEM_DIRECTORY,
        ENVIRONMENT_GET_VARIABLE, ENVIRONMENT_GET_VARIABLE_UPDATE, ENVIRONMENT_SET_HW_RENDER,
        ENVIRONMENT_SET_PIXEL_FORMAT, ENVIRONMENT_SET_VARIABLES,
    };
    // A private command is somebody else's; RetroArch matches full command
    // values and only ever masks the experimental bit off (runloop.c), so
    // clearing the private bit too would answer a command we do not know.
    if cmd & ENVIRONMENT_PRIVATE != 0 {
        return false;
    }
    let cmd = cmd & !ENVIRONMENT_EXPERIMENTAL;
    // Acknowledging a core's option list needs nothing from `data`, and
    // libretro.h lets a core pass NULL there to declare that it has none.
    if matches!(
        cmd,
        ENVIRONMENT_SET_VARIABLES
            | ENVIRONMENT_SET_CORE_OPTIONS
            | ENVIRONMENT_SET_CORE_OPTIONS_INTL
            | ENVIRONMENT_SET_CORE_OPTIONS_V2
            | ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL
    ) {
        return true;
    }
    if data.is_null() {
        // A null `retro_variable*` is a core probing whether this frontend
        // supports core options at all, which RetroArch answers `true`
        // (runloop.c). Every other command below reads or writes `data`.
        return cmd == ENVIRONMENT_GET_VARIABLE;
    }
    let mut s = shared();
    // SAFETY: the caller guarantees `data` is the type libretro.h documents
    // for `cmd` and is valid for this call; each arm casts to exactly that
    // type and only under its own command, and the null case returned above.
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
            ENVIRONMENT_GET_VARIABLE => {
                // RetroArch answers every query `true` and says "no such
                // option" only by leaving the value null (runloop.c), so
                // that a core can tell a frontend without core-option
                // support from one that simply has no value to give.
                // A null key asks for the whole environment string, which
                // this frontend does not build, so it gets a null value too.
                let var = &mut *(data as *mut Variable);
                var.value = std::ptr::null();
                if !var.key.is_null() {
                    let key = CStr::from_ptr(var.key).to_string_lossy();
                    if let Some(v) = s.options.get(key.as_ref()) {
                        var.value = v.as_ptr();
                    }
                }
                true
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

/// Copy the core's framebuffer out as BGRA8 before the call returns.
///
/// A frame whose geometry `to_bgra` could not read in bounds (a row of
/// `width` pixels wider than `pitch`, or a buffer whose length overflows)
/// is dropped rather than converted, so the caller sees no frame instead
/// of a panic unwinding into the core.
///
/// # Safety
///
/// `data`, when non-null, must point at `height` rows of `pitch` bytes,
/// valid for reads for the duration of the call. Only a libretro core may
/// call this.
pub unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    if data.is_null() {
        // A dupe: the core is telling us the previous frame still stands.
        return;
    }
    let mut s = shared();
    let rows = height as usize;
    if rows == 0 || width == 0 {
        s.frame = None;
        return;
    }
    // `to_bgra` reads `width * bytes_per_pixel` bytes from each row and
    // indexes rows by `pitch`; both must be in range for every row. The
    // buffer a core owns ends with the last row's pixels, not its padding,
    // so the slice covers `(rows - 1) * pitch + row_bytes` bytes.
    let row_bytes = (width as usize).checked_mul(bytes_per_pixel(s.pixel_format));
    let total = row_bytes.and_then(|rb| (rows - 1).checked_mul(pitch)?.checked_add(rb));
    let (Some(row_bytes), Some(total)) = (row_bytes, total) else {
        s.frame = None;
        return;
    };
    if row_bytes > pitch {
        s.frame = None;
        return;
    }
    // SAFETY: libretro.h guarantees `data` holds `height` rows `pitch`
    // bytes apart, each with `width` pixels; the last row need not carry
    // padding, so exactly `total` bytes are valid for reads for the
    // duration of this call. The slice is only read here and is dropped
    // before returning, so no pointer of the core's escapes.
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, total) };
    let bgra = to_bgra(s.pixel_format, bytes, width, height, pitch);
    s.frame = Some(Frame {
        size: Size { width, height },
        bgra,
    });
}

/// Audio is not recorded; a sample is dropped.
///
/// # Safety
///
/// Nothing to uphold; the signature is `unsafe` only to match libretro's.
pub unsafe extern "C" fn audio_sample(_left: i16, _right: i16) {}

/// Audio is not recorded; every frame offered is reported consumed.
///
/// # Safety
///
/// Nothing to uphold: `data` is never read.
pub unsafe extern "C" fn audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    frames
}

/// No input is delivered, so there is nothing to poll.
///
/// # Safety
///
/// Nothing to uphold; the signature is `unsafe` only to match libretro's.
pub unsafe extern "C" fn input_poll() {}

/// Every input reads as neutral.
///
/// # Safety
///
/// Nothing to uphold; the signature is `unsafe` only to match libretro's.
pub unsafe extern "C" fn input_state(
    _port: c_uint,
    _device: c_uint,
    _index: c_uint,
    _id: c_uint,
) -> i16 {
    0
}
