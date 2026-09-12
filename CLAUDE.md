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
`ra-metal-capture entitle [--app PATH]` re-signs a RetroArch.app ad hoc
with `com.apple.security.get-task-allow`, which `gpucapture` needs and
libretro's builds lack. Requires macOS 27 and Xcode 27 at run time.
Published on crates.io as `ra-metal-capture`.

## Commands

```
cargo build
cargo test                                  # no RetroArch, Xcode, or GPU needed
cargo test --no-default-features            # the RetroArch-only shape
cargo test --lib hosted::render             # one module's tests
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
  inside `src/hosted/render/` and `src/hosted/libretro/`, each block with a `// SAFETY:`
  comment. The six `extern "C"` libretro callbacks in `src/hosted/libretro/env.rs`
  are the only hand-written C ABI; they must never unwind.
- All Metal and AppKit calls go through `objc2-metal`, `objc2-foundation`,
  and `objc2-app-kit`. Never write an `extern` block or hand-rolled FFI.
- Under `MTL_CAPTURE_ENABLED=1` Metal wraps the device in a capture proxy
  class that does not statically declare every `MTLDevice` selector, and
  objc2's debug-build method check panics on one it cannot find
  (`hasUnifiedMemory` did; `supportsFamily` works). Any new device call in
  `src/hosted/render/` needs a real debug-build run before it is trusted.
- The user's `retroarch.cfg` is read for one thing only: locating a bare
  core name, a `--slot`, or the system directory when it is not in
  RetroArch's default place (`layout::DirResolver`, defaults first). It is
  never written, and neither are the user's states or saves. RetroArch is
  launched on a per-run config in a temp dir (`-c`) that
  `retroarch::runconfig::RunConfig` writes from scratch, so any setting a
  run needs (the Vulkan driver, `video_shader_enable`) must be in that
  file; nothing else supplies it. A fresh config also triggers RetroArch's
  first-run asset-bundle extraction, whose completion reinitialises every
  driver a few seconds into the run and saves the config; the run config
  disables it (`bundle_assets_extract_enable`), and any new key that makes
  RetroArch reinit mid-run will show up as gpucapture reporting "process
  crashed or disconnected". Every launch passes `--sram-mode
  noload-nosave` so RetroArch never touches `.srm`/`.rtc` files. A
  `--state` file is copied into the temp dir rather than loaded in place.
- `--shader` and `--output` are `Option` fields with `required = true`
  in `src/main.rs`, not plain `PathBuf`s: clap's `subcommand_negates_reqs`
  waives them for `entitle`, and the derive cannot fill a non-`Option`
  field that is absent. Turning them back into `PathBuf` breaks the
  subcommand while every capture test still passes.
- `deny.toml` lists exactly the licenses the tree uses with
  `unused-allowed-license = "deny"`; adding or dropping a dependency may
  require editing that list.
- docs.rs builds with `no-default-features` (the librashader tree's C++
  cannot cross-build there), so crate-level docs must not intra-doc-link
  into the `hosted` module.
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

`retroarch/entitle.rs` is the `entitle` subcommand: `codesign -d` to read
the entitlements, an ad-hoc `codesign --force --sign -` with a one-key
plist when `get-task-allow` is missing, and a re-read to confirm.
`capture::parse_listing` keeps the `[non-debuggable]` marker from
`gpucapture list`, and `wait_capturable` fails at once, naming the app
and the command, when RetroArch is listed without it.

`retroarch/mod.rs` assembles a `launch::LaunchPlan` plus a
`runconfig::RunConfig`, renders the run config to a temp dir, builds a
`launch::LaunchCommand`, and hands it to `capture::run` with a
`capture::CaptureOptions`. `hosted/mod.rs` resolves the request into a
`CoreRun` through `layout`, and `boot_core` opens the core, checks the ROM
and finds and decodes the state before `init`, then boots it and hands
the `render::FrameSource` to `render::run`. `bundle` (recognising and
clearing a `.gputrace`, checking the output directory exists),
`image_file` (the `--image` check against a backend's extension list),
and `layout::DirResolver` (core, system directory, states, defaults first
and `retroarch.cfg` only on a miss) are shared.

Ctrl-C: the RetroArch backend's handler kills the launched process and
exits 130 itself. The hosted backend cannot do that, because Metal is
writing the bundle and the `Trace` guard lives on the render thread, so
`hosted::interrupt` only raises a flag, the frame loops poll it, and the
run unwinds through its error path (stop capture, remove the partial
bundle) returning `backend::Interrupted`, which `main` maps to exit 130.

`capture::run` spawns RetroArch under a drop guard that SIGKILLs it on any
failure path, polls `gpucapture list` until the pid is capturable, then
runs one of two triggers. **Settle** (no state) sleeps and runs
`gpucapture start`. **Paused** (`--state` or `--slot`) connects to
RetroArch's UDP command interface via `remote::Remote`, pauses, sends
`LOAD_STATE`, frame-advances, arms `gpucapture` on a thread, and keeps
advancing until the capture closes, because a paused RetroArch only
presents on a frame advance. The measured constants and their reasons are
in the doc comments on `retroarch/capture.rs` and in the spec.

`hosted/libretro/mod.rs` hosts a core in this process in four steps: `Core::open`
dlopens the `.dylib` through `libloading`, resolves every `retro_*` symbol,
and checks `retro_api_version`; `Core::system_info` names the core, which is
what picks its state directory, and libretro.h allows that call before
`retro_init`; `Core::init` publishes the `Context` to the
callbacks, installs them, and calls `retro_init`, so a core that reads its
options that early sees the real values; and `Core::load_game`, given that
`SystemInfo`, loads the ROM and returns `AvInfo`. Then `Core::restore` feeds a decoded state to
`retro_unserialize`, and `Core::run_frame` runs one `retro_run` and returns
the `Frame` the core delivered. libretro's callbacks carry no user pointer,
so everything they need lives in a process-wide static in `hosted/libretro/env.rs`,
and `Core::open` therefore refuses a second core in the same process;
`Drop` unloads the game, deinitializes what `init` initialized, and releases
that slot. `Context` carries what the `environment` callback answers
from (system and save directories, and core options from `--core-options`,
empty by default). A `Core` is a `render::FrameSource`, which is how
emulated frames reach the render loop.

`hosted/render/mod.rs` builds BGRA8 textures, loads the preset with
`librashader::runtime::mtl::FilterChain`, and runs a two-phase loop over a
`FrameSource`: `warmup` frames are rendered before `Trace::start`, so
history and frame-count passes see real prior frames, then `--frames`
command buffers are rendered between `Trace::start` and `Trace::finish`
(`hosted/render/trace.rs`, a guard over `MTLCaptureManager` whose drop stops an
unfinished capture). The frame count advances across both phases. Preset
compilation happens before the capture starts so only the recorded frame
command buffers land in the bundle. `output_size` maps the shared window
modes to pixels (RetroArch's are points): `Exact` is pixels as given,
`Scale` multiplies the source size, `Fullscreen` and the default fill use
`display::Screen`'s backing scale.

`retroarch::runconfig::RunConfig` is the single place RetroArch settings
are set; `config::WindowMode` is shared by both backends.
The source tree mirrors the split: shared modules at the top of `src/`,
everything RetroArch-only under `src/retroarch/`, everything in-process
under `src/hosted/` behind the feature. `display`
queries the main screen through AppKit and falls back to 1920x1080 at 1x
off the main thread, which is what every test sees.

Unit tests live inline and are deterministic: config rendering, argument
parsing and conflicts, output sizing, the `gpucapture list` parser, and the
UDP protocol against a fake on 127.0.0.1. A test that restates its
implementation (a wrapper compared with what it wraps) is not wanted; assert
concrete values. `hosted::render` tests its sizing
and image conversion; its render loop touches the GPU and has no unit
tests by design.

## Releasing

`RELEASING.md` covers it: bump `Cargo.toml`, retitle the `## Unreleased`
CHANGELOG section, push, then dispatch the `release` workflow. Every
release is a `0.x` pre-release.
