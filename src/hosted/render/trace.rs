//! Writing a `.gputrace` from inside this process with Metal's capture
//! API, through `objc2-metal`'s `MTLCaptureManager` bindings.

use anyhow::{Context, Result, anyhow, bail};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_foundation::{NSString, NSURL};
use objc2_metal::{MTLCaptureDescriptor, MTLCaptureDestination, MTLCaptureManager, MTLDevice};
use std::path::Path;

/// The environment variable Metal reads when it loads to decide whether
/// programmatic capture to a trace document is offered. It must be `1`
/// before the process touches Metal; `main` re-execs to make it so.
pub const CAPTURE_ENV: &str = "MTL_CAPTURE_ENABLED";

/// An in-progress capture. Every command buffer created on the device after
/// [`Trace::start`] and committed before [`Trace::finish`] lands in the
/// bundle. Dropping an unfinished `Trace` stops the capture, so a failure
/// mid-frame never leaves the process capturing while it unwinds.
#[must_use = "dropping a Trace stops the capture"]
pub struct Trace {
    manager: Retained<MTLCaptureManager>,
    capturing: bool,
}

impl Trace {
    /// Start writing a GPU trace document at `output` for every command
    /// buffer on `device`.
    pub fn start(device: &ProtocolObject<dyn MTLDevice>, output: &Path) -> Result<Trace> {
        // SAFETY: the generated binding for this accessor documents no
        // invariant (there is no `# Safety` section); this crate calls it
        // from one thread only.
        let manager = unsafe { MTLCaptureManager::sharedCaptureManager() };
        if !manager.supportsDestination(MTLCaptureDestination::GPUTraceDocument) {
            bail!(
                "Metal will not write a GPU trace document from this process; \
                 {CAPTURE_ENV}=1 must be in the environment before Metal loads"
            );
        }
        let path = output
            .to_str()
            .with_context(|| format!("{} is not valid UTF-8", output.display()))?;
        let descriptor = MTLCaptureDescriptor::new();
        let object: &AnyObject = device.as_ref();
        // SAFETY: an MTLDevice is one of the object types the descriptor
        // documents as valid (device, command queue, or capture scope).
        unsafe { descriptor.setCaptureObject(Some(object)) };
        descriptor.setDestination(MTLCaptureDestination::GPUTraceDocument);
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        descriptor.setOutputURL(Some(&url));
        manager
            .startCaptureWithDescriptor_error(&descriptor)
            .map_err(|e| {
                anyhow!(
                    "starting the Metal capture to {}: {}",
                    output.display(),
                    e.localizedDescription()
                )
            })?;
        Ok(Trace {
            manager,
            capturing: true,
        })
    }

    /// Stop the capture and let Metal finish writing the bundle.
    pub fn finish(mut self) {
        self.manager.stopCapture();
        self.capturing = false;
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if self.capturing {
            self.manager.stopCapture();
        }
    }
}
