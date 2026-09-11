# ra-metal-capture

[![github](https://img.shields.io/badge/github-northbymidwest%2Fra--metal--capture-blue?logo=github)](https://github.com/northbymidwest/ra-metal-capture)
[![crates.io](https://img.shields.io/crates/v/ra-metal-capture.svg)](https://crates.io/crates/ra-metal-capture)
[![docs.rs](https://docs.rs/ra-metal-capture/badge.svg)](https://docs.rs/ra-metal-capture)
[![CI](https://github.com/northbymidwest/ra-metal-capture/actions/workflows/ci.yml/badge.svg)](https://github.com/northbymidwest/ra-metal-capture/actions/workflows/ci.yml)

Captures a Metal frame trace (`.gputrace`) from RetroArch. It launches a
RetroArch.app with a core, ROM, save state and shader preset, sizes the
window, and records presented frames with Apple's `gpucapture(1)`. RetroArch
runs Vulkan through MoltenVK, which is what makes the trace Metal. The
result opens in Xcode's GPU debugger like any other capture.

A static image can be captured instead, through RetroArch's built-in image
viewer, in place of an emulator core and ROM; see [Image mode](#image-mode).

With `--backend librashader` that same image, or a software-rendered
libretro core loaded with a ROM and a save state, is rendered through the
preset inside this process instead, with no RetroArch at all; see
[librashader backend](#librashader-backend).

## Requirements

- macOS 27 or newer, with Xcode 27 or newer installed. `gpucapture` is
  needed for the RetroArch backend (the default); the librashader backend
  needs Xcode for Metal's capture layer and to open the resulting trace.
  Older versions are untested and unsupported.
- For the RetroArch backend (the default), a RetroArch.app with its
  `video_driver` set to `vulkan` (the default on macOS). The tool has been
  run against the release, nightly, and debug builds. Not needed for
  `--backend librashader`.
- Rust 1.98 or newer to build.
- The default build compiles librashader and its C++ dependencies (glslang,
  SPIRV-Cross) from source, about 30 s on an M-series Mac for a clean
  build. `cargo install ra-metal-capture --no-default-features` skips them and
  gives a RetroArch-only tool without `--backend librashader`.

## Install

```
cargo install ra-metal-capture
```

From a checkout, `cargo install --path .` builds the same binary.

## Usage

```
ra-metal-capture \
  --core sameboy \
  --rom "path/to/game.gb" \
  --state "path/to/game.state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" \
  --output /tmp/game.gputrace
```

That launches `/Applications/RetroArch.app` with the SameBoy core, loads the
state, advances one frame, and writes a one-frame trace to
`/tmp/game.gputrace`. Without `--state` or `--slot` it instead waits for
the game to settle and captures whatever is on screen.

A static image can be captured the same way, without a core or ROM:

```
ra-metal-capture \
  --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --output /tmp/sample-vectorscale.gputrace
```

`sample.png` is the 160x144 fixture tracked in this repository; any PNG (or
other accepted image format) works in its place. `--image` replaces
`--core` and `--rom` with a file loaded through RetroArch's built-in image
viewer; see [Image mode](#image-mode) below.

The same image can be rendered without RetroArch at all, through the
librashader crate's Metal runtime inside this process:

```
ra-metal-capture \
  --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --backend librashader \
  --output /tmp/sample-vectorscale-ls.gputrace
```

A libretro core can be hosted the same way, with no RetroArch process:

```
ra-metal-capture \
  --backend librashader \
  --core sameboy \
  --rom "path/to/game.gbc" \
  --state "path/to/game.state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --output /tmp/game-ls.gputrace
```

| flag | meaning |
|---|---|
| `--app PATH` | `.app` bundle or its binary. Default `/Applications/RetroArch.app`. |
| `--core CORE` | `.dylib` path, or a bare name resolved in `libretro_directory` (`sameboy` finds `sameboy_libretro.dylib`). |
| `--rom PATH` | content to load |
| `--image FILE` | static image via RetroArch's image viewer; replaces `--core` and `--rom`; incompatible with `--state`, `--slot`, `--advance` |
| `--backend NAME` | renderer: `retroarch` (default) or `librashader`; `librashader` requires `--shader` and takes either `--image` or `--core` with `--rom`; ignores `--app`, `--cmd-port`, `--keep-running` |
| `--output PATH` | output `.gputrace` path (required) |
| `--state FILE` | save state to load; copied to a temp states dir as slot 0, or under librashader restored into the hosted core after the ROM loads |
| `--slot N` | load slot N from your real states dir instead; under librashader the file is found the way RetroArch names it under `savestate_directory` |
| `--shader PRESET` | `.slangp` / `.glslp` passed via `--set-shader` |
| `--config PATH` | `retroarch.cfg` to base the run on; default `~/Library/Application Support/RetroArch/config/retroarch.cfg` |
| `--size WxH` | exact window size in points (RetroArch) or output size in pixels (librashader) |
| `--scale N` | integer scale of the core's native resolution |
| `--fullscreen` | launch with `-f` |
| `--settle SECS` | wait before capturing when no state is given (default 5); under librashader, `round(SECS * fps)` emulated frames run before recording instead |
| `--advance N` | frame advances after loading the state, before the capture is armed (default 1, min 1); only applies when `--state` or `--slot` is given; under librashader, emulated frames run after the state and the Nth is the recorded one |
| `--cmd-port PORT` | UDP port for RetroArch's command interface, enabled only for this run (default 55355) |
| `--frames N` | frame boundaries to record (RetroArch) or frames to render with the frame count advancing (librashader) (default 1) |
| `--keep-running` | do not close RetroArch afterwards |
| `-v` | print the command line and appendconfig, pass `-v` to RetroArch |

By default the window is windowed and as large as fits the main display
(RetroArch scales the core output up and clamps it to the display's visible
area, keeping the aspect ratio).

## How it works

RetroArch is exec'd directly, not via `open`, with `MTL_CAPTURE_ENABLED=1`
in its environment so GPUToolsCapture loads into the process. Its command
line carries the core, the shader, the ROM, and an `--appendconfig` that
the tool writes for this run (in image mode there is no core, and the
image is the content). The appendconfig sets the window size, keeps
RetroArch rendering when unfocused, turns off config-save-on-exit and
savestate auto-save and auto-load, and hides the load-content animation and
on-screen text so nothing lands in the captured frame.

**Without a state**, the tool polls `gpucapture list` until the process is
capturable, waits `--settle` seconds, then runs `gpucapture start`, which
blocks until the trace is written.

**With a state**, the appendconfig also enables RetroArch's UDP command
interface for this run and points `state_slot` at the slot to load. Once
the process is capturable the tool pauses RetroArch over that interface,
sends `LOAD_STATE`, frame-advances `--advance` times, and arms `gpucapture`.
A paused RetroArch only presents on a frame advance, and `gpucapture` needs
a few presents to open and close a frame, so the tool keeps advancing until
the capture reports done. The recorded frame is the one `--advance`
advances past the loaded state; the closing advances happen after it.

Afterwards RetroArch is asked to quit, escalating to SIGTERM and then
SIGKILL if it does not. A drop guard kills it on any failure, so no process
is left behind.

## Image mode

`--image FILE` loads a static image through RetroArch's built-in image
viewer core instead of an emulator core and ROM. The viewer reports the
image's own pixel dimensions as its content geometry, so a 160x144 PNG is
treated exactly like a Game Boy frame: window sizing, integer scale, and
shader passes all behave the same as they would with an emulator core. The
viewer presents continuously, so the same settle flow used without a save
state captures it; `--state`, `--slot`, and `--advance` are not accepted
with `--image`.

To capture a raw, pre-shader frame from a save state instead of a plain
image file, load the state in RetroArch and take a screenshot with
`video_gpu_screenshot = "false"` set (the "GPU Screenshot" toggle under
Settings, Video; turning it off makes RetroArch's screenshot hotkey save
the core's raw framebuffer instead of the shaded viewport). RetroArch's
own `.state.png` thumbnail is post-shader and unsuitable as input here.

Because the image viewer never advances a frame counter, shader passes
that depend on frame count or a history of prior frames see a permanently
frozen image rather than the animation they would see under a running core.

## librashader backend

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

### Cores

Under `--backend librashader`, `--core` and `--rom` load the libretro core
into this process instead of launching RetroArch. The core runs headless:
no window, no input (every button reads as released, which is what makes
the run repeatable), no audio. `--state` restores a RetroArch save state
(RetroArch's compressed and container formats are both read); `--slot N`
finds it the way RetroArch names it under `savestate_directory`. The
core's options come from RetroArch's per-core options file, so the core
renders with the same settings RetroArch would use. `--advance N` then
runs N frames, the last of which is recorded, matching the RetroArch
backend; without a state, `--settle` seconds of frames run first. Every
frame before the recorded one still passes through the preset, so
history-dependent passes see real prior frames, and `--frames N` records N
consecutive emulated frames.

Warm-up frames leave the preset's history and feedback textures holding
real data, which the trace records, so a run with many warm-up frames
(`--advance 30`, or the default settle) writes a bundle roughly three
times the size of one with none.

Only software-rendered cores are supported; a core that asks for an
OpenGL or Vulkan context is refused. Zipped ROMs must be extracted first.
Cores that need BIOS files read them from `system_directory`.

## Development

```
cargo test                          # no RetroArch, Xcode, or GPU needed
cargo test --no-default-features    # the RetroArch-only shape
```

Pre-commit rustfmt hook: `git config core.hooksPath .githooks`. The
repository is ASCII only (`scripts/check-ascii.sh`).

## License

[BSD Zero Clause License](LICENSE)

### Why 0BSD?

The majority of this codebase was generated by AI coding agents (primarily
Claude). AI-generated code is not copyrightable and is effectively public
domain, making 0BSD, which imposes no restrictions on use, the most
appropriate license.

### Disclaimer

While AI-generated code itself is public domain, AI agents may have reproduced
or closely derived code from copyrighted sources (training data, reference
implementations, open-source projects, etc.). No audit has been conducted to
identify such instances, as this is a personal side project. Any such code
fragments remain subject to the licenses of their original creators. Use at
your own discretion.

### librashader

The default build links the librashader crates, which are MPL-2.0. MPL is
file-scoped: it covers those crates' own source, which is on crates.io,
and places no terms on this crate or on binaries built from it beyond
that. `--no-default-features` builds without them.
