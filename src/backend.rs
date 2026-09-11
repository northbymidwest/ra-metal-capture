//! The one interface both backends present to the binary: a [`Request`]
//! describing what to capture, and a [`Backend`] that does it. The binary
//! turns its arguments into a request and picks a backend; everything a
//! backend needs beyond the request (the RetroArch app to launch, say) is
//! the backend's own state.

use crate::config::WindowMode;
use anyhow::Result;
use std::path::PathBuf;

/// Where the frames come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A static image file.
    Image(PathBuf),
    /// A libretro core and its content.
    Core {
        /// A `.dylib` path, or a bare name for the core resolver.
        core: String,
        rom: PathBuf,
        state: Option<StateSource>,
        /// A RetroArch-format options file for a hosted core.
        options: Option<PathBuf>,
        /// Load the ROM even if its extension is not one the core declares.
        skip_extension_check: bool,
    },
}

/// Which save state to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSource {
    /// A state file, as given.
    File(PathBuf),
    /// Slot N, found where RetroArch keeps it for this ROM and core.
    Slot(u32),
}

/// A backend-neutral description of one capture.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub source: Source,
    /// The preset. Required by the hosted backend, optional for RetroArch.
    pub shader: Option<PathBuf>,
    pub window: WindowMode,
    /// Frames to record.
    pub frames: u32,
    /// Seconds to run (or wait) before recording when there is no state.
    pub settle: f64,
    /// Frames to run after loading a state before the recorded one.
    pub advance: u32,
    /// Absolute path of the `.gputrace` to write.
    pub output: PathBuf,
    /// A `retroarch.cfg` to consult when a path inferred from RetroArch's
    /// default layout is missing. `None` means consult nothing, as on a
    /// machine without RetroArch; a `Some` that does not exist is treated
    /// the same way.
    pub config: Option<PathBuf>,
    pub verbose: bool,
}

/// A way of turning a [`Request`] into a `.gputrace`.
pub trait Backend {
    /// Anything that must happen before the process touches the platform,
    /// such as re-executing with an environment variable set. Runs before
    /// [`Backend::run`]; the default does nothing.
    fn prepare(&self) -> Result<()> {
        Ok(())
    }

    /// Validate what this backend can honour, do the capture, and print the
    /// bundle's path on stdout as the last thing it does. A backend rejects
    /// a request carrying something it cannot honour, naming it.
    fn run(&self, request: Request) -> Result<()>;
}
