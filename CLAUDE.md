# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A macOS-only Rust CLI that records a Metal frame trace (`.gputrace`) of a
shader preset. Two backends: render through the librashader crate's Metal
runtime in this process and write the trace with `MTLCaptureManager`
(`--backend librashader`, the default whenever the feature is compiled
in), or launch RetroArch.app and capture its presented frames with Apple's
`gpucapture(1)` (`--backend retroarch`, the default only in a
`--no-default-features` build). The librashader backend
renders either a static image (`--image FILE`) or a software-rendered
libretro core hosted in this same process (`--core` and `--rom`, with
`--state` or `--slot` to restore a RetroArch save state).
Requires macOS 27 and Xcode 27 at run time. Published on crates.io as
`ra-metal-capture`.

## Commands

```
cargo build
cargo test                                  # no RetroArch, Xcode, or GPU needed
cargo test --no-default-features            # the RetroArch-only shape
cargo test --lib render                     # one module's tests
cargo test image_mode_parses                # one test by name
cargo clippy --all-targets -- -D warnings
cargo fmt --check                           # the pre-commit hook enforces this
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
./scripts/check-ascii.sh                    # every tracked file must be ASCII
cargo deny check licenses bans sources
cargo machete
```

CI runs every check above in both feature shapes (default and
`--no-default-features`); keep both green. `cargo test` must never construct
a Metal device; anything that touches the GPU is verified by a real run,
not a test.

Real run against the fixture (re-execs itself with `MTL_CAPTURE_ENABLED=1`):

```
cargo run -q -- --image fixtures/sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --output /tmp/sample.gputrace -v
```

The same command with `--backend retroarch` launches
`/Applications/RetroArch.app` and captures through `gpucapture`.

## Rules that are easy to miss

- ASCII only in every tracked file: no em dashes, en dashes, arrows, or
  typographic quotes. `scripts/check-ascii.sh` fails CI otherwise.
- `#![deny(unsafe_code)]` in `lib.rs` and `main.rs`. `unsafe` is allowed only
  inside `src/render/` and `src/libretro/`, each block with a `// SAFETY:`
  comment. The six `extern "C"` libretro callbacks in `src/libretro/env.rs`
  are the only hand-written C ABI; they must never unwind.
- All Metal and AppKit calls go through `objc2-metal`, `objc2-foundation`,
  and `objc2-app-kit`. Never write an `extern` block or hand-rolled FFI.
- Under `MTL_CAPTURE_ENABLED=1` Metal wraps the device in a capture proxy
  class that does not statically declare every `MTLDevice` selector, and
  objc2's debug-build method check panics on one it cannot find
  (`hasUnifiedMemory` did; `supportsFamily` works). Any new device call in
  `src/render/` needs a real debug-build run before it is trusted.
- Never write the user's `retroarch.cfg` or savestate directory. Every
  RetroArch override goes into a per-run appendconfig in a temp dir, and a
  `--state` file is copied there rather than loaded in place.
- `deny.toml` lists exactly the licenses the tree uses with
  `unused-allowed-license = "deny"`; adding or dropping a dependency may
  require editing that list.
- docs.rs builds with `no-default-features` (the librashader tree's C++
  cannot cross-build there), so crate-level docs must not intra-doc-link
  into the `render` module.
- Commit messages end with a `Co-Authored-By:` trailer for the agent that
  wrote them. Design specs and implementation plans live under
  `docs/superpowers/` and are the record of why things are the way they are.

## Architecture

The binary in `src/main.rs` parses args with clap, turns them into a
backend-neutral `backend::Request` (a `Source` that is an image or a core
with its ROM, state, and options, plus preset, window mode, frame counts,
output, and the config path), rejects flags meant for the other backend by
name, and picks a `backend::Backend`: `retroarch::RetroArch` (app, command
port, keep-running) or `hosted::Hosted`. `prepare` runs first (the hosted
backend re-execs with `MTL_CAPTURE_ENABLED=1`), then `run`, which prints
the bundle path as its last act. Main knows nothing about launch plans,
cores, or Metal.

`retroarch.rs` assembles a `launch::LaunchPlan` plus a
`config::AppendConfig`, renders the appendconfig to a temp dir, builds a
`launch::LaunchCommand`, and hands it to `capture::run` with a
`capture::CaptureOptions`. `hosted.rs` resolves the core and state through
`layout`, drives `libretro::Core`, and hands a `render::FrameSource` to
`render::run`. `bundle` (recognising and clearing a `.gputrace`) is shared.

`capture::run` spawns RetroArch under a drop guard that SIGKILLs it on any
failure path, polls `gpucapture list` until the pid is capturable, then
runs one of two triggers. **Settle** (no state) sleeps and runs
`gpucapture start`. **Paused** (`--state` or `--slot`) connects to
RetroArch's UDP command interface via `remote::Remote`, pauses, sends
`LOAD_STATE`, frame-advances, arms `gpucapture` on a thread, and keeps
advancing until the capture closes, because a paused RetroArch only
presents on a frame advance. The measured constants and their reasons are
in the doc comments on `capture.rs` and in the spec.

`libretro/mod.rs` hosts a core in this process in four steps: `Core::open`
dlopens the `.dylib` through `libloading`, resolves every `retro_*` symbol,
and checks `retro_api_version`; `Core::system_info` names the core, which is
what picks its state directory, and libretro.h allows that call before
`retro_init`; `Core::init` publishes the `Context` to the
callbacks, installs them, and calls `retro_init`, so a core that reads its
options that early sees the real values; and `Core::load_game` loads the ROM
and returns `AvInfo`. Then `Core::restore` feeds a decoded state to
`retro_unserialize`, and `Core::run_frame` runs one `retro_run` and returns
the `Frame` the core delivered. libretro's callbacks carry no user pointer,
so everything they need lives in a process-wide static in `libretro/env.rs`,
and `Core::open` therefore refuses a second core in the same process;
`Drop` unloads the game, deinitializes what `init` initialized, and releases
that slot. `Context` carries what the `environment` callback answers
from (system and save directories, and core options from `--core-options`,
empty by default). Inferred paths (a bare core name, `--slot`, the system
directory) go through `DirResolver` in `main.rs`: RetroArch's macOS default
layout first, `retroarch.cfg` parsed lazily only on a miss. A `Core` is a `render::FrameSource`, which is how
emulated frames reach the render loop.

`render/mod.rs` builds BGRA8 textures, loads the preset with
`librashader::runtime::mtl::FilterChain`, and runs a two-phase loop over a
`FrameSource`: `warmup` frames are rendered before `Trace::start`, so
history and frame-count passes see real prior frames, then `--frames`
command buffers are rendered between `Trace::start` and `Trace::finish`
(`render/trace.rs`, a guard over `MTLCaptureManager` whose drop stops an
unfinished capture). The frame count advances across both phases. Preset
compilation happens before the capture starts so only the recorded frame
command buffers land in the bundle. `output_size` maps the shared window
modes to pixels (RetroArch's are points): `Exact` is pixels as given,
`Scale` multiplies the source size, `Fullscreen` and the default fill use
`display::Screen`'s backing scale.

`config::AppendConfig` is the single place RetroArch settings are
overridden; `config::WindowMode` is shared by both backends. `display`
queries the main screen through AppKit and falls back to 1920x1080 at 1x
off the main thread, which is what every test sees.

Unit tests live inline and are deterministic: config rendering, argument
parsing and conflicts, output sizing, the `gpucapture list` parser, and the
UDP protocol against a fake on 127.0.0.1. The one GPU-dependent module,
`render`, has no unit tests by design.

## Releasing

`RELEASING.md` covers it: bump `Cargo.toml`, retitle the `## Unreleased`
CHANGELOG section, push, then dispatch the `release` workflow. Every
release is a `0.x` pre-release.
