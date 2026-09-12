//! The frontend state a core reaches through its callbacks, and the six
//! `extern "C"` callbacks themselves. libretro passes no user pointer, so
//! this is process-wide; `Core::open` claims it and `Drop` releases it.
//!
//! Nothing here may unwind: a panic crossing back into the core's C frames
//! is undefined behaviour. So the callbacks avoid `?`, `unwrap`, `expect`,
//! and any indexing that is not proved in range first, and they recover
//! from a poisoned lock rather than panicking on it.

use super::pixels::{PixelFormat, to_bgra};
use super::sys;
use crate::config::Size;
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
    /// Values from `--core-options`; consulted first by `GET_VARIABLE`.
    pub options: HashMap<String, CString>,
    /// The defaults a core declared through `SET_VARIABLES` or one of the
    /// `SET_CORE_OPTIONS` forms; what `GET_VARIABLE` answers when
    /// `options` has nothing, which is what RetroArch does on a fresh
    /// config. A core may take `true` from `GET_VARIABLE` as "a value is
    /// present" (snes9x's `update_variables` calls `strcmp` on it), so a
    /// registered key must never come back null.
    pub defaults: HashMap<String, CString>,
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
            defaults: HashMap::new(),
            // libretro's default when a core never sets one. `PixelFormat`
            // is a foreign type, so it cannot carry its own `Default`.
            pixel_format: PixelFormat::Xrgb1555,
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
        PixelFormat::Xrgb8888 => 4,
        PixelFormat::Rgb565 | PixelFormat::Xrgb1555 => 2,
    }
}

/// The core-options API version this frontend implements, answered to
/// `GET_CORE_OPTIONS_VERSION`; a core then registers with the v2 form.
const CORE_OPTIONS_VERSION: c_uint = 2;
/// `RETRO_NUM_CORE_OPTION_VALUES_MAX` as a length.
const NUM_CORE_OPTION_VALUES_MAX: usize = sys::RETRO_NUM_CORE_OPTION_VALUES_MAX as usize;

/// A C string as an owned key or value; None for a null pointer.
///
/// # Safety
///
/// `p` is null or points at a NUL-terminated string that outlives the call.
unsafe fn owned(p: *const c_char) -> Option<CString> {
    if p.is_null() {
        None
    } else {
        // SAFETY: the caller's contract.
        Some(unsafe { CStr::from_ptr(p) }.to_owned())
    }
}

/// The default of a v0 variable: its value is `"Description; a|b|c"` and
/// the first listed option is the default (RetroArch's
/// `core_option_manager_new_vars`). A value without the `"; "` separator
/// is taken whole, as RetroArch does.
fn v0_default(value: &CStr) -> CString {
    let text = value.to_bytes();
    let after_desc = match text.windows(2).position(|w| w == b"; ") {
        Some(i) => &text[i + 2..],
        None => text,
    };
    let first = after_desc.split(|b| *b == b'|').next().unwrap_or_default();
    CString::new(first).unwrap_or_default()
}

/// The default of a v1 or v2 definition: `default_value`, else the first
/// value, else nothing.
///
/// # Safety
///
/// Every non-null pointer is a NUL-terminated string valid for the call.
unsafe fn definition_default(
    default_value: *const c_char,
    values: &[sys::retro_core_option_value; NUM_CORE_OPTION_VALUES_MAX],
) -> Option<CString> {
    // SAFETY: the caller's contract covers both pointers.
    unsafe { owned(default_value).or_else(|| owned(values[0].value)) }
}

/// Record the defaults a `SET_VARIABLES` list declares: `retro_variable`
/// entries up to one with a null key.
///
/// # Safety
///
/// `list` points at such an array, valid for the call.
unsafe fn register_v0(s: &mut Shared, mut list: *const sys::retro_variable) {
    // SAFETY: the caller's contract; the walk stops at the null key.
    unsafe {
        while !(*list).key.is_null() {
            if let (Some(key), Some(value)) = (owned((*list).key), owned((*list).value)) {
                s.defaults
                    .insert(key.to_string_lossy().into_owned(), v0_default(&value));
            }
            list = list.add(1);
        }
    }
}

/// Record the defaults a v1 definition list declares.
///
/// # Safety
///
/// `list` is null or points at a null-key-terminated array valid for the call.
unsafe fn register_v1(s: &mut Shared, mut list: *const sys::retro_core_option_definition) {
    if list.is_null() {
        return;
    }
    // SAFETY: the caller's contract; the walk stops at the null key.
    unsafe {
        while !(*list).key.is_null() {
            let d = &*list;
            if let (Some(key), Some(value)) =
                (owned(d.key), definition_default(d.default_value, &d.values))
            {
                s.defaults.insert(key.to_string_lossy().into_owned(), value);
            }
            list = list.add(1);
        }
    }
}

/// Record the defaults a v2 table declares.
///
/// # Safety
///
/// `table` is null or points at a `sys::retro_core_options_v2` whose `definitions` is
/// null or a null-key-terminated array, all valid for the call.
unsafe fn register_v2(s: &mut Shared, table: *const sys::retro_core_options_v2) {
    if table.is_null() {
        return;
    }
    // SAFETY: the caller's contract; the walk stops at the null key.
    unsafe {
        let mut list: *const sys::retro_core_option_v2_definition = (*table).definitions;
        if list.is_null() {
            return;
        }
        while !(*list).key.is_null() {
            let d = &*list;
            if let (Some(key), Some(value)) =
                (owned(d.key), definition_default(d.default_value, &d.values))
            {
                s.defaults.insert(key.to_string_lossy().into_owned(), value);
            }
            list = list.add(1);
        }
    }
}

/// The environment callback. Answers exactly what a software-rendered core
/// needs and `false` to everything else.
///
/// # Safety
///
/// `data` must be the pointer type libretro.h documents for `cmd`, valid
/// for the duration of the call. Only a libretro core may call this.
pub unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    // A private command is somebody else's (RETRO_ENVIRONMENT_PRIVATE marks
    // a frontend's own); RetroArch matches full command values and only
    // ever masks the experimental bit off (runloop.c), so clearing the
    // private bit too would answer a command we do not know.
    if cmd & sys::RETRO_ENVIRONMENT_PRIVATE != 0 {
        return false;
    }
    let cmd = cmd & !sys::RETRO_ENVIRONMENT_EXPERIMENTAL;
    // libretro.h lets a core pass NULL for its option list to declare
    // that it has none; that is acknowledged without reading anything.
    if data.is_null()
        && matches!(
            cmd,
            sys::RETRO_ENVIRONMENT_SET_VARIABLES
                | sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS
                | sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL
                | sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2
                | sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL
        )
    {
        return true;
    }
    if data.is_null() {
        // A null `retro_variable*` is a core probing whether this frontend
        // supports core options at all, which RetroArch answers `true`
        // (runloop.c). Every other command below reads or writes `data`.
        return cmd == sys::RETRO_ENVIRONMENT_GET_VARIABLE;
    }
    let mut s = shared();
    // SAFETY: the caller guarantees `data` is the type libretro.h documents
    // for `cmd` and is valid for this call; each arm casts to exactly that
    // type and only under its own command, and the null case returned above.
    unsafe {
        match cmd {
            sys::RETRO_ENVIRONMENT_GET_CAN_DUPE => {
                *(data as *mut bool) = true;
                true
            }
            sys::RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY => match &s.system_dir {
                Some(d) => {
                    *(data as *mut *const c_char) = d.as_ptr();
                    true
                }
                None => false,
            },
            sys::RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => match &s.save_dir {
                Some(d) => {
                    *(data as *mut *const c_char) = d.as_ptr();
                    true
                }
                None => false,
            },
            sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
                match PixelFormat::from_raw(*(data as *const c_uint)) {
                    Some(format) => {
                        s.pixel_format = format;
                        true
                    }
                    None => false,
                }
            }
            sys::RETRO_ENVIRONMENT_GET_VARIABLE => {
                // RetroArch answers every query `true` and says "no such
                // option" only by leaving the value null (runloop.c), so
                // that a core can tell a frontend without core-option
                // support from one that simply has no value to give.
                // A null key asks for the whole environment string, which
                // this frontend does not build, so it gets a null value too.
                let var = &mut *(data as *mut sys::retro_variable);
                var.value = std::ptr::null();
                if !var.key.is_null() {
                    let key = CStr::from_ptr(var.key).to_string_lossy();
                    if let Some(v) = s
                        .options
                        .get(key.as_ref())
                        .or_else(|| s.defaults.get(key.as_ref()))
                    {
                        var.value = v.as_ptr();
                    }
                }
                true
            }
            sys::RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
                *(data as *mut c_uint) = CORE_OPTIONS_VERSION;
                true
            }
            sys::RETRO_ENVIRONMENT_SET_VARIABLES => {
                register_v0(&mut s, data as *const sys::retro_variable);
                true
            }
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS => {
                register_v1(&mut s, data as *const sys::retro_core_option_definition);
                true
            }
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL => {
                register_v1(&mut s, (*(data as *const sys::retro_core_options_intl)).us);
                true
            }
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2 => {
                register_v2(&mut s, data as *const sys::retro_core_options_v2);
                true
            }
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL => {
                register_v2(
                    &mut s,
                    (*(data as *const sys::retro_core_options_v2_intl)).us,
                );
                true
            }
            sys::RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                *(data as *mut bool) = false;
                true
            }
            sys::RETRO_ENVIRONMENT_SET_HW_RENDER => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::{Mutex, MutexGuard};

    /// The callbacks read one process-wide state, so tests take this lock
    /// and reset that state before touching it.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn fresh() -> MutexGuard<'static, ()> {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *shared() = Shared::default();
        guard
    }

    fn env(cmd: c_uint, data: *mut c_void) -> bool {
        // SAFETY: every caller below passes the pointer type libretro.h
        // documents for `cmd`, backed by a live local.
        unsafe { environment(cmd, data) }
    }

    #[test]
    fn can_dupe_is_answered_true_through_the_out_pointer() {
        let _g = fresh();
        let mut flag = false;
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_CAN_DUPE,
            &mut flag as *mut bool as *mut c_void
        ));
        assert!(flag);
    }

    #[test]
    fn experimental_bit_is_ignored_but_private_bit_refuses() {
        let _g = fresh();
        let mut flag = false;
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_CAN_DUPE | sys::RETRO_ENVIRONMENT_EXPERIMENTAL,
            &mut flag as *mut bool as *mut c_void
        ));
        assert!(flag);
        let mut untouched = false;
        assert!(!env(
            sys::RETRO_ENVIRONMENT_GET_CAN_DUPE | sys::RETRO_ENVIRONMENT_PRIVATE,
            &mut untouched as *mut bool as *mut c_void
        ));
        assert!(!untouched, "a private command must not write through data");
    }

    #[test]
    fn option_declarations_are_acknowledged_even_with_null_data() {
        let _g = fresh();
        for cmd in [
            sys::RETRO_ENVIRONMENT_SET_VARIABLES,
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS,
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL,
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2,
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL,
        ] {
            assert!(env(cmd, std::ptr::null_mut()), "cmd {cmd}");
        }
    }

    /// The value the callback answers for `key`, or None for a null value.
    fn lookup(key: &str) -> Option<String> {
        let key = CString::new(key).unwrap();
        let mut var = sys::retro_variable {
            key: key.as_ptr(),
            value: std::ptr::null(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_VARIABLE,
            &mut var as *mut sys::retro_variable as *mut c_void
        ));
        if var.value.is_null() {
            None
        } else {
            // SAFETY: the callback set `value` to a CString it owns in `shared()`.
            Some(
                unsafe { CStr::from_ptr(var.value) }
                    .to_str()
                    .unwrap()
                    .into(),
            )
        }
    }

    fn no_values() -> [sys::retro_core_option_value; NUM_CORE_OPTION_VALUES_MAX] {
        [sys::retro_core_option_value {
            value: std::ptr::null(),
            label: std::ptr::null(),
        }; NUM_CORE_OPTION_VALUES_MAX]
    }

    #[test]
    fn set_variables_registers_the_first_listed_option_as_the_default() {
        let _g = fresh();
        let k1 = CString::new("snes9x_hires_blend").unwrap();
        let v1 = CString::new("Hires blending; disabled|merge|blur").unwrap();
        let k2 = CString::new("snes9x_region").unwrap();
        let v2 = CString::new("Region; auto|ntsc|pal").unwrap();
        let mut list = [
            sys::retro_variable {
                key: k1.as_ptr(),
                value: v1.as_ptr(),
            },
            sys::retro_variable {
                key: k2.as_ptr(),
                value: v2.as_ptr(),
            },
            sys::retro_variable {
                key: std::ptr::null(),
                value: std::ptr::null(),
            },
        ];
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_VARIABLES,
            list.as_mut_ptr() as *mut c_void
        ));
        assert_eq!(lookup("snes9x_hires_blend").as_deref(), Some("disabled"));
        assert_eq!(lookup("snes9x_region").as_deref(), Some("auto"));
        assert_eq!(lookup("snes9x_other"), None);
    }

    #[test]
    fn set_core_options_v1_uses_default_value_or_else_the_first_value() {
        let _g = fresh();
        let k1 = CString::new("a").unwrap();
        let k2 = CString::new("b").unwrap();
        let x = CString::new("x").unwrap();
        let y = CString::new("y").unwrap();
        let mut values = no_values();
        values[0].value = x.as_ptr();
        values[1].value = y.as_ptr();
        let mut defs = [
            sys::retro_core_option_definition {
                key: k1.as_ptr(),
                desc: std::ptr::null(),
                info: std::ptr::null(),
                values,
                default_value: y.as_ptr(),
            },
            sys::retro_core_option_definition {
                key: k2.as_ptr(),
                desc: std::ptr::null(),
                info: std::ptr::null(),
                values,
                default_value: std::ptr::null(),
            },
            sys::retro_core_option_definition {
                key: std::ptr::null(),
                desc: std::ptr::null(),
                info: std::ptr::null(),
                values: no_values(),
                default_value: std::ptr::null(),
            },
        ];
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS,
            defs.as_mut_ptr() as *mut c_void
        ));
        assert_eq!(lookup("a").as_deref(), Some("y"));
        assert_eq!(lookup("b").as_deref(), Some("x"));

        // The intl form carries the same table under `us`.
        *shared() = Shared::default();
        let mut intl = sys::retro_core_options_intl {
            us: defs.as_mut_ptr(),
            local: std::ptr::null_mut(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL,
            &mut intl as *mut sys::retro_core_options_intl as *mut c_void
        ));
        assert_eq!(lookup("a").as_deref(), Some("y"));
    }

    #[test]
    fn set_core_options_v2_and_its_intl_form_register_the_definitions() {
        let _g = fresh();
        let k = CString::new("snes9x_up_down_allowed").unwrap();
        let dis = CString::new("disabled").unwrap();
        let en = CString::new("enabled").unwrap();
        let mut values = no_values();
        values[0].value = dis.as_ptr();
        values[1].value = en.as_ptr();
        let mut defs = [
            sys::retro_core_option_v2_definition {
                key: k.as_ptr(),
                desc: std::ptr::null(),
                desc_categorized: std::ptr::null(),
                info: std::ptr::null(),
                info_categorized: std::ptr::null(),
                category_key: std::ptr::null(),
                values,
                default_value: std::ptr::null(),
            },
            sys::retro_core_option_v2_definition {
                key: std::ptr::null(),
                desc: std::ptr::null(),
                desc_categorized: std::ptr::null(),
                info: std::ptr::null(),
                info_categorized: std::ptr::null(),
                category_key: std::ptr::null(),
                values: no_values(),
                default_value: std::ptr::null(),
            },
        ];
        let mut v2 = sys::retro_core_options_v2 {
            categories: std::ptr::null_mut(),
            definitions: defs.as_mut_ptr(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2,
            &mut v2 as *mut sys::retro_core_options_v2 as *mut c_void
        ));
        assert_eq!(
            lookup("snes9x_up_down_allowed").as_deref(),
            Some("disabled")
        );

        *shared() = Shared::default();
        let mut intl = sys::retro_core_options_v2_intl {
            us: &mut v2,
            local: std::ptr::null_mut(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL,
            &mut intl as *mut sys::retro_core_options_v2_intl as *mut c_void
        ));
        assert_eq!(
            lookup("snes9x_up_down_allowed").as_deref(),
            Some("disabled")
        );
    }

    #[test]
    fn a_core_options_file_value_overrides_a_registered_default() {
        let _g = fresh();
        shared()
            .options
            .insert("snes9x_region".into(), CString::new("pal").unwrap());
        let k = CString::new("snes9x_region").unwrap();
        let v = CString::new("Region; auto|ntsc|pal").unwrap();
        let mut list = [
            sys::retro_variable {
                key: k.as_ptr(),
                value: v.as_ptr(),
            },
            sys::retro_variable {
                key: std::ptr::null(),
                value: std::ptr::null(),
            },
        ];
        assert!(env(
            sys::RETRO_ENVIRONMENT_SET_VARIABLES,
            list.as_mut_ptr() as *mut c_void
        ));
        assert_eq!(lookup("snes9x_region").as_deref(), Some("pal"));
    }

    #[test]
    fn core_options_version_probe_answers_two() {
        let _g = fresh();
        let mut version: c_uint = 0;
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION,
            &mut version as *mut c_uint as *mut c_void
        ));
        assert_eq!(version, 2);
    }

    #[test]
    fn unknown_commands_and_null_data_are_refused_except_the_variable_probe() {
        let _g = fresh();
        let mut flag = false;
        assert!(!env(9999, &mut flag as *mut bool as *mut c_void));
        assert!(!env(
            sys::RETRO_ENVIRONMENT_GET_CAN_DUPE,
            std::ptr::null_mut()
        ));
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_VARIABLE,
            std::ptr::null_mut()
        ));
    }

    #[test]
    fn get_variable_answers_true_and_signals_unknown_keys_with_a_null_value() {
        let _g = fresh();
        shared()
            .options
            .insert("sameboy_model".into(), CString::new("Auto").unwrap());
        let key = CString::new("sameboy_model").unwrap();
        let mut var = sys::retro_variable {
            key: key.as_ptr(),
            value: std::ptr::null(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_VARIABLE,
            &mut var as *mut sys::retro_variable as *mut c_void
        ));
        // SAFETY: the callback set `value` to a CString it owns in `shared()`.
        assert_eq!(
            unsafe { CStr::from_ptr(var.value) }.to_str().unwrap(),
            "Auto"
        );

        let unknown = CString::new("nope").unwrap();
        let mut var = sys::retro_variable {
            key: unknown.as_ptr(),
            value: key.as_ptr(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_VARIABLE,
            &mut var as *mut sys::retro_variable as *mut c_void
        ));
        assert!(var.value.is_null());

        let mut var = sys::retro_variable {
            key: std::ptr::null(),
            value: key.as_ptr(),
        };
        assert!(env(
            sys::RETRO_ENVIRONMENT_GET_VARIABLE,
            &mut var as *mut sys::retro_variable as *mut c_void
        ));
        assert!(var.value.is_null());
    }

    #[test]
    fn pixel_format_maps_the_three_known_values_and_refuses_others() {
        let _g = fresh();
        for (raw, want) in [
            (0u32, PixelFormat::Xrgb1555),
            (1, PixelFormat::Xrgb8888),
            (2, PixelFormat::Rgb565),
        ] {
            let mut v = raw;
            assert!(env(
                sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                &mut v as *mut c_uint as *mut c_void
            ));
            assert_eq!(shared().pixel_format, want);
        }
        let mut v = 7u32;
        assert!(!env(
            sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            &mut v as *mut c_uint as *mut c_void
        ));
        assert_eq!(shared().pixel_format, PixelFormat::Rgb565, "unchanged");
    }

    #[test]
    fn hw_render_is_refused_and_remembered() {
        let _g = fresh();
        let mut anything = 0u32;
        assert!(!env(
            sys::RETRO_ENVIRONMENT_SET_HW_RENDER,
            &mut anything as *mut c_uint as *mut c_void
        ));
        assert!(shared().asked_for_hw_render);
    }

    #[test]
    fn video_refresh_copies_a_frame_sized_to_what_the_core_owns() {
        let _g = fresh();
        shared().pixel_format = PixelFormat::Xrgb8888;
        // 2x2 XRGB8888 with a pitch of 12: the last row carries no padding.
        let buf: [u8; 20] = [
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, // row 0 plus padding
            7, 8, 9, 0, 10, 11, 12, 0, // row 1, exactly width * 4
        ];
        // SAFETY: `buf` is 20 bytes, which is (2 - 1) * 12 + 2 * 4.
        unsafe { video_refresh(buf.as_ptr() as *const c_void, 2, 2, 12) };
        let frame = shared().frame.clone().expect("a frame");
        assert_eq!(
            frame.size,
            Size {
                width: 2,
                height: 2
            }
        );
        assert_eq!(
            frame.bgra,
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn video_refresh_keeps_the_previous_frame_on_a_dupe_and_drops_bad_geometry() {
        let _g = fresh();
        shared().pixel_format = PixelFormat::Xrgb8888;
        let buf = [0u8; 4];
        // SAFETY: one 1x1 XRGB8888 pixel, pitch 4.
        unsafe { video_refresh(buf.as_ptr() as *const c_void, 1, 1, 4) };
        assert!(shared().frame.is_some());
        // SAFETY: a null pointer is libretro's dupe signal; nothing is read.
        unsafe { video_refresh(std::ptr::null(), 1, 1, 4) };
        assert!(shared().frame.is_some(), "dupe keeps the previous frame");
        // A row wider than the pitch is refused before any read.
        // SAFETY: the guard rejects the geometry before touching the buffer.
        unsafe { video_refresh(buf.as_ptr() as *const c_void, 4, 1, 4) };
        assert!(shared().frame.is_none());
    }
}
