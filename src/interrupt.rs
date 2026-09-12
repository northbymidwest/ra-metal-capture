//! Ctrl-C handling for both backends. A capture in progress cannot be
//! abandoned from a signal handler: Metal or `gpucapture` is writing the
//! bundle, and the guards that stop it (the hosted backend's
//! `render::Trace`, the RetroArch backend's child guard and temp dir) live
//! on the thread running the capture. So the first Ctrl-C only raises a
//! flag, the wait loops poll it with [`check`], and the run unwinds through
//! its normal error path, which stops the capture, removes the partial
//! bundle, and returns [`Interrupted`] for the binary to turn into exit
//! status 130.
//!
//! Two things happen in the handler itself. A RetroArch the run launched
//! ([`register_child`]) is SIGKILLed at once, since a paused RetroArch
//! never exits on its own and nothing else can reach it from the handler.
//! And a second Ctrl-C exits the process with status 130 immediately,
//! accepting a partial bundle, for a run stuck somewhere that never polls
//! (a hung `waitUntilCompleted`, a core that never returns).

use crate::backend::Interrupted;
use anyhow::Result;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// The launched RetroArch's pid, or 0 when there is none to kill.
static CHILD: AtomicU32 = AtomicU32::new(0);
static INSTALL: Once = Once::new();

/// Install the handler once per process. A failure to install (another
/// handler already owns the signal) is ignored: the run then simply keeps
/// the default behaviour of dying on Ctrl-C.
pub fn install() {
    INSTALL.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
            let pid = CHILD.load(Ordering::SeqCst);
            if pid != 0 {
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
            }
        });
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

/// The child the handler kills on Ctrl-C; call once RetroArch is spawned.
pub fn register_child(pid: u32) {
    CHILD.store(pid, Ordering::SeqCst);
}

/// Forget the registered child, once it has exited or been handed over
/// to the user with `--keep-running`.
pub fn clear_child() {
    CHILD.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_reports_interrupted_once_the_flag_is_raised() {
        assert!(check().is_ok());
        INTERRUPTED.store(true, Ordering::SeqCst);
        let err = check().unwrap_err();
        assert!(err.is::<Interrupted>());
        INTERRUPTED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn registered_child_is_what_the_handler_would_kill() {
        register_child(4242);
        assert_eq!(CHILD.load(Ordering::SeqCst), 4242);
        clear_child();
        assert_eq!(CHILD.load(Ordering::SeqCst), 0);
    }
}
