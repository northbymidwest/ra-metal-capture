//! Ctrl-C for the in-process backend. A capture in progress cannot be
//! abandoned from a signal handler: Metal is writing the bundle, and the
//! [`super::render::Trace`] guard that stops it lives on the render
//! thread. So the handler only raises a flag, the frame loops poll it
//! between frames, and the run unwinds through its normal error path,
//! which stops the capture and removes the partial bundle, returning
//! [`Interrupted`] for the binary to turn into exit status 130.

use crate::backend::Interrupted;
use anyhow::Result;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();

/// Install the handler once per process. A failure to install (another
/// handler already owns the signal) is ignored: the run then simply keeps
/// the default behaviour of dying on Ctrl-C.
pub fn install() {
    INSTALL.call_once(|| {
        let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
    });
}

/// `Err(Interrupted)` once Ctrl-C has been pressed.
pub fn check() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        Err(Interrupted.into())
    } else {
        Ok(())
    }
}
