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
use libloading::Library;
use libretro_sys::{CoreAPI, GameInfo, SystemAvInfo, SystemInfo as RawSystemInfo};
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
    // Declared after `api` so the library is closed last.
    _lib: Library,
    loaded: bool,
}

/// Resolve every `retro_*` symbol into a `CoreAPI`.
///
/// # Safety
///
/// `lib` must be a libretro core: each symbol is looked up by the name
/// libretro.h gives it and used at the signature libretro.h declares for
/// it, and the resolved pointers stay valid only while `lib` is loaded.
unsafe fn resolve(lib: &Library) -> Result<CoreAPI> {
    macro_rules! sym {
        ($name:literal) => {{
            // SAFETY: the caller guarantees `lib` is a libretro core, so
            // this name denotes the function libretro.h declares, whose
            // signature is the `CoreAPI` field this initialises. The
            // `Symbol` borrow ends here; the copied pointer is valid as
            // long as `lib` stays loaded, which `Core` guarantees.
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
        // Everything fallible that does not need the claim happens first,
        // so a bad path cannot leave the claim set.
        let system_dir = cstring(&ctx.system_dir)?;
        let save_dir = cstring(&ctx.save_dir)?;
        let options = ctx
            .options
            .into_iter()
            .filter_map(|(k, v)| CString::new(v).ok().map(|v| (k, v)))
            .collect();
        {
            let mut s = env::shared();
            if s.claimed {
                bail!("a libretro core is already loaded in this process");
            }
            // Replacing the whole struct, rather than assigning field by
            // field, is what guarantees a second core in this process
            // inherits nothing from the first: the pixel format goes back
            // to libretro's ARGB1555 default along with everything else.
            *s = env::Shared {
                claimed: true,
                system_dir: Some(system_dir),
                save_dir: Some(save_dir),
                options,
                ..Default::default()
            };
        }
        // Nothing past this point has a `Core` to drop, so a failure has to
        // release the claim itself or a retry would be refused with the
        // wrong message. `retro_init` is the last step, so there is never
        // an initialised core to deinitialise here.
        Core::open(dylib).inspect_err(|_| env::shared().claimed = false)
    }

    /// The part of `load` that runs with the claim held.
    fn open(dylib: &Path) -> Result<Core> {
        // SAFETY: loading a libretro core runs its constructors, which the
        // API contract keeps side-effect free until retro_init.
        let lib = unsafe { Library::new(dylib) }
            .with_context(|| format!("loading core {}", dylib.display()))?;
        // SAFETY: `lib` was just dlopened from the path the caller named as
        // a libretro core, which is `resolve`'s requirement.
        let api = unsafe { resolve(&lib) }
            .with_context(|| format!("reading the libretro API of {}", dylib.display()))?;
        // SAFETY: the libretro contract: every callback is set before
        // retro_init, each with the signature libretro.h declares for it,
        // and each points at a function in `env` that cannot unwind. `api`
        // came from `lib`, which is still loaded and outlives this call.
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

    /// `retro_get_system_info`, which a core may answer at any time.
    pub fn system_info(&self) -> SystemInfo {
        let mut raw = RawSystemInfo {
            library_name: std::ptr::null(),
            library_version: std::ptr::null(),
            valid_extensions: std::ptr::null(),
            need_fullpath: false,
            block_extract: false,
        };
        // SAFETY: `raw` is a live, fully initialised SystemInfo for the
        // call; libretro.h allows this call at any time after loading and
        // has the core fill the struct with pointers to static strings.
        unsafe { (self.api.retro_get_system_info)(&mut raw) };
        let name = if raw.library_name.is_null() {
            String::new()
        } else {
            // SAFETY: libretro.h requires a non-null library_name to be a
            // NUL-terminated string that stays valid until retro_deinit,
            // and this core has not been deinitialised.
            unsafe { CStr::from_ptr(raw.library_name) }
                .to_string_lossy()
                .into_owned()
        };
        SystemInfo {
            library_name: name,
            need_fullpath: raw.need_fullpath,
        }
    }

    /// `retro_load_game` with the ROM's bytes and path, then the AV info.
    pub fn load_game(&mut self, rom: &Path) -> Result<AvInfo> {
        if rom
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
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
        // SAFETY: `info` is fully initialised and its pointers (`path` and
        // `bytes`, both still owned here) stay valid for the whole call;
        // libretro.h says the core copies anything it keeps.
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
        // SAFETY: `av` is a live, fully initialised SystemAvInfo, and
        // libretro.h allows this call once retro_load_game has succeeded,
        // which the check above established.
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
        // SAFETY: `state` is a live slice of `state.len()` readable bytes
        // for the duration of the call, which is what libretro.h asks for.
        let ok =
            unsafe { (self.api.retro_unserialize)(state.as_ptr() as *const c_void, state.len()) };
        if !ok {
            // SAFETY: libretro.h allows this call between retro_load_game
            // and retro_unload_game; it takes no arguments and only reads
            // the core's own state.
            let expected = unsafe { (self.api.retro_serialize_size)() };
            bail!(
                "the core rejected the save state ({} bytes; the core serializes {expected} bytes)",
                state.len()
            );
        }
        Ok(())
    }

    /// Run one emulated frame and return what the core drew. On a dupe the
    /// previous frame is returned again. It is an error before any frame has
    /// arrived, and also on a dupe that follows a frame `video_refresh`
    /// rejected for inconsistent geometry, since that clears the stored one.
    pub fn run_frame(&mut self) -> Result<Frame> {
        // SAFETY: libretro.h allows this call once a game is loaded, and
        // every callback it reaches is one of `env`'s, which copy what they
        // need out before returning, so no pointer of the core's escapes.
        unsafe { (self.api.retro_run)() };
        env::shared()
            .frame
            .clone()
            .context("the core ran a frame but delivered no video")
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: this mirrors `load` and runs exactly once, since `Core`
        // is neither `Clone` nor reachable after drop: unload the game only
        // if `load_game` succeeded, then deinitialise the core that `load`
        // initialised. `_lib` is dropped after this, so the functions are
        // still mapped.
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
