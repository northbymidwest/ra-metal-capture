# librashader backend design

A second backend for image mode. Today `--image FILE` loads a static image
through RetroArch's built-in image viewer and records RetroArch's presented
frames with `gpucapture(1)`. This design adds `--backend librashader`, which
renders the same image through the same `.slangp` preset inside this
process, using the `librashader` crate's Metal runtime, and writes the
`.gputrace` itself through Metal's `MTLCaptureManager`. No RetroArch, no
window, no `gpucapture`.

Both backends stay available. `--backend retroarch` remains the default and
is unchanged.

Decided 2026-09-10: capture is in-process via `MTLCaptureManager` (not a
child process presenting to a window under `gpucapture`), and the backend
is a Cargo feature that is on by default. `MTLCaptureManager`, `MTLDevice`,
textures, and command buffers all come from the `objc2-metal` crate; this
crate declares no FFI of its own.

## Goals

- `ra-metal-capture --image sample.png --shader crt.slangp --backend
  librashader --output out.gputrace` produces a `.gputrace` of that image
  under that preset, repeatably, with no RetroArch installed.
- The same sizing vocabulary as the RetroArch backend (`--size`, `--scale`,
  `--fullscreen`, or fill the display by default), so a preset can be
  captured under both backends at a comparable output size.
- `--frames N` records N successive frames with librashader's frame count
  advancing, so passes that depend on frame count or frame history are
  exercised rather than frozen (the documented limitation of the RetroArch
  image viewer).
- A build without the feature is exactly today's tool.

## Non-goals

- Replacing the RetroArch backend, or using librashader for ROM and
  save-state captures. Only image mode gets a second backend.
- Matching RetroArch's output pixel for pixel. RetroArch and librashader
  are different implementations of the same preset format; the goal is a
  trace of librashader's rendering, not a diff tool.
- Shader parameter overrides (`--param name=value`). librashader supports
  them; nothing here needs them yet.
- Warm-up frames rendered before the capture starts. `--frames N` covers
  history within one trace; a separate warm-up count can come later if a
  preset needs many frames of history before the interesting one.
- Linux or Windows. Metal only.

## CLI

New in the option list:

```
  --backend <BACKEND>   Which renderer image mode uses: retroarch (default)
                        or librashader. Requires --image. librashader
                        requires --shader.
```

Rules, enforced by clap where it can express them and in `run` otherwise:

- `--backend` requires `--image`. With `--core`/`--rom` it is an error.
- `--backend librashader` requires `--shader`: with no preset there is
  nothing to render. Checked in `run`, since clap cannot condition
  `requires` on an argument's value.
- In a build without the `librashader` feature, `--backend librashader`
  fails in `run` with: `this build has no librashader backend; reinstall
  with the "librashader" feature (it is on by default)`.
- `--state`, `--slot`, and `--advance` already conflict with `--image`.
- `--frames N` applies to both backends.
- `--size`, `--scale`, `--fullscreen` apply to both backends, with the
  pixel semantics described under Sizing.
- `--app`, `--config`, `--settle`, `--cmd-port`, and `--keep-running` are
  RetroArch-only. Under `--backend librashader` they are ignored, and the
  README's table says so. Detecting an explicitly passed default-valued flag
  needs clap's `ArgMatches`; not worth it for flags that do no harm.
- `-v` prints the decoded image size, the chosen output size, and the
  frame count.

Exit status is non-zero, with a one-line reason on stderr, when: the image
cannot be decoded; the preset fails to parse or compile; no Metal device
is available; the capture destination is unsupported even after the
re-exec (see Capture); `startCapture` returns an error; or the bundle has
no `index` afterwards.

## Cargo feature

```toml
[features]
default = ["librashader"]
librashader = ["dep:librashader", "dep:objc2", "dep:objc2-metal", "dep:image"]

[dependencies]
librashader = { version = "0.12.0", optional = true, default-features = false,
                features = ["runtime-metal", "presets", "preprocess"] }
objc2 = { version = "0.6", optional = true }
objc2-metal = { version = "0.3.2", optional = true,
                features = ["MTLCaptureManager", "MTLCaptureScope", "MTLDevice",
                            "MTLCommandQueue", "MTLCommandBuffer", "MTLTexture",
                            "MTLResource", "MTLTypes"] }
image = { version = "0.25", optional = true, default-features = false,
          features = ["png", "jpeg", "bmp", "gif", "tga", "pnm", "hdr"] }
```

`objc2-foundation` (already a dependency) gains the `NSURL`, `NSString`,
and `NSError` features for the capture descriptor's output URL and error.
The exact objc2-metal feature list is settled by the compiler during Task 3
of the plan; the set above is the expected one.

`librashader` 0.12.0 is the current release on crates.io. With only the
Metal runtime enabled its tree resolves to 159 packages and builds glslang
and SPIRV-Cross from C++ source through `cc`, which needs the Xcode command
line tools that any machine with `gpucapture` already has. Measured
2026-09-10 in a scratch project on an M-series Mac: a clean build of that
tree takes 25.6 s wall, 159 s CPU.

Why on by default: the user of this tool wants both backends from one
`cargo install`. `--no-default-features` gives the RetroArch-only build for
anyone who does not.

## Components

### `render` (new, `#[cfg(feature = "librashader")]`)

`src/render/mod.rs` with two submodules. The whole module carries
`#![allow(unsafe_code)]`; it is the only place in the crate that allows it.
`lib.rs` and `main.rs` keep `#![deny(unsafe_code)]`.

**`render::output_size`** (pure, in `mod.rs`):

```rust
pub fn output_size(mode: &WindowMode, image: Size, screen: &display::Screen) -> Size
```

See Sizing.

**`render::RenderOptions`**:

```rust
pub struct RenderOptions {
    pub image: PathBuf,
    pub preset: PathBuf,
    pub window: WindowMode,  // from the command line; mapped to pixels inside run
    pub screen: Screen,      // display::main_screen()
    pub frames: u32,
    pub output: PathBuf,     // absolute .gputrace path
    pub verbose: bool,
}
```

`run` takes the window mode rather than a pixel size because the fill
computation needs the image's dimensions, and the image is decoded once,
inside `run`.

**`render::run(&RenderOptions) -> Result<()>`** does, in order:

1. `capture::prepare_output(&output)` (made `pub`): a previous bundle is
   removed, anything else there is refused.
2. Decode the image with `image::open` into RGBA8, then swap to BGRA8.
   Errors carry the path. Compute the output size with `output_size`.
3. `MTLCreateSystemDefaultDevice`, `newCommandQueue`. Bail if either is
   `None`.
4. Input texture: `BGRA8Unorm`, image size, `Shared` storage, usage
   `ShaderRead`; `replaceRegion` uploads the bytes. Output texture:
   `BGRA8Unorm`, the computed output size, `Shared` storage, usage
   `RenderTarget | ShaderRead`.
5. `librashader::runtime::mtl::FilterChain::load_from_path(preset,
   ShaderFeatures::NONE, &queue, None)`. Errors are librashader's, wrapped
   with the preset path.
6. `Viewport::new_render_target_sized_origin(output_texture, None)`.
7. Begin the capture (`render::trace`, below).
8. For `frame in 0..frames`: one command buffer from the queue,
   `chain.frame(&input, &viewport, &cmd, frame as usize, None)`, `commit`,
   `waitUntilCompleted`. Bail on a `None` command buffer or a frame error.
9. End the capture. Fail if `output/index` does not exist.

**`render::trace`** (`src/render/trace.rs`) wraps `MTLCaptureManager`:

```rust
pub struct Trace { manager: Retained<MTLCaptureManager> }
impl Trace {
    /// Start writing a GPU trace document for every command buffer on `device`.
    pub fn start(device: &ProtocolObject<dyn MTLDevice>, output: &Path) -> Result<Trace>;
    /// Stop the capture and let Metal finish writing the bundle.
    pub fn finish(self);
}
impl Drop for Trace { /* stopCapture if still capturing */ }
```

`start` gets `MTLCaptureManager::sharedCaptureManager()`, checks
`supportsDestination(MTLCaptureDestination::GPUTraceDocument)` and bails
with a message naming `MTL_CAPTURE_ENABLED` if false, builds an
`MTLCaptureDescriptor` with `setCaptureObject(device)`,
`setDestination(GPUTraceDocument)`, `setOutputURL(NSURL::fileURLWithPath)`,
and calls `startCaptureWithDescriptor_error`, converting the `NSError` into
an `anyhow` error. `finish` calls `stopCapture` and disarms the guard; the
`index` check that follows in `run` is what reports an incomplete bundle. The
guard exists so an error mid-frame-loop still closes the capture and does
not leave the process in a capturing state while it unwinds.

### `display` - main screen

Gains a pixel-aware description of the main display alongside the existing
`visible_size()`:

```rust
pub struct Screen {
    pub visible: Size,     // points, excludes menu bar and dock
    pub full: Size,        // points, the whole display
    pub backing_scale: f64 // NSScreen backingScaleFactor, e.g. 2.0
}
pub fn main_screen() -> Screen
```

Falls back to 1920x1080, scale 1.0, with the same warnings as
`visible_size()`. `visible_size()` and `fill_mode()` stay as they are; the
RetroArch backend is untouched.

### `main`

- `Backend` enum (`clap::ValueEnum`), `Retroarch` (default) and
  `Librashader`, with `--backend` carrying `requires = "image"`.
- `run` branches early: when the backend is librashader, it validates the
  preset path and `--shader` presence, computes the window mode and the
  output size, and hands a `RenderOptions` to `render::run`. The RetroArch
  path is unchanged from 0.2.0.
- The `MTL_CAPTURE_ENABLED` re-exec happens in `main` before `run`, and
  before anything touches Metal; see Capture.

### `capture`

`prepare_output` and `is_gputrace_bundle` become `pub` so `render` reuses
them. Nothing else changes.

### `image`

Unchanged. The extension check (RetroArch's image viewer list) applies to
both backends so one rule governs what `--image` accepts. Under librashader,
`psd` and `pic` pass the check but fail to decode, since the `image` crate
does not read them; the decoder's error is surfaced with the path.

## Sizing

RetroArch's window modes are in points, and on a Retina display the
presented frame is that size times the backing scale. librashader renders
into a texture whose size is chosen directly, in pixels. `output_size`
maps each mode to pixels so both backends produce a similarly sized frame:

| mode | output size (pixels) |
|---|---|
| `Fill { max }` (default) | image scaled by `min(max.w*s / img.w, max.h*s / img.h)`, floored, where `s` is the backing scale and `max` is the visible area minus the title bar, as today |
| `Exact(size)` (`--size WxH`) | `size` as given, in pixels, not points |
| `Scale(n)` (`--scale N`) | `img * n` |
| `Fullscreen` (`--fullscreen`) | `full * s` |

`--size` is the one deliberate difference: under RetroArch it is points,
under librashader it is pixels. The README's table says so. Fill scales
down as well as up, as RetroArch's clamp does for an image larger than
the display. The fill computation floors after multiplying and never
produces an empty texture: a `max` with a zero dimension, or a factor that
floors a dimension to zero, falls back to the image's own size.

## Capture

Programmatic capture to a trace document is only offered when
`MTL_CAPTURE_ENABLED=1` is in the process environment at startup, before
the Metal framework loads. So, in `main`, when the parsed backend is
librashader and that variable is unset, the tool re-executes itself with it
set: `std::os::unix::process::CommandExt::exec` on `current_exe()` with
`args_os()` and the variable added. `exec` replaces the process image, so
there is no child to guard. If the variable is already set (by the user, or
by the re-exec) and `supportsDestination` still says no, the tool fails
with a message naming the variable, so a broken environment cannot loop.

The capture object is the device, so every command buffer committed between
`start` and `finish` lands in the trace: exactly the `--frames` frame
buffers. There are no presents, so Xcode shows the trace as one frame
containing N command buffers rather than N presented frames. The README
says so.

`GPUToolsCapture.framework`, which writes the bundle, is a private system
framework present on this machine under `/System/Library/PrivateFrameworks`
(checked 2026-09-10). Whether it is present without Xcode installed is
unverified and does not matter: the tool already requires Xcode for
`gpucapture`.

## Data flow

```
args
  -> (librashader backend, env unset) exec self with MTL_CAPTURE_ENABLED=1
  -> validate image path, preset path
  -> decode image -> Size
  -> output_size(window mode, image size, main_screen())
  -> prepare_output
  -> device, queue, input + output textures, FilterChain::load_from_path
  -> Trace::start(device, output)
  -> for f in 0..frames: command buffer, chain.frame(f), commit, wait
  -> Trace::finish -> check output/index
  -> print output path
```

## Error handling

`anyhow` throughout, with context naming the image, preset, or output path
on every step. `Trace` stops the capture on drop so no failure leaves a
capture open. Metal calls that return `Option` (device, queue, texture,
command buffer) bail with a message naming the object. `NSError` from
`startCaptureWithDescriptor_error` is rendered through its
`localizedDescription`.

## Testing

Unit tests, no GPU, RetroArch, or Xcode required (`cargo test` stays
deterministic in CI):

- `render::output_size`: each of the four modes at scale 1.0 and 2.0,
  including a Fill that is width-limited, one that is height-limited, and
  a degenerate max that falls back to the image size.
- `main`: `--backend` defaults to retroarch; `--backend librashader`
  without `--image` is rejected; with `--image` and `--shader` parses;
  `--backend` with `--core --rom` is rejected.
- The re-exec decision: a pure `fn needs_capture_env(var: Option<&OsStr>)
  -> bool` tested for unset, `1`, and other values.

Both feature shapes compile and test in CI: the `macos` job runs `cargo
build`, `clippy`, `test`, and `doc` as today (default features) and adds
`cargo build --no-default-features` plus `cargo test --no-default-features`.
No test constructs a Metal device.

Manual end-to-end check, recorded in the README with the measured bundle
size and wall time:

```
ra-metal-capture --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --backend librashader --frames 2 --output /tmp/sample-vectorscale-ls.gputrace -v
```

Success is a bundle that opens in Xcode's GPU debugger showing two command
buffers with the preset's passes, and that `gputrace-bundle` can list
textures from.

## Repository policy changes

- **deny.toml**: `allow` gains `MPL-2.0` (librashader and its crates,
  persy, smartstring) and `BSD-3-Clause` (encoding_rs is `(Apache-2.0 OR
  MIT) AND BSD-3-Clause`). `unused-allowed-license = "deny"` stays, so the
  list remains honest. `multiple-versions` drops from `deny` to `warn`:
  librashader's tree carries ten duplicate crate versions (`syn`,
  `thiserror`, `hashbrown`, and others) that this crate cannot resolve; a
  `skip` list would go stale on every librashader release. `wildcards` and
  `[sources]` are unchanged.
- **License section of the README**: a sentence that the default build
  links librashader under MPL-2.0, that MPL is file-scoped so the 0BSD
  terms of this crate are unaffected, and that librashader's source is on
  crates.io.
- **docs.rs**: `[package.metadata.docs.rs]` gains `no-default-features =
  true`. docs.rs builds on Linux with an Apple cross target, and `cargo
  doc` runs dependency build scripts, so the glslang and SPIRV-Cross C++
  builds would fail there. The `render` module is therefore absent from
  docs.rs; the README documents the backend.
- **CI**: the `macos` job gains the two `--no-default-features` steps. The
  `msrv` job builds with default features; librashader declares no MSRV
  and 1.98 is above anything in its tree.
- **typos.toml**: `librashader` and any identifiers the crate forces
  (`slangp` is already fine) go under `extend-words` only if typos flags
  them.

## Toolchain and dependencies

Additions to the table in the 2026-09-09 design:

| crate | version | use |
|---|---|---|
| librashader | 0.12 (runtime-metal, presets, preprocess) | preset parsing, compilation, Metal filter chain |
| objc2-metal | 0.3.2 | device, queue, textures, command buffers, MTLCaptureManager |
| objc2 | 0.6 | `Retained`, `ProtocolObject`, `AnyObject` for the above |
| image | 0.25 | decoding the input image |

All four are optional behind the `librashader` feature. `unsafe` is
confined to `src/render/`.

## Known risks

- **`index` layout.** The bundle written by `MTLCaptureManager` is assumed
  to have the same `index` entry `gpucapture` writes, which is what
  `is_gputrace_bundle` and the post-capture check look for. Verified by the
  first real run; if it differs, the check is adjusted to whatever the
  document destination actually writes.
- **Re-exec and `cargo run`.** `current_exe()` under `cargo run` is the
  target binary, so the re-exec works there too, but any wrapper that
  replaces `argv[0]` semantics (a shim script) is outside what is tested.
- **Output texture usage.** librashader's final pass draws into the
  viewport texture with a render encoder, so `RenderTarget` usage is
  required; librashader's own CLI sets `ShaderWrite` and works, so Metal
  may be lenient here. The design sets `RenderTarget | ShaderRead`
  explicitly and the first real run confirms it.
- **Build time.** glslang and SPIRV-Cross compile from C++ on every clean
  build (25.6 s wall for the dependency tree alone, measured 2026-09-10).
  The README's Requirements section says so, so nobody is surprised by
  `cargo install`.
- **RetroArch stays on `gpucapture`.** `MTLCaptureManager` is an
  in-process API; using it for RetroArch would mean injecting code into
  RetroArch's process (a `DYLD_INSERT_LIBRARIES` shim, blocked by the
  hardened runtime on most builds, or a patched RetroArch), which the
  2026-09-09 design rules out. `gpucapture` is Apple's out-of-process front
  end to the same capture machinery and remains the RetroArch path.
