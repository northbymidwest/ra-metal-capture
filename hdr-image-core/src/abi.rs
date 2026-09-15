//! The libretro C ABI: the `retro_*` exports a frontend resolves by name
//! (`Core::open` in ra-metal-capture, `dylib_load` in RetroArch). This is
//! the crate's only unsafe code. Nothing here may unwind into the
//! frontend: a panic leaving an `extern "C"` function aborts the process,
//! so every body that can fail runs under `catch_unwind` and reports
//! failure through the return value instead.

use crate::content::{Content, Format, encode};
use ra_metal_capture::config::{Gamut, Hdr, HdrMode};
use ra_metal_capture::hosted::libretro::sys;
use std::ffi::{CStr, c_char, c_uint, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// The callbacks the frontend installed. libretro sets each before
/// `retro_init` and they stay valid until `retro_deinit`.
#[derive(Clone, Copy)]
struct Callbacks {
    environment: sys::retro_environment_t,
    video_refresh: sys::retro_video_refresh_t,
    input_poll: sys::retro_input_poll_t,
}

static CALLBACKS: Mutex<Callbacks> = Mutex::new(Callbacks {
    environment: None,
    video_refresh: None,
    input_poll: None,
});

/// The loaded image, encoded for the format the frontend accepted.
struct Loaded {
    width: u32,
    height: u32,
    /// `width * height` pixels, rows top first, `width * 4` bytes each.
    frame: Vec<u32>,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

fn callbacks() -> Callbacks {
    *CALLBACKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn loaded() -> MutexGuard<'static, Option<Loaded>> {
    LOADED.lock().unwrap_or_else(|e| e.into_inner())
}

const NAME: &CStr = c"hdr-image";
const VERSION: &CStr = c"0.0.0";
const EXTENSIONS: &CStr = c"hdr|png|jpg|jpeg";

/// Ask the frontend `cmd` with `data`; `false` when no environment
/// callback is installed.
///
/// # Safety
///
/// `data` must be the pointer type libretro.h documents for `cmd`, valid
/// for the call.
unsafe fn environ(cmd: c_uint, data: *mut c_void) -> bool {
    match callbacks().environment {
        // SAFETY: the frontend installed this for us to call, with the
        // caller's contract on `data`.
        Some(f) => unsafe { f(cmd, data) },
        None => false,
    }
}

/// The first format the frontend accepts, asked in HDR10, 10-bit SDR,
/// 8-bit order, as libretro.h tells a core with an SDR fallback to do.
fn choose_format() -> Option<Format> {
    use sys::retro_pixel_format as pf;
    for (format, raw) in [
        (Format::Hdr10, pf::RETRO_PIXEL_FORMAT_HDR10_2101010),
        (Format::Xrgb2101010, pf::RETRO_PIXEL_FORMAT_XRGB2101010),
        (Format::Xrgb8888, pf::RETRO_PIXEL_FORMAT_XRGB8888),
    ] {
        let mut v = raw as c_uint;
        // SAFETY: SET_PIXEL_FORMAT takes a `const enum retro_pixel_format
        // *`, backed by a live local for the call.
        if unsafe {
            environ(
                sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                &mut v as *mut c_uint as *mut c_void,
            )
        } {
            return Some(format);
        }
    }
    None
}

/// The frontend's HDR settings through the three queries an encoder
/// needs, each keeping libretro.h's fallback when unrecognised. `mode`
/// is not queried: the format the frontend accepted already says.
fn query_settings() -> Hdr {
    let mut s = Hdr::default();
    let mut white = 0f32;
    let mut peak = 0f32;
    let mut gamut = 0u32;
    // SAFETY: each pointer is the type libretro.h documents for its
    // query (`float *`, `float *`, `unsigned *`), backed by a live local.
    unsafe {
        if environ(
            sys::RETRO_ENVIRONMENT_GET_HDR_PAPER_WHITE_NITS,
            &mut white as *mut f32 as *mut c_void,
        ) {
            s.paper_white_nits = white;
        }
        if environ(
            sys::RETRO_ENVIRONMENT_GET_HDR_MAX_NITS,
            &mut peak as *mut f32 as *mut c_void,
        ) {
            s.max_nits = peak;
        }
        if environ(
            sys::RETRO_ENVIRONMENT_GET_HDR_EXPAND_GAMUT,
            &mut gamut as *mut c_uint as *mut c_void,
        ) {
            s.expand_gamut = match gamut {
                1 => Gamut::Expanded,
                2 => Gamut::Wide,
                3 => Gamut::Super,
                _ => Gamut::Accurate,
            };
        }
    }
    s
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_environment(cb: sys::retro_environment_t) {
    CALLBACKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .environment = cb;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_video_refresh(cb: sys::retro_video_refresh_t) {
    CALLBACKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .video_refresh = cb;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample(_cb: sys::retro_audio_sample_t) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample_batch(_cb: sys::retro_audio_sample_batch_t) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_poll(cb: sys::retro_input_poll_t) {
    CALLBACKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .input_poll = cb;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_state(_cb: sys::retro_input_state_t) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_init() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_deinit() {
    *loaded() = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_api_version() -> c_uint {
    sys::RETRO_API_VERSION
}

/// # Safety
///
/// `info` is null or points at a `retro_system_info` the frontend owns,
/// valid for writes for the call; the strings written into it are static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_info(info: *mut sys::retro_system_info) {
    if info.is_null() {
        return;
    }
    // SAFETY: the caller's contract; every field is written, none read.
    unsafe {
        info.write(sys::retro_system_info {
            library_name: NAME.as_ptr(),
            library_version: VERSION.as_ptr(),
            valid_extensions: EXTENSIONS.as_ptr(),
            need_fullpath: true,
            block_extract: false,
        })
    };
}

/// # Safety
///
/// `info` is null or points at a `retro_system_av_info` the frontend
/// owns, valid for writes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut sys::retro_system_av_info) {
    if info.is_null() {
        return;
    }
    let (w, h) = loaded().as_ref().map_or((1, 1), |l| (l.width, l.height));
    // SAFETY: the caller's contract; every field is written, none read.
    unsafe {
        info.write(sys::retro_system_av_info {
            geometry: sys::retro_game_geometry {
                base_width: w,
                base_height: h,
                max_width: w,
                max_height: h,
                aspect_ratio: w as f32 / h as f32,
            },
            timing: sys::retro_system_timing {
                fps: 60.0,
                sample_rate: 44100.0,
            },
        })
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_controller_port_device(_port: c_uint, _device: c_uint) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {}

/// Deliver the loaded frame, the same bytes every call.
#[unsafe(no_mangle)]
pub extern "C" fn retro_run() {
    let cb = callbacks();
    if let Some(poll) = cb.input_poll {
        // SAFETY: the frontend installed this for us to call each frame.
        unsafe { poll() };
    }
    let guard = loaded();
    if let (Some(l), Some(video)) = (guard.as_ref(), cb.video_refresh) {
        // SAFETY: `frame` holds `width * height` pixels in rows of
        // `width * 4` bytes, valid for the call, and libretro's contract
        // is that the frontend copies before returning; the lock is held
        // across the call so nothing frees it meanwhile. The lock is held
        // across the call, so a frontend that re-entered this core from
        // inside its video callback would deadlock on it; neither
        // ra-metal-capture nor RetroArch does, and libretro does not
        // allow it.
        unsafe {
            video(
                l.frame.as_ptr() as *const c_void,
                l.width,
                l.height,
                l.width as usize * 4,
            )
        };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize_size() -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize(_data: *mut c_void, _size: usize) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unserialize(_data: *const c_void, _size: usize) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_reset() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_set(_index: c_uint, _enabled: bool, _code: *const c_char) {}

/// Decode the content, pick the format, read the HDR settings, encode.
///
/// # Safety
///
/// `game` is null or points at a `retro_game_info` valid for the call
/// whose `path` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game(game: *const sys::retro_game_info) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        if game.is_null() {
            eprintln!("[hdr-image] no content");
            return false;
        }
        // SAFETY: the caller's contract.
        let path = unsafe { (*game).path };
        if path.is_null() {
            eprintln!("[hdr-image] no content path (need_fullpath is set)");
            return false;
        }
        // SAFETY: the caller's contract: a NUL-terminated string.
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned();
        let content = match Content::open(Path::new(&path)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[hdr-image] {e}");
                return false;
            }
        };
        let Some(format) = choose_format() else {
            eprintln!(
                "[hdr-image] the frontend accepted none of HDR10_2101010, XRGB2101010, XRGB8888"
            );
            return false;
        };
        let mut settings = query_settings();
        settings.mode = match format {
            Format::Hdr10 => HdrMode::Hdr10,
            Format::Xrgb2101010 | Format::Xrgb8888 => HdrMode::Off,
        };
        let (width, height) = content.size();
        eprintln!(
            "[hdr-image] {}x{} {} content as {format:?}; paper white {} nits, peak {} nits, gamut {}",
            width,
            height,
            match content {
                Content::Sdr { .. } => "SDR",
                Content::Hdr { .. } => "HDR",
            },
            settings.paper_white_nits,
            settings.max_nits,
            settings.expand_gamut.as_u32()
        );
        let frame = encode(&content, format, &settings);
        *loaded() = Some(Loaded {
            width,
            height,
            frame,
        });
        true
    }))
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_load_game_special(
    _game_type: c_uint,
    _info: *const sys::retro_game_info,
    _num_info: usize,
) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    *loaded() = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_region() -> c_uint {
    sys::RETRO_REGION_NTSC
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_data(_id: c_uint) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_size(_id: c_uint) -> usize {
    0
}
