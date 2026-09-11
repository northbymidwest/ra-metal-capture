//! Hosting a libretro core in this process: loading the dylib, answering
//! its environment queries, and turning each `retro_run` into a BGRA8
//! frame for the render module. The libretro ABI carries no user pointer,
//! so the callbacks' state lives in process-wide statics (`env`), and one
//! `Core` per process is enforced. This module and `render` are the only
//! ones that allow `unsafe`; this one is also the only place with a
//! hand-written C ABI (the six callbacks a core calls back into).
#![allow(unsafe_code)]

mod env;
pub(crate) mod pixels;

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
    /// Content extensions the core accepts, lowercased, from libretro's
    /// `valid_extensions` (`"gb|gbc"`). Empty when the core declares none.
    pub valid_extensions: Vec<String>,
    pub need_fullpath: bool,
}

impl SystemInfo {
    /// Whether `rom`'s extension is one the core declares. A core that
    /// declares none accepts anything, as RetroArch treats it.
    pub fn accepts_extension(&self, rom: &Path) -> bool {
        if self.valid_extensions.is_empty() {
            return true;
        }
        rom.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            self.valid_extensions
                .iter()
                .any(|v| v == &e.to_ascii_lowercase())
        })
    }
}

/// `retro_get_system_av_info`, the parts this tool uses.
#[derive(Debug, Clone, PartialEq)]
pub struct AvInfo {
    pub base: Size,
    pub fps: f64,
}

/// An open core. Dropping it unloads the game and deinitialises the core.
pub struct Core {
    api: CoreAPI,
    // Held for the lifetime of `api`'s function pointers; never read.
    // Declared after `api` so the library is closed last.
    _lib: Library,
    initialised: bool,
    loaded: bool,
    /// The base geometry from `load_game`'s `AvInfo`, zero before a game is
    /// loaded. `run_frame` requires every frame to match this size.
    base: Size,
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

/// A path as the UTF-8 C string libretro.h specifies for `retro_game_info`
/// paths and the directory queries. Going through `&str` keeps the encoding
/// claim in the types: a path that is not valid UTF-8 is an error rather
/// than bytes a core would misread.
fn cstring(path: &Path) -> Result<CString> {
    let utf8 = path.to_str().with_context(|| {
        format!(
            "{} is not valid UTF-8, which libretro requires of paths",
            path.display()
        )
    })?;
    CString::new(utf8).with_context(|| format!("{} contains a NUL byte", path.display()))
}

/// Convert a core options map to the `CString` values the callbacks read,
/// dropping any value that contains a NUL byte.
fn options_to_cstring(options: HashMap<String, String>) -> HashMap<String, CString> {
    options
        .into_iter()
        .filter_map(|(k, v)| CString::new(v).ok().map(|v| (k, v)))
        .collect()
}

impl Core {
    /// dlopen `dylib` and resolve its API. No callback is installed and
    /// `retro_init` is not called, so the core is not running yet: that is
    /// `init`, which the caller reaches through `system_info` (the library
    /// name names the options file the `Context` carries). Only one `Core`
    /// may exist per process.
    pub fn open(dylib: &Path) -> Result<Core> {
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
                ..Default::default()
            };
        }
        // Nothing past this point has a `Core` to drop, so a failure has to
        // release the claim itself or a retry would be refused with the
        // wrong message. Nothing here initialises the core, so there is
        // never an initialised core to deinitialise on the way out.
        Core::dlopen(dylib).inspect_err(|_| env::shared().claimed = false)
    }

    /// The part of `open` that runs with the claim held.
    fn dlopen(dylib: &Path) -> Result<Core> {
        // SAFETY: loading a libretro core runs its constructors, which the
        // API contract keeps side-effect free until retro_init.
        let lib = unsafe { Library::new(dylib) }
            .with_context(|| format!("loading core {}", dylib.display()))?;
        // SAFETY: `lib` was just dlopened from the path the caller named as
        // a libretro core, which is `resolve`'s requirement.
        let api = unsafe { resolve(&lib) }
            .with_context(|| format!("reading the libretro API of {}", dylib.display()))?;
        // SAFETY: retro_api_version takes no arguments and returns the
        // constant the core was built against; RetroArch calls it before
        // retro_init as well. libretro.h does not spell out a pre-init
        // guarantee for it (it does for retro_get_system_info).
        let version = unsafe { (api.retro_api_version)() };
        if version != libretro_sys::API_VERSION {
            bail!(
                "{} reports libretro API version {version}; this build speaks version {}",
                dylib.display(),
                libretro_sys::API_VERSION
            );
        }
        Ok(Core {
            api,
            _lib: lib,
            initialised: false,
            loaded: false,
            base: Size {
                width: 0,
                height: 0,
            },
        })
    }

    /// Publish `ctx` to the callbacks, install them, and call `retro_init`.
    /// The options travel in `ctx` because RetroArch has option values
    /// available from `retro_init` onward and a core may read them there.
    pub fn init(&mut self, ctx: Context) -> Result<()> {
        if self.initialised {
            bail!("this core is already initialised");
        }
        // Everything fallible happens before the shared state is touched,
        // so a bad path cannot leave half a Context behind.
        let system_dir = cstring(&ctx.system_dir)?;
        let save_dir = cstring(&ctx.save_dir)?;
        let options = options_to_cstring(ctx.options);
        {
            let mut s = env::shared();
            s.system_dir = Some(system_dir);
            s.save_dir = Some(save_dir);
            s.options = options;
        }
        // SAFETY: the libretro contract: every callback is set before
        // retro_init, each with the signature libretro.h declares for it,
        // and each points at a function in `env` that cannot unwind. `api`
        // came from `_lib`, which is still loaded and outlives this call.
        unsafe {
            (self.api.retro_set_environment)(env::environment);
            (self.api.retro_set_video_refresh)(env::video_refresh);
            (self.api.retro_set_audio_sample)(env::audio_sample);
            (self.api.retro_set_audio_sample_batch)(env::audio_sample_batch);
            (self.api.retro_set_input_poll)(env::input_poll);
            (self.api.retro_set_input_state)(env::input_state);
            (self.api.retro_init)();
        }
        self.initialised = true;
        Ok(())
    }

    /// `retro_get_system_info`, which libretro.h allows at any time, even
    /// before `init`.
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
        let valid_extensions = if raw.valid_extensions.is_null() {
            Vec::new()
        } else {
            // SAFETY: as for library_name; libretro.h documents
            // valid_extensions as a static, NUL-terminated string when set.
            unsafe { CStr::from_ptr(raw.valid_extensions) }
                .to_string_lossy()
                .split('|')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_ascii_lowercase())
                .collect()
        };
        SystemInfo {
            library_name: name,
            valid_extensions,
            need_fullpath: raw.need_fullpath,
        }
    }

    /// `retro_load_game` with the ROM's bytes and path, then the AV info.
    pub fn load_game(&mut self, rom: &Path) -> Result<AvInfo> {
        if !self.initialised {
            bail!("the core is not initialised (init must run before load_game)");
        }
        if rom
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            bail!(
                "{} is a zip; this backend takes the extracted ROM (RetroArch extracts archives itself)",
                rom.display()
            );
        }
        // A core that sets need_fullpath opens the content itself and is
        // handed no buffer, as RetroArch does; the rest get the bytes.
        let bytes = if self.system_info().need_fullpath {
            Vec::new()
        } else {
            std::fs::read(rom).with_context(|| format!("reading {}", rom.display()))?
        };
        let path = cstring(rom)?;
        let info = GameInfo {
            path: path.as_ptr(),
            data: if bytes.is_empty() {
                std::ptr::null()
            } else {
                bytes.as_ptr() as *const c_void
            },
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
        let base = Size {
            width: av.geometry.base_width,
            height: av.geometry.base_height,
        };
        self.base = base;
        Ok(AvInfo {
            base,
            fps: av.timing.fps,
        })
    }

    /// `retro_unserialize`. The message on failure names both sizes, since
    /// a size mismatch is the usual cause (wrong core version or ROM).
    pub fn restore(&mut self, state: &[u8]) -> Result<()> {
        if !self.loaded {
            bail!("no game is loaded (load_game must run before restore)");
        }
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

    /// The base geometry from `load_game`'s `AvInfo`, zero before a game is
    /// loaded.
    pub fn size(&self) -> Size {
        self.base
    }

    /// Run one emulated frame and return what the core drew. On a dupe the
    /// previous frame is returned again. It is an error before any frame has
    /// arrived, and also on a dupe that follows a frame `video_refresh`
    /// rejected for inconsistent geometry, since that clears the stored one.
    /// The returned frame's size must match `load_game`'s base geometry.
    pub fn run_frame(&mut self) -> Result<Frame> {
        if !self.loaded {
            bail!("no game is loaded (load_game must run before run_frame)");
        }
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
        // SAFETY: this mirrors `open` and `init` and runs exactly once,
        // since `Core` is neither `Clone` nor reachable after drop: unload
        // the game only if `load_game` succeeded, and deinitialise only the
        // core that `init` initialised, since libretro.h pairs retro_deinit
        // with retro_init. `_lib` is dropped after this, so the functions
        // are still mapped.
        unsafe {
            if self.loaded {
                (self.api.retro_unload_game)();
            }
            if self.initialised {
                (self.api.retro_deinit)();
            }
        }
        let mut s = env::shared();
        s.claimed = false;
        s.frame = None;
    }
}

impl crate::render::FrameSource for Core {
    fn size(&self) -> Size {
        self.size()
    }
    fn next(&mut self) -> Result<Vec<u8>> {
        let frame = self.run_frame()?;
        if frame.size != self.base {
            bail!(
                "the core changed its frame size to {}x{} (base geometry is {}x{}); mid-run geometry changes are not supported",
                frame.size.width,
                frame.size.height,
                self.base.width,
                self.base.height
            );
        }
        Ok(frame.bgra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(exts: &[&str]) -> SystemInfo {
        SystemInfo {
            library_name: "SameBoy".into(),
            valid_extensions: exts.iter().map(|e| e.to_string()).collect(),
            need_fullpath: false,
        }
    }

    #[test]
    fn accepts_extension_is_case_insensitive_and_exact() {
        let i = info(&["gb", "gbc"]);
        assert!(i.accepts_extension(Path::new("/r/zelda.GBC")));
        assert!(i.accepts_extension(Path::new("/r/tetris.gb")));
        assert!(!i.accepts_extension(Path::new("/r/sample.png")));
        assert!(!i.accepts_extension(Path::new("/r/noext")));
    }

    #[test]
    fn cstring_requires_utf8_and_no_interior_nul() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            cstring(Path::new("/roms/zelda.gbc")).unwrap().as_bytes(),
            b"/roms/zelda.gbc"
        );
        let not_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/roms/\xff.gbc"));
        let err = cstring(not_utf8).unwrap_err().to_string();
        assert!(err.contains("UTF-8"), "{err}");
        let with_nul = Path::new(std::ffi::OsStr::from_bytes(b"/roms/a\0b"));
        let err = cstring(with_nul).unwrap_err().to_string();
        assert!(err.contains("NUL"), "{err}");
    }

    #[test]
    fn a_core_declaring_no_extensions_accepts_anything() {
        assert!(info(&[]).accepts_extension(Path::new("/r/anything.bin")));
    }
}
