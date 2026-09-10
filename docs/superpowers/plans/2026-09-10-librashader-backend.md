# librashader Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--backend librashader` to image mode: render the image through the `.slangp` preset in-process with librashader's Metal runtime and write the `.gputrace` with Metal's `MTLCaptureManager`, with the RetroArch backend unchanged and still the default.

**Architecture:** A new feature-gated `render` module (the only module allowed `unsafe`) decodes the image, builds Metal textures, loads the preset through `librashader::runtime::mtl::FilterChain`, and renders `--frames` command buffers between `MTLCaptureManager` start and stop. `main` gains a `--backend` flag, re-execs itself once with `MTL_CAPTURE_ENABLED=1` when that backend is chosen, and branches to `render::run` before any RetroArch work. `display` learns the main screen's full frame and backing scale so window modes map to pixel sizes.

**Tech Stack:** librashader 0.12 (runtime-metal), objc2-metal 0.3.2, objc2 0.6, image 0.25; existing clap, anyhow, objc2-app-kit.

**Spec:** `docs/superpowers/specs/2026-09-10-librashader-backend-design.md`

## Global Constraints

- All Metal and capture calls go through `objc2-metal` and `objc2-foundation`; no hand-written `extern` blocks or FFI anywhere in this crate.
- `unsafe` only inside `src/render/`; `src/lib.rs` and `src/main.rs` keep `#![deny(unsafe_code)]`.
- Never write the user's `retroarch.cfg` or states dir (unchanged; the new backend never reads them).
- ASCII only, no em dashes or en dashes (`scripts/check-ascii.sh`).
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` clean after every task, for both `--no-default-features` and default features from Task 2 on. `cargo test` never constructs a Metal device.
- `rust-version = "1.98"`, edition 2024 (unchanged).
- Commit trailer: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Branch `librashader-backend` from `main` at 7c7b36d (spec commit), using the worktree flow (`superpowers:using-git-worktrees`).

## File map

| file | change | responsibility |
|---|---|---|
| `Cargo.toml` | modify | `librashader` feature (default on), optional deps, docs.rs `no-default-features` |
| `deny.toml` | modify | allow MPL-2.0 and BSD-3-Clause; duplicates to warn |
| `.github/workflows/ci.yml` | modify | `--no-default-features` build and test steps |
| `src/display.rs` | modify | `Screen`, `main_screen()` |
| `src/render/mod.rs` | create | `RenderOptions`, `output_size`, `run` |
| `src/render/trace.rs` | create | `Trace` wrapper over `MTLCaptureManager`, `CAPTURE_ENV` |
| `src/capture.rs` | modify | `prepare_output`, `is_gputrace_bundle` become `pub` |
| `src/lib.rs` | modify | `pub mod render` behind the feature |
| `src/main.rs` | modify | `Backend` enum, `--backend`, re-exec, `run_librashader` |
| `README.md`, `CHANGELOG.md`, spec | modify | docs, measured run |

---

### Task 1: `display::Screen`

**Files:**
- Modify: `src/display.rs`

**Interfaces:**
- Produces: `pub struct Screen { pub visible: Size, pub full: Size, pub backing_scale: f64 }` and `pub fn main_screen() -> Screen`. `visible_size()` and `fill_mode()` keep their signatures. Task 2 consumes `Screen`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/display.rs`:

```rust
    #[test]
    fn visible_size_matches_main_screen() {
        // Both are AppKit queries (or the same fallback off the main thread),
        // so they must agree; this pins visible_size to main_screen.
        assert_eq!(visible_size(), main_screen().visible);
    }

    #[test]
    fn fallback_screen_is_1080p_at_1x() {
        assert_eq!(FALLBACK_SCREEN.visible, FALLBACK);
        assert_eq!(FALLBACK_SCREEN.full, FALLBACK);
        assert_eq!(FALLBACK_SCREEN.backing_scale, 1.0);
    }
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test --lib display`
Expected: compile error, `main_screen` and `FALLBACK_SCREEN` not found.

- [ ] **Step 3: Implement**

Replace the body of `src/display.rs` above the tests with:

```rust
//! The main display: its visible and full frames in points and its
//! backing scale, for the default fill-the-screen window mode and for
//! sizing the librashader backend's output in pixels. Queries AppKit;
//! falls back to 1920x1080 at 1x off the main thread.

use crate::config::{Size, WindowMode};
use objc2_app_kit::NSScreen;
use objc2_foundation::MainThreadMarker;

/// Allowance for the window title bar, which the visible frame does not exclude.
pub const TITLE_BAR_POINTS: u32 = 28;

const FALLBACK: Size = Size {
    width: 1920,
    height: 1080,
};

/// The main display as the tool sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// Visible frame in points; excludes the menu bar and dock.
    pub visible: Size,
    /// The whole display in points.
    pub full: Size,
    /// Pixels per point, for example 2.0 on a Retina display.
    pub backing_scale: f64,
}

const FALLBACK_SCREEN: Screen = Screen {
    visible: FALLBACK,
    full: FALLBACK,
    backing_scale: 1.0,
};

/// The main display's visible frame, full frame, and backing scale. Falls
/// back to 1920x1080 at 1x with a warning on stderr when no screen can be
/// queried.
pub fn main_screen() -> Screen {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("warning: not on the main thread; assuming a 1920x1080 display at 1x");
        return FALLBACK_SCREEN;
    };
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        eprintln!("warning: no main screen; assuming a 1920x1080 display at 1x");
        return FALLBACK_SCREEN;
    };
    let visible = screen.visibleFrame();
    let full = screen.frame();
    Screen {
        visible: Size {
            width: visible.size.width as u32,
            height: visible.size.height as u32,
        },
        full: Size {
            width: full.size.width as u32,
            height: full.size.height as u32,
        },
        backing_scale: screen.backingScaleFactor(),
    }
}

/// Width and height in points of the main display's visible frame
/// (excludes the menu bar and dock). See [`main_screen`] for the fallback.
pub fn visible_size() -> Size {
    main_screen().visible
}

/// The fill-the-screen window mode for a given visible size.
pub fn fill_mode(visible: Size) -> WindowMode {
    WindowMode::Fill {
        max: Size {
            width: visible.width,
            height: visible.height.saturating_sub(TITLE_BAR_POINTS),
        },
    }
}
```

`frame` and `backingScaleFactor` are already enabled by objc2-app-kit's `NSScreen` feature; no manifest change.

- [ ] **Step 4: Run tests, clippy, fmt**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass (the two new tests included; `visible_size_matches_main_screen` prints two fallback warnings off the main thread, which is fine).

- [ ] **Step 5: Commit**

```bash
git add src/display.rs
git commit -m "Expose the main screen's full frame and backing scale

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Feature, dependencies, policy, and `render::output_size`

**Files:**
- Modify: `Cargo.toml`, `deny.toml`, `.github/workflows/ci.yml`, `src/lib.rs`
- Create: `src/render/mod.rs`

**Interfaces:**
- Consumes: `display::Screen` (Task 1), `config::{Size, WindowMode}`.
- Produces: `pub fn output_size(mode: &WindowMode, image: Size, screen: &Screen) -> Size` in `render`. Task 3 fills in the rest of the module.

- [ ] **Step 1: Manifest**

In `Cargo.toml`, add after `[package.metadata.docs.rs]`'s existing two lines:

```toml
# docs.rs builds on Linux with the Apple target above, and `cargo doc` runs
# dependency build scripts, so the librashader feature's C++ (glslang,
# SPIRV-Cross) cannot build there. Document the RetroArch-only shape.
no-default-features = true
```

Add before `[lib]`:

```toml
[features]
default = ["librashader"]
# The in-process image backend: librashader's Metal runtime plus Metal's
# capture API. Without it the tool is RetroArch-only and builds no C++.
librashader = ["dep:librashader", "dep:objc2", "dep:objc2-metal", "dep:image"]
```

Replace the `objc2-foundation` line and append to `[dependencies]`:

```toml
objc2-foundation = { version = "0.3.2", features = ["NSGeometry", "NSString", "NSURL", "NSError"] }
# librashader backend (feature "librashader")
librashader = { version = "0.12.0", optional = true, default-features = false, features = ["runtime-metal", "presets", "preprocess"] }
objc2 = { version = "0.6.4", optional = true }
objc2-metal = { version = "0.3.2", optional = true, features = [
    "MTLCaptureManager",
    "MTLCommandBuffer",
    "MTLCommandQueue",
    "MTLDevice",
    "MTLPixelFormat",
    "MTLResource",
    "MTLTexture",
    "MTLTypes",
] }
image = { version = "0.25.8", optional = true, default-features = false, features = ["png", "jpeg", "bmp", "gif", "tga", "pnm", "hdr"] }
```

- [ ] **Step 2: deny.toml**

Replace the `[licenses]` `allow` list and the `[bans]` section:

```toml
[licenses]
# The union of every SPDX license in the resolved tree (surveyed from
# `cargo metadata`). `unused-allowed-license = "deny"` keeps this list honest:
# it fails if an entry here stops being needed.
#
# MPL-2.0 is librashader (and persy, smartstring in its tree). MPL is
# file-scoped copyleft: linking it does not touch this crate's 0BSD terms,
# and its source is on crates.io. BSD-3-Clause is encoding_rs, which is
# "(Apache-2.0 OR MIT) AND BSD-3-Clause".
allow = [
    "0BSD",
    "MIT",
    "Apache-2.0",
    "Unicode-3.0",
    "Zlib",
    "MPL-2.0",
    "BSD-3-Clause",
]
unused-allowed-license = "deny"

[bans]
# librashader's tree carries around ten duplicate crate versions (syn,
# thiserror, hashbrown, ...) that this crate cannot resolve; a skip list
# would go stale on every librashader release. Warn, do not fail.
multiple-versions = "warn"
wildcards = "deny"
```

- [ ] **Step 3: CI**

In `.github/workflows/ci.yml`, in the `macos` job after the `cargo test` step and before `docs`, add:

```yaml
      # The RetroArch-only shape must keep building and testing without the
      # librashader feature (and its C++ dependencies).
      - run: cargo build --no-default-features
      - run: cargo clippy --no-default-features --all-targets -- -D warnings
      - run: cargo test --no-default-features
```

Also update the job's leading comment: after "rustdoc." add "Both feature shapes are built: default (librashader backend, compiles glslang and SPIRV-Cross from C++) and `--no-default-features`."

- [ ] **Step 4: Write the failing tests**

Create `src/render/mod.rs`:

```rust
//! The librashader backend: render an image through a preset inside this
//! process and write the `.gputrace` with Metal's capture API. Sizing is
//! pure and tested; everything that touches Metal lives in [`run`] and
//! [`trace`], and is exercised only by a real run.

use crate::config::{Size, WindowMode};
use crate::display::Screen;

/// Multiply a size in points by the backing scale, flooring to whole pixels.
fn to_pixels(size: Size, backing_scale: f64) -> Size {
    Size {
        width: (size.width as f64 * backing_scale) as u32,
        height: (size.height as f64 * backing_scale) as u32,
    }
}

/// Scale `image` by one factor so it fits inside `max`, keeping its aspect
/// ratio and flooring to whole pixels, the way RetroArch's fill mode clamps
/// a scaled window. Falls back to the image's own size when the factor
/// would be zero or produce an empty texture (a zero `max`, or an image
/// with a zero dimension).
fn fit(image: Size, max: Size) -> Size {
    if image.width == 0 || image.height == 0 {
        return image;
    }
    let factor = f64::min(
        max.width as f64 / image.width as f64,
        max.height as f64 / image.height as f64,
    );
    let fitted = Size {
        width: (image.width as f64 * factor) as u32,
        height: (image.height as f64 * factor) as u32,
    };
    if fitted.width == 0 || fitted.height == 0 {
        image
    } else {
        fitted
    }
}

/// The output texture size, in pixels, for a window mode. RetroArch's modes
/// are in points; this maps each to pixels on `screen` so both backends
/// produce a similarly sized frame. `Exact` is taken as pixels as given.
pub fn output_size(mode: &WindowMode, image: Size, screen: &Screen) -> Size {
    match mode {
        WindowMode::Exact(size) => *size,
        WindowMode::Scale(n) => Size {
            width: image.width * n,
            height: image.height * n,
        },
        WindowMode::Fullscreen => to_pixels(screen.full, screen.backing_scale),
        WindowMode::Fill { max } => fit(image, to_pixels(*max, screen.backing_scale)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: Size = Size {
        width: 160,
        height: 144,
    };

    fn screen(backing_scale: f64) -> Screen {
        Screen {
            visible: Size {
                width: 2488,
                height: 1410,
            },
            full: Size {
                width: 2560,
                height: 1440,
            },
            backing_scale,
        }
    }

    #[test]
    fn exact_is_pixels_as_given() {
        let size = Size {
            width: 1600,
            height: 1440,
        };
        assert_eq!(output_size(&WindowMode::Exact(size), GB, &screen(2.0)), size);
    }

    #[test]
    fn scale_multiplies_the_image() {
        assert_eq!(
            output_size(&WindowMode::Scale(4), GB, &screen(2.0)),
            Size {
                width: 640,
                height: 576
            }
        );
    }

    #[test]
    fn fullscreen_is_the_full_frame_in_pixels() {
        assert_eq!(
            output_size(&WindowMode::Fullscreen, GB, &screen(2.0)),
            Size {
                width: 5120,
                height: 2880
            }
        );
        assert_eq!(
            output_size(&WindowMode::Fullscreen, GB, &screen(1.0)),
            Size {
                width: 2560,
                height: 1440
            }
        );
    }

    #[test]
    fn fill_is_height_limited_for_a_wide_display() {
        // 2488x1382 points at 2x is 4976x2764 px; 2764/144 = 19.19 < 4976/160.
        let max = Size {
            width: 2488,
            height: 1382,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, GB, &screen(2.0)),
            Size {
                width: 3071,
                height: 2764
            }
        );
    }

    #[test]
    fn fill_is_width_limited_for_a_wide_image() {
        let max = Size {
            width: 1000,
            height: 1000,
        };
        let wide = Size {
            width: 400,
            height: 100,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, wide, &screen(1.0)),
            Size {
                width: 1000,
                height: 250
            }
        );
    }

    #[test]
    fn fill_shrinks_an_image_larger_than_the_display() {
        let max = Size {
            width: 1000,
            height: 1000,
        };
        let huge = Size {
            width: 4000,
            height: 1000,
        };
        assert_eq!(
            output_size(&WindowMode::Fill { max }, huge, &screen(1.0)),
            Size {
                width: 1000,
                height: 250
            }
        );
    }

    #[test]
    fn fill_falls_back_to_the_image_when_max_is_degenerate() {
        let zero = Size {
            width: 0,
            height: 0,
        };
        assert_eq!(output_size(&WindowMode::Fill { max: zero }, GB, &screen(2.0)), GB);
        let sliver = Size {
            width: 100,
            height: 0,
        };
        assert_eq!(output_size(&WindowMode::Fill { max: sliver }, GB, &screen(1.0)), GB);
    }
}
```

In `src/lib.rs`, after `pub mod launch;` add:

```rust
#[cfg(feature = "librashader")]
pub mod render;
```

and extend the crate doc comment with one paragraph after the paused-capture sentence:

```rust
//!
//! With the `librashader` feature (on by default), image mode can instead
//! render in-process through [`render::run`], which writes the trace with
//! Metal's capture API and never launches RetroArch.
```

- [ ] **Step 5: Build both shapes and run tests**

Run: `time cargo build` (first build compiles glslang and SPIRV-Cross; note the wall time for the README), then `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`, then `cargo build --no-default-features && cargo test --no-default-features && cargo clippy --no-default-features --all-targets -- -D warnings`.
Expected: all pass; the seven `render::tests` run under default features and are absent without it. `cargo deny check licenses bans sources` passes (warnings for duplicate versions are expected). `cargo machete` reports `librashader`, `objc2`, `objc2-metal`, and `image` unused until Task 3; that is expected at this commit.

Reference measurement, 2026-09-10, scratch project with the same librashader features, M-series Mac: clean build 25.6 s wall, 159 s CPU.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock deny.toml .github/workflows/ci.yml src/lib.rs src/render/mod.rs
git commit -m "Add the librashader feature, its policy changes, and output sizing

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `render::trace` and `render::run`

**Files:**
- Create: `src/render/trace.rs`
- Modify: `src/render/mod.rs`, `src/capture.rs`

**Interfaces:**
- Consumes: `output_size` (Task 2), `capture::prepare_output(&Path) -> Result<()>` and `capture::is_gputrace_bundle(&Path) -> bool` (made `pub` here).
- Produces: `pub struct RenderOptions { pub image: PathBuf, pub preset: PathBuf, pub window: WindowMode, pub screen: Screen, pub frames: u32, pub output: PathBuf, pub verbose: bool }`, `pub fn run(&RenderOptions) -> Result<()>`, `pub const CAPTURE_ENV: &str = "MTL_CAPTURE_ENABLED"`, `pub struct Trace` with `start` and `finish`. Task 4 consumes `RenderOptions`, `run`, and `CAPTURE_ENV`.

No unit test can cover this task without a GPU; the test is compiling and linting both shapes, and the real run in Task 5. Keep the module small and the two files single-purpose.

- [ ] **Step 1: Make the capture helpers public**

In `src/capture.rs`, change `fn is_gputrace_bundle` and `fn prepare_output` to `pub fn`, keeping their doc comments. Nothing else changes.

- [ ] **Step 2: Write `src/render/trace.rs`**

```rust
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

/// An in-progress capture. Every command buffer committed on the device
/// between [`Trace::start`] and [`Trace::finish`] lands in the bundle.
/// Dropping an unfinished `Trace` stops the capture, so a failure
/// mid-frame never leaves the process capturing while it unwinds.
pub struct Trace {
    manager: Retained<MTLCaptureManager>,
    capturing: bool,
}

impl Trace {
    /// Start writing a GPU trace document at `output` for every command
    /// buffer on `device`.
    pub fn start(device: &ProtocolObject<dyn MTLDevice>, output: &Path) -> Result<Trace> {
        // SAFETY: the shared manager is a process-wide singleton. objc2
        // marks the accessor unsafe because it cannot prove thread safety;
        // this tool uses it from one thread only.
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
```

- [ ] **Step 3: Add `RenderOptions` and `run` to `src/render/mod.rs`**

At the top of the file, replace the module doc and the two `use` lines with:

```rust
//! The librashader backend: render an image through a preset inside this
//! process and write the `.gputrace` with Metal's capture API. Sizing is
//! pure and tested; everything that touches Metal lives in [`run`] and
//! [`trace`], and is exercised only by a real run. This is the one module
//! in the crate that allows `unsafe`, for the Metal calls objc2 cannot
//! prove safe (descriptor construction and the byte upload).
#![allow(unsafe_code)]

mod trace;
pub use trace::{CAPTURE_ENV, Trace};

use crate::capture::{is_gputrace_bundle, prepare_output};
use crate::config::{Size, WindowMode};
use crate::display::Screen;
use anyhow::{Context, Result, anyhow, bail};
use librashader::presets::ShaderFeatures;
use librashader::runtime::Viewport;
use librashader::runtime::mtl::FilterChain;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin,
    MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

/// Everything [`run`] needs: the image, the preset, how to size the output,
/// how many frames to record, and where the bundle goes.
pub struct RenderOptions {
    /// The image to render. Decoded with the `image` crate.
    pub image: PathBuf,
    /// The `.slangp` preset.
    pub preset: PathBuf,
    /// Window mode from the command line, mapped to pixels by [`output_size`].
    pub window: WindowMode,
    /// The main display, for `Fill` and `Fullscreen`.
    pub screen: Screen,
    /// Number of frames to render and record; the frame count advances by one each.
    pub frames: u32,
    /// Absolute path of the `.gputrace` to write.
    pub output: PathBuf,
    /// Print the decoded and output sizes to stderr.
    pub verbose: bool,
}

/// Decode `path` to tightly packed BGRA8 rows, top row first.
fn decode_bgra(path: &Path) -> Result<(Size, Vec<u8>)> {
    let img = image::open(path)
        .with_context(|| format!("decoding {}", path.display()))?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let mut bytes = img.into_raw();
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok((Size { width, height }, bytes))
}

/// A BGRA8 2D texture of `size` with `usage`, in shared memory.
fn new_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    size: Size,
    usage: MTLTextureUsage,
    what: &str,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    // SAFETY: plain descriptor construction with a pixel format and a size.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            size.width as usize,
            size.height as usize,
            false,
        )
    };
    desc.setStorageMode(if cfg!(target_arch = "aarch64") {
        MTLStorageMode::Shared
    } else {
        MTLStorageMode::Managed
    });
    desc.setUsage(usage);
    device.newTextureWithDescriptor(&desc).ok_or_else(|| {
        anyhow!(
            "creating the {what} texture ({}x{})",
            size.width,
            size.height
        )
    })
}

/// Render `opts.frames` frames of the image through the preset and write
/// them to `opts.output` as a `.gputrace`.
pub fn run(opts: &RenderOptions) -> Result<()> {
    prepare_output(&opts.output)?;
    let (image_size, bytes) = decode_bgra(&opts.image)?;
    let size = output_size(&opts.window, image_size, &opts.screen);
    if opts.verbose {
        eprintln!(
            "image {}x{} -> output {}x{} px, {} frame(s)",
            image_size.width, image_size.height, size.width, size.height, opts.frames
        );
    }

    let device = MTLCreateSystemDefaultDevice().context("no Metal device is available")?;
    let queue = device
        .newCommandQueue()
        .context("creating a Metal command queue")?;

    let input = new_texture(&device, image_size, MTLTextureUsage::ShaderRead, "input")?;
    let region = MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width: image_size.width as usize,
            height: image_size.height as usize,
            depth: 1,
        },
    };
    let pixels = NonNull::new(bytes.as_ptr().cast_mut().cast())
        .context("decoded image buffer is null")?;
    // SAFETY: `bytes` holds exactly width * height * 4 bytes of BGRA8 with
    // rows of width * 4 bytes, matching `region` and the row stride, and
    // Metal copies them before returning.
    unsafe {
        input.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
            region,
            0,
            pixels,
            image_size.width as usize * 4,
        )
    };
    let output = new_texture(
        &device,
        size,
        MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead,
        "output",
    )?;

    let mut chain = FilterChain::load_from_path(&opts.preset, ShaderFeatures::NONE, &queue, None)
        .with_context(|| format!("loading preset {}", opts.preset.display()))?;
    let viewport = Viewport::new_render_target_sized_origin(&*output, None)
        .context("sizing the viewport to the output texture")?;

    let trace = Trace::start(&device, &opts.output)?;
    for frame in 0..opts.frames {
        let cmd = queue
            .commandBuffer()
            .context("creating a Metal command buffer")?;
        chain
            .frame(&input, &viewport, &cmd, frame as usize, None)
            .with_context(|| format!("rendering frame {frame}"))?;
        cmd.commit();
        cmd.waitUntilCompleted();
    }
    trace.finish();

    if !is_gputrace_bundle(&opts.output) {
        bail!(
            "the Metal capture finished but {} has no `index` entry; the bundle looks incomplete",
            opts.output.display()
        );
    }
    Ok(())
}
```

Keep `to_pixels`, `fit`, `output_size`, and the tests from Task 2 below this, unchanged.

- [ ] **Step 4: Build, lint, test both shapes**

Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test && cargo fmt --check`, then the three `--no-default-features` commands from Task 2 Step 5, then `cargo machete` (now clean) and `cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings`.
Expected: all pass. If the compiler rejects a method for a missing objc2-metal feature (the exact feature list is settled here), add that feature to `Cargo.toml` and note it in the commit message. If `device.as_ref()` is ambiguous, write `AsRef::<AnyObject>::as_ref(device)`.

- [ ] **Step 5: Commit**

```bash
git add src/render/mod.rs src/render/trace.rs src/capture.rs Cargo.toml Cargo.lock
git commit -m "Render an image through librashader's Metal runtime under MTLCaptureManager

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `--backend`, re-exec, and the librashader branch of `run`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `render::{RenderOptions, run, CAPTURE_ENV}` (Task 3), `display::main_screen` (Task 1), `image::is_image_path`, `image::EXTENSIONS`.
- Produces: `enum Backend { Retroarch, Librashader }`, `Cli.backend: Backend`, `fn needs_capture_env(Option<&OsStr>) -> bool`, `fn validate_image(&Path) -> Result<()>`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/main.rs`:

```rust
    #[test]
    fn backend_defaults_to_retroarch_and_requires_image() {
        assert_eq!(parse_rom(&[]).unwrap().backend, Backend::Retroarch);
        assert!(parse_rom(&["--backend", "librashader"]).is_err());
        assert!(parse_rom(&["--backend", "retroarch"]).is_err());
        let cli = parse_raw(&[
            "--image",
            "s.png",
            "--shader",
            "p.slangp",
            "--backend",
            "librashader",
        ])
        .unwrap();
        assert_eq!(cli.backend, Backend::Librashader);
        assert!(parse_raw(&["--image", "s.png", "--backend", "bogus"]).is_err());
    }

    #[test]
    fn capture_env_reexec_decision() {
        use std::ffi::OsStr;
        assert!(needs_capture_env(None));
        assert!(needs_capture_env(Some(OsStr::new("0"))));
        assert!(needs_capture_env(Some(OsStr::new(""))));
        assert!(!needs_capture_env(Some(OsStr::new("1"))));
    }

    #[test]
    fn validate_image_checks_extension_then_existence() {
        let err = validate_image(Path::new("Cargo.toml")).unwrap_err().to_string();
        assert!(err.contains("extension"), "{err}");
        let err = validate_image(Path::new("/nonexistent/x.png"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
        validate_image(Path::new("sample.png")).unwrap();
    }

    #[cfg(feature = "librashader")]
    #[test]
    fn librashader_backend_requires_a_shader_before_touching_metal() {
        let cli = parse_raw(&["--image", "sample.png", "--backend", "librashader"]).unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("--shader"), "{err}");
    }

    #[cfg(not(feature = "librashader"))]
    #[test]
    fn librashader_backend_is_refused_without_the_feature() {
        let cli = parse_raw(&["--image", "sample.png", "--backend", "librashader"]).unwrap();
        let err = run_librashader(&cli).unwrap_err().to_string();
        assert!(err.contains("librashader"), "{err}");
    }
```

Add `use std::path::Path;` to the test module's imports (`use super::*;` already brings `PathBuf`).

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --bin ra-metal-capture`
Expected: compile errors for `Backend`, `needs_capture_env`, `validate_image`, `run_librashader`.

- [ ] **Step 3: Implement**

Imports at the top of `src/main.rs`: add `use std::ffi::OsStr;` and, under `#[cfg(feature = "librashader")]`, `use ra_metal_capture::render;`. Change `use std::path::PathBuf;` to `use std::path::{Path, PathBuf};`.

Add the enum above `Cli`:

```rust
/// Which renderer image mode uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Backend {
    /// RetroArch's built-in image viewer, recorded with gpucapture
    Retroarch,
    /// librashader's Metal runtime in this process, recorded with MTLCaptureManager
    Librashader,
}
```

Add the field to `Cli`, after `image`:

```rust
    /// Renderer for --image: retroarch (default) or librashader
    #[arg(long, value_enum, default_value_t = Backend::Retroarch, requires = "image")]
    backend: Backend,
```

Update the `Cli` doc comment's first line to: "Launch RetroArch with a ROM and save state, or a static image, plus a shader preset, then capture frames to a .gputrace with gpucapture; or render a static image through librashader in-process."

Replace `main`:

```rust
fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(feature = "librashader")]
    if cli.backend == Backend::Librashader {
        reexec_with_capture_env()?;
    }
    run(cli)
}

/// Whether `MTL_CAPTURE_ENABLED` needs setting: Metal offers programmatic
/// capture to a trace document only when it is exactly `1` at load time.
fn needs_capture_env(current: Option<&OsStr>) -> bool {
    current != Some(OsStr::new("1"))
}

/// Replace this process with itself, same arguments, plus
/// `MTL_CAPTURE_ENABLED=1`, unless that is already set. `exec` only returns
/// on failure. After the re-exec the variable is `1`, so this never loops;
/// if Metal still refuses, `render::run` fails with a message naming it.
#[cfg(feature = "librashader")]
fn reexec_with_capture_env() -> Result<()> {
    use std::os::unix::process::CommandExt;
    if !needs_capture_env(std::env::var_os(render::CAPTURE_ENV).as_deref()) {
        return Ok(());
    }
    let exe = std::env::current_exe().context("locating this executable to re-exec it")?;
    let err = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .env(render::CAPTURE_ENV, "1")
        .exec();
    Err(err).with_context(|| {
        format!(
            "re-executing {} with {}=1",
            exe.display(),
            render::CAPTURE_ENV
        )
    })
}

/// `--image` must carry an extension the image viewer accepts and exist.
fn validate_image(image: &Path) -> Result<()> {
    if !is_image_path(image) {
        bail!(
            "{} does not have an image extension the image viewer accepts ({})",
            image.display(),
            EXTENSIONS.join(", ")
        );
    }
    if !image.is_file() {
        bail!("image not found at {}", image.display());
    }
    Ok(())
}

#[cfg(not(feature = "librashader"))]
fn run_librashader(_cli: &Cli) -> Result<()> {
    bail!(
        "this build has no librashader backend; reinstall with the \"librashader\" \
         feature (it is on by default)"
    )
}

#[cfg(feature = "librashader")]
fn run_librashader(cli: &Cli) -> Result<()> {
    let image = cli
        .image
        .as_deref()
        .context("--backend requires --image")?;
    validate_image(image)?;
    let preset = cli.shader.clone().context(
        "--backend librashader requires --shader: there is nothing to render without a preset",
    )?;
    if !preset.is_file() {
        bail!("shader preset not found at {}", preset.display());
    }
    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let opts = render::RenderOptions {
        image: image.to_path_buf(),
        preset,
        window: cli.window_mode(),
        screen: display::main_screen(),
        frames: cli.frames,
        output: output.clone(),
        verbose: cli.verbose,
    };
    render::run(&opts)?;
    println!("{}", output.display());
    Ok(())
}
```

In `run`, add as the first statement:

```rust
    if cli.backend == Backend::Librashader {
        return run_librashader(&cli);
    }
```

and replace the inline image checks in the `if let Some(image) = &cli.image` branch with `validate_image(image)?;` so both backends share one rule:

```rust
    let (core, content) = if let Some(image) = &cli.image {
        validate_image(image)?;
        (None, image.clone())
    } else {
```

`cli.window_mode()` for `Fill` calls `display::visible_size()`, which now goes through `main_screen()`; that is one AppKit query more than before and harmless.

- [ ] **Step 4: Run tests, clippy, fmt, both shapes**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`, then `cargo test --no-default-features && cargo clippy --no-default-features --all-targets -- -D warnings`.
Expected: all pass, including the feature-conditional test in each shape. If clippy flags the `#[cfg]` on the `if` in `main`, move the check into a small `#[cfg]`-gated helper `fn maybe_reexec(cli: &Cli) -> Result<()>` with an empty `Ok(())` twin under `#[cfg(not(feature = "librashader"))]`.

Smoke, without a GPU run: `cargo run -q -- --image sample.png --backend librashader --output /tmp/x.gputrace` prints the `--shader` message and exits non-zero; `cargo run -q -- --image sample.png --shader Cargo.toml --backend librashader --output /tmp/x.gputrace` gets as far as `loading preset Cargo.toml` and fails there (this one re-execs and creates a Metal device; skip it on a machine without one).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Add --backend librashader with the MTL_CAPTURE_ENABLED re-exec

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Real run, README, CHANGELOG, spec

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `docs/superpowers/specs/2026-09-10-librashader-backend-design.md`

- [ ] **Step 1: Real run**

```bash
cargo run -q -- --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --backend librashader --frames 2 --output /tmp/sample-vectorscale-ls.gputrace -v
```

Expected: stderr shows `image 160x144 -> output WxH px, 2 frame(s)` with W and H matching the fill computation for this display, stdout prints the output path, exit 0, `ls /tmp/sample-vectorscale-ls.gputrace/index` exists, and `pgrep -fl RetroArch` shows nothing new. Record `du -sh` of the bundle and the wall time. Open the bundle in Xcode (`open /tmp/sample-vectorscale-ls.gputrace`) and confirm two command buffers with the preset's passes. If `is_gputrace_bundle` rejects the bundle because the document destination lays it out differently, adjust that function to what was actually written and update its test and doc comment in the same commit.

Also confirm the RetroArch backend is untouched: rerun the README's existing image example (`--backend` omitted) and check it still produces a bundle.

- [ ] **Step 2: README**

Requirements: add a bullet:

```
- The default build compiles librashader and its C++ dependencies (glslang,
  SPIRV-Cross) from source, about <measured> s on an M-series Mac for a
  clean build. `cargo install --path . --no-default-features` skips them
  and gives a RetroArch-only tool without `--backend librashader`.
```

Usage: after the image example, add:

```
The same image can be rendered without RetroArch at all, through the
librashader crate's Metal runtime inside this process:

```
ra-metal-capture \
  --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --backend librashader \
  --output /tmp/sample-vectorscale-ls.gputrace
```
```

Options table: add after `--image`:

```
| `--backend NAME` | renderer for `--image`: `retroarch` (default) or `librashader`; requires `--image`; `librashader` requires `--shader` and ignores `--app`, `--config`, `--settle`, `--cmd-port`, `--keep-running` |
```

and amend the `--size` row: "exact window size in points (RetroArch) or output size in pixels (librashader)".

Image mode section: add a subsection `### Backends` after the frozen-image caveat paragraph:

```
`--backend librashader` renders the image through the same preset with the
[librashader](https://github.com/SnowflakePowered/librashader) crate's Metal
runtime, inside this process, and writes the trace with Metal's
`MTLCaptureManager` instead of `gpucapture`. RetroArch is not launched.
The tool re-executes itself once with `MTL_CAPTURE_ENABLED=1` so Metal
offers programmatic capture. `--frames N` renders N successive frames with
librashader's frame count advancing, so frame-count and history passes see
real prior frames, unlike the frozen image viewer. Nothing is presented to
a window, so Xcode shows the trace as a single frame holding N command
buffers, one per rendered frame.

Sizing is in pixels: `--size` is taken as pixels, `--scale N` is N times
the image, `--fullscreen` is the main display's full pixel size, and the
default fits the image into the visible area at the display's backing
scale, aspect preserved, matching RetroArch's fill mode. RetroArch and
librashader are different implementations of the preset format; the
librashader trace is of librashader's rendering, not a pixel-exact stand-in
for RetroArch's.

Verified <date>: `--image sample.png --backend librashader --frames 2`
with `vectorscale.slangp` produced a <size> bundle in <time> at <WxH> px.
```

Development: change the test line to:

```
cargo test                          # no RetroArch, Xcode, or GPU needed
cargo test --no-default-features    # the RetroArch-only shape
```

License: after the 0BSD paragraph, add:

```
### librashader

The default build links the librashader crates, which are MPL-2.0. MPL is
file-scoped: it covers those crates' own source, which is on crates.io,
and places no terms on this crate or on binaries built from it beyond
that. `--no-default-features` builds without them.
```

Fill in `<measured>`, `<date>`, `<size>`, `<time>`, `<WxH>` from Step 1 and Task 2's build timing.

- [ ] **Step 3: CHANGELOG**

Insert above `## 0.2.0 - 2026-09-10`:

```
## Unreleased

### Added

- `--backend librashader`: render `--image` through the shader preset
  inside this process with the librashader crate's Metal runtime and write
  the `.gputrace` with Metal's `MTLCaptureManager`. RetroArch is not
  launched. Requires `--shader`; `--frames N` records N successive frames
  with the frame count advancing. Behind the `librashader` Cargo feature,
  on by default; `--no-default-features` gives the RetroArch-only build.
- `display::Screen` and `display::main_screen()`: the main display's full
  frame and backing scale alongside the visible frame.
- `render` module (feature `librashader`): `RenderOptions`, `run`,
  `output_size`, `Trace`, `CAPTURE_ENV`.

### Changed

- `capture::prepare_output` and `capture::is_gputrace_bundle` are now
  public.
- Dependency policy: `deny.toml` allows MPL-2.0 and BSD-3-Clause for the
  librashader tree and warns rather than fails on duplicate crate versions.
- docs.rs documents the crate without default features, since the
  librashader tree's C++ does not cross-build there.
```

- [ ] **Step 4: Spec**

In the spec, replace the sentence "the actual build time is recorded by the plan's first real build" with the measured figure, and under "Known risks" mark the `index` layout and output texture usage items with what the real run showed (one sentence each, dated).

- [ ] **Step 5: Lint and commit**

Run: `./scripts/check-ascii.sh && typos && cargo fmt --check && cargo test`
Expected: clean.

```bash
git add README.md CHANGELOG.md docs/superpowers/specs/2026-09-10-librashader-backend-design.md
git commit -m "Document the librashader backend and record the sample.png run

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

Then follow `superpowers:finishing-a-development-branch`. The release (0.3.0: new feature, new public library items) goes through `RELEASING.md` separately.
