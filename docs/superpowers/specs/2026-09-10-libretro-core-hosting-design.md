# libretro core hosting design

Extend the librashader backend from static images to live emulator cores.
`--backend librashader --core CORE --rom ROM [--state FILE | --slot N]`
loads the libretro core into this process, runs it headless, feeds each
emulated frame through the shader preset, and records the trace with
`MTLCaptureManager`, exactly as image mode already does. RetroArch is not
launched. The RetroArch backend keeps doing what it does today and stays
the default.

Decided 2026-09-10 on the strength of a throwaway spike (below): host the
core in-process behind a small libretro frontend, use the `libretro-sys`
bindings crate for the API surface, and confine the C ABI to one module.

## Spike (2026-09-10, throwaway, not in the repo)

A 300-line frontend built on `libloading` ran `sameboy_libretro.dylib`
headless against the Link's Awakening DX ROM and the user's real RetroArch
`.state` for it:

- The core initialised, loaded the ROM, reported 160x144 XRGB8888 at
  59.728 fps, and produced frames with every unhandled environment query
  answered `false`. It queried 15 distinct commands; six needed a real
  answer (can-dupe, system dir, save dir, pixel format, set-variables,
  get-variable).
- The state file on disk is a 28 KB `#RZIPv1#` container holding a 252 KB
  `RASTATE1` container whose `MEM ` block is exactly the core's
  `retro_serialize_size`. `retro_unserialize` accepted it and the next
  frame matched RetroArch's own thumbnail of that state.
- Answering `GET_VARIABLE` from RetroArch's `SameBoy.opt` gave the same
  colour correction RetroArch shows; without it the core used its defaults.
- 60 frames ran in 123 ms; load took 5 ms.
- The core's log-interface request needs a C-variadic callback, which
  stable Rust cannot define. Declining it costs nothing: the core prints
  to stderr instead.

## Goals

- `ra-metal-capture --backend librashader --core sameboy --rom z.gbc
  --state z.state --shader crt.slangp --output out.gputrace` records the
  same emulated frame the RetroArch backend would record for the same
  `--state` and `--advance`, rendered by librashader, with no RetroArch
  process.
- `--frames N` records N successive emulated frames, each rendered
  through the preset with frame count and history advancing, which the
  RetroArch backend cannot do at all (a paused RetroArch presents one
  frame per advance and `gpucapture` needs three presents per boundary).
- Frames before the recorded one pass through the preset uncaptured, so
  history-dependent passes see real prior frames in the recorded one.
- The user's RetroArch configuration keeps working: core resolution from
  `libretro_directory`, states from `savestate_directory`, per-core
  options from the config directory, BIOS files from `system_directory`.
  Nothing under those paths is written.

## Non-goals

- Hardware-rendered cores. A core that asks for an OpenGL or Vulkan
  context (`SET_HW_RENDER`) is refused; if it cannot fall back to software
  rendering, `retro_load_game` fails and the tool says which core and why.
  Software-rendered cores cover the 8-bit and 16-bit systems this tool
  exists for.
- Input. No controller is connected; every input query returns 0, which
  is also what makes the run deterministic.
- Audio. Samples are dropped.
- Zipped ROMs. RetroArch extracts archives itself; this backend takes the
  extracted file. A `.zip` is refused with a message saying so.
- Subsystems, multi-disc content, cheats, achievements, netplay,
  rewind, run-ahead, and the core log interface (see the spike).
- Save states in formats other than RetroArch's (raw core data, RASTATE,
  RZIP-wrapped RASTATE). Those three are all RetroArch has ever written.
- Producing a save state, `.srm` writes, or anything else that persists.

## CLI

`--backend librashader` no longer requires `--image`; it requires
`--shader` and exactly one of `--image` or `--core` plus `--rom`. The
existing `--core`, `--rom`, `--state`, `--slot`, `--advance`, `--settle`,
and `--frames` flags apply with these meanings under librashader:

| flag | librashader meaning |
|---|---|
| `--core CORE` | `.dylib` path or bare name resolved in `libretro_directory`, as today |
| `--rom PATH` | content file, loaded by data (and by path for cores that need it) |
| `--state FILE` | RetroArch state file to restore after loading the ROM |
| `--slot N` | resolve slot N under `savestate_directory` the way RetroArch names it (see States) |
| `--advance N` | emulated frames to run after the state before the recorded frame, default 1, min 1; the Nth frame is the recorded one, matching the RetroArch backend |
| `--settle SECS` | without a state, emulated frames to run before recording: `round(SECS * fps)`, default 5 s, so a fresh boot gets past the logo like the RetroArch settle does |
| `--frames N` | emulated frames to record, each rendered with the frame count advancing |
| `--size`, `--scale`, `--fullscreen` | as for image mode, in pixels, against the core's base geometry |

Unchanged rules: `--state` and `--slot` conflict; `--image` conflicts with
`--core`, `--rom`, `--state`, `--slot`, `--advance`. `--app`, `--config`
(still read, see below), `--cmd-port`, and `--keep-running` are
RetroArch-backend flags; `--app`, `--cmd-port`, and `--keep-running` are
ignored under librashader as they are today.

`--config` matters to both backends: the librashader backend reads
`libretro_directory`, `system_directory`, `savestate_directory`,
`sort_savestates_enable`, `sort_savestates_by_content_enable`,
`savestates_in_content_dir`, and `rgui_config_directory` from it.

Exit status is non-zero, with a one-line reason on stderr, when: the core
cannot be loaded (`dlopen` failure or a missing `retro_*` symbol); the ROM
is a `.zip`; `retro_load_game` returns false; the core asks for hardware
rendering; the state file cannot be decoded; `retro_unserialize` returns
false (the message carries the payload size and the core's serialize
size); the core sets a pixel format this tool does not convert; or the
core never delivers a frame.

## Components

### `libretro` (new, `#[cfg(feature = "librashader")]`)

`src/libretro/mod.rs`, `src/libretro/env.rs`, `src/libretro/pixels.rs`.
The module carries `#![allow(unsafe_code)]` and is, with `render`, one of
the two places the crate allows it. All type and constant names come from
`libretro-sys` 0.1.1 (`CoreAPI`, `GameInfo`, `SystemAvInfo`, `Variable`,
`ENVIRONMENT_*`, `PixelFormat`); the crate declares the six callbacks it
owns as `unsafe extern "C" fn` items, which is the only hand-written C ABI
in the repository.

**`Core`** (`mod.rs`):

```rust
pub struct Core { lib: libloading::Library, api: libretro_sys::CoreAPI, loaded: bool }

impl Core {
    /// dlopen the core, resolve every `retro_*` symbol, install the
    /// callbacks, and call `retro_init`. Fails if another Core is alive
    /// in this process.
    pub fn load(dylib: &Path, ctx: Context) -> Result<Core>;
    /// `retro_get_system_info`: library name, valid extensions, need_fullpath.
    pub fn system_info(&self) -> SystemInfo;
    /// `retro_load_game` with the file's bytes (and path), then
    /// `retro_get_system_av_info`. Refuses `.zip`.
    pub fn load_game(&mut self, rom: &Path) -> Result<AvInfo>;
    /// `retro_unserialize`; the message on failure names both sizes.
    pub fn restore(&mut self, state: &[u8]) -> Result<()>;
    /// `retro_run` once and return the frame it delivered (or the
    /// previous one on a dupe). Fails if no frame has ever arrived.
    pub fn run_frame(&mut self) -> Result<&Frame>;
}
impl Drop for Core { /* retro_unload_game if loaded, retro_deinit, release the process slot */ }

pub struct AvInfo { pub base: Size, pub max: Size, pub aspect_ratio: f32, pub fps: f64 }
pub struct Frame { pub size: Size, pub bgra: Vec<u8> }
pub struct Context {
    pub system_dir: PathBuf,
    pub save_dir: PathBuf,            // a temp dir; nothing there is kept
    pub options: HashMap<String, String>,
}
```

libretro callbacks carry no user pointer, so the frontend state they
touch is a process-wide `static` behind a `Mutex` (`env.rs`): the pixel
format, the options map as `CString`s, the directory strings, and the
latest frame. `Core::load` takes a process-wide slot and `Drop` releases
it, so a second `Core` in one process is an error rather than two cores
sharing one set of globals. The tool only ever needs one.

**Environment callback** (`env.rs`), answering exactly what the spike
showed a software core needs:

| command | answer |
|---|---|
| `GET_CAN_DUPE` | `true` |
| `GET_SYSTEM_DIRECTORY`, `GET_SAVE_DIRECTORY` | the Context paths |
| `SET_PIXEL_FORMAT` | record it; `true` for the three formats `pixels` converts |
| `SET_VARIABLES`, `SET_CORE_OPTIONS*` | `true` (accepted, values come from the options map) |
| `GET_VARIABLE` | value from the options map when known, else a null value; `true` either way, and `true` on a null pointer too, which is what RetroArch does (runloop.c) |
| `GET_VARIABLE_UPDATE` | `false` |
| `SET_HW_RENDER` | `false`, and a flag the loader turns into a clear error if `retro_load_game` then fails |
| everything else | `false` |

**Pixel conversion** (`pixels.rs`, pure, tested):
`pub fn to_bgra(format: PixelFormat, data: &[u8], width: u32, height: u32, pitch: usize) -> Vec<u8>`
for XRGB8888 (a straight copy per row, the X byte forced to 0xFF),
RGB565, and 0RGB1555, expanding 5- and 6-bit channels by bit replication.
The video-refresh callback copies the core's buffer out during the call,
because the pointer is only valid until `retro_run` returns.

### `state` - decoding RetroArch state files

`src/state.rs` gains two pure functions:

- `pub fn decode(bytes: &[u8]) -> Result<Vec<u8>>`: if the file starts
  with `#RZIPv` `<version>` `#`, read the u32 chunk size and u64 total
  size at offsets 8 and 12, then inflate consecutive `(u32 length, zlib
  stream)` chunks until the total is reached (`flate2`). Then, if the
  result starts with `RASTATE` `<version>`, walk 8-byte block headers
  (4-byte tag, u32 little-endian size, payload padded to 8) and return the
  `MEM ` payload at its unpadded size; `END ` before `MEM ` is an error.
  Anything that starts with neither prefix is raw core data and is
  returned as is. Errors name the offending offset.
- `pub fn slot_path(cfg: &StateDirs, core_name: &str, rom: &Path, slot: u32) -> PathBuf`:
  RetroArch's naming. Directory: `rom`'s own directory when
  `savestates_in_content_dir`, else `savestate_directory` joined with the
  core's library name when `sort_savestates_enable`, or with the ROM's
  parent directory name when `sort_savestates_by_content_enable`. File:
  `<rom stem>.state` for slot 0, `<rom stem>.state<N>` otherwise.
  `StateDirs { savestate_directory, sort_by_core: bool, sort_by_content: bool, in_content_dir: bool }`.

The existing `stage` stays for the RetroArch backend.

### `config` - core options

`src/config.rs` gains `pub fn read_all(text: &str) -> HashMap<String, String>`
(the same `key = "value"` parser as `read_keys`, every key) and the
librashader path reads `<rgui_config_directory>/<library_name>/<library_name>.opt`
through it when the file exists. A missing file means the core's defaults,
which is also what RetroArch does on first run. `core_options_path` in
`retroarch.cfg` is not consulted; a global options file is rare and
documented as unsupported.

### `render` - frame sources

`render::run` stops decoding an image itself and takes a frame source:

```rust
pub trait FrameSource {
    /// Size of every frame this source yields.
    fn size(&self) -> Size;
    /// The next frame as tightly packed BGRA8 rows, top row first.
    fn next(&mut self) -> Result<Vec<u8>>;
}
pub struct ImageSource { .. }   // decodes once, yields the same bytes forever
pub struct RenderOptions {
    pub source: Box<dyn FrameSource>,
    pub preset: PathBuf,
    pub window: WindowMode,
    pub screen: Screen,
    pub warmup: u32,     // frames rendered through the chain before the capture starts
    pub frames: u32,
    pub output: PathBuf,
    pub verbose: bool,
}
```

`ImageSource::open(path)` lives in `render`; the core source lives in
`libretro` (`impl FrameSource for Core` runs one frame per `next`), so
`render` knows nothing about libretro and `libretro` knows nothing about
Metal. The run loop becomes: `warmup` iterations of upload-and-render with
no capture, then `Trace::start`, then `frames` iterations captured, then
`Trace::finish`. The frame count passed to librashader keeps counting
across both phases so history is continuous. Each iteration re-uploads
the input texture with `replaceRegion`; at core resolutions that is
microseconds.

For image mode `warmup` is 0, preserving today's behaviour exactly.

### `main`

Under `--backend librashader` with `--core`:

1. Read the config keys listed under CLI. Resolve the core with
   `core::resolve_core` as today. Refuse a `.zip` ROM up front.
2. `Core::load`, `system_info` for the library name (needed for the
   options file and slot path), read the options file, `load_game`.
3. Resolve the state: `--state` is read as given; `--slot` goes through
   `state::slot_path`. `state::decode`, then `Core::restore`.
4. Warm-up count: `--advance - 1` with a state (so the recorded frame is
   the `--advance`th, as under RetroArch); `round(--settle * fps)` without
   one.
5. Hand the core to `render::run` as the frame source, with `warmup`,
   `frames`, and the window mode against the core's base geometry.

The `MTL_CAPTURE_ENABLED` re-exec happens before any of this, as today.
Image mode goes through the same `render::run` with an `ImageSource` and
`warmup = 0`.

## Data flow

```
args + retroarch.cfg
  -> resolve core dylib, system dir, options file, state path
  -> Core::load (dlopen, callbacks, retro_init)
  -> Core::load_game (retro_load_game, av info -> base size)
  -> state::decode -> Core::restore            (with --state/--slot)
  -> render::run(source = Core):
       output_size(window, base, screen)
       warmup x { core.run_frame -> upload -> chain.frame(n) }
       Trace::start
       frames x { core.run_frame -> upload -> chain.frame(n) }   (captured)
       Trace::finish
  -> <out>.gputrace
```

## Error handling

`anyhow` throughout, naming the core, ROM, or state path. `Core`'s `Drop`
always runs `retro_deinit`, so a failed load leaves no half-initialised
core, and the process-wide slot is released. The C callbacks never
unwind: each is a small body that locks a mutex, copies or answers, and
returns; there is no `?` and no panic path inside them (a poisoned mutex
is recovered with `into_inner`). Frames are copied out inside the
video-refresh callback, so no core pointer escapes the call.

## Testing

Unit tests, no core, GPU, or RetroArch:

- `pixels::to_bgra`: each of the three formats on a 2x2 image with a
  pitch wider than the row, checking exact bytes and the X byte.
- `state::decode`: a raw payload passes through; a RASTATE container with
  a leading `RPLY` block and a padded `MEM ` block yields the `MEM `
  payload at its exact size; an RZIP container built in the test with
  `flate2` around that RASTATE round-trips; a truncated block header,
  a size past the end, and `END ` before `MEM ` are errors.
- `state::slot_path`: the four directory rules and the slot-0 versus
  slot-N naming.
- `config::read_all`: quoted values, comments, blank lines.
- `render::ImageSource`: `sample.png` yields 160x144 and the same bytes
  twice.
- `main`: `--backend librashader --core c --rom r` parses; with `--image`
  too it is rejected; `--slot` and `--state` still conflict; a `.zip` ROM
  is refused before any core is loaded (checked in `run` with a fake
  path).

No test loads a core: cores are user-installed binaries. Manual
end-to-end check, recorded in the spec rather than the README:

```
ra-metal-capture --backend librashader --core sameboy \
  --rom "<Link's Awakening DX rom>" \
  --state "~/Documents/RetroArch/states/SameBoy/<same>.state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --frames 2 --output /tmp/ladx-ls.gputrace -v
```

Success is a bundle whose recorded frame shows the same scene as the
state's `.state.png` thumbnail and whose second command buffer differs
from the first.

**Verified 2026-09-11.** All four runs passed on an M-series Mac with the
SameBoy core and Link's Awakening DX. The command above loaded the core
(`SameBoy (160x144 at 59.728 fps)`), reported `source 160x144 -> output
3071x2764 px, 0 warm-up + 2 recorded frame(s)`, exited 0, and wrote a 35 MB
bundle with an `index` entry in 1.2 s warm (4.0 s on the first, cold run).
`gpudebug` reports exactly two command buffers of 14 encoders each.
`--slot 0` resolved to the same state under `sort_savestates_enable` and
produced byte-identical input and output textures. `--advance 30` reported
29 warm-up frames and a different input texture, and its bundle grew to
107 MB because the warm-up frames fill the preset's history and feedback
targets (792 resource objects against 163). Without a state the default
`--settle 5` ran 299 warm-up frames and recorded the wave intro scene, past
the boot logo. The RetroArch backend still captures the same core, ROM, and
state through `gpucapture` and leaves no process behind. Frame content was
checked against the spike's rendered frames and the state's `.state.png`
thumbnail, not in Xcode's GPU debugger.

## Repository policy changes

- **Dependencies** (all behind the `librashader` feature): `libretro-sys`
  0.1 (MIT, pregenerated bindings, no build script), `libloading` 0.9
  (ISC), `flate2` 1 (MIT OR Apache-2.0, pure-Rust `miniz_oxide` backend
  already in the tree). `deny.toml`'s allow list gains `ISC`.
- **CLAUDE.md** and the spec's unsafe rule: `unsafe` is allowed in
  `src/render/` and `src/libretro/`; hand-written C ABI (the six
  `extern "C"` callbacks) only in `src/libretro/`. Metal stays
  objc2-only.
- **README**: the librashader backend section grows a "Cores" paragraph:
  what works (software-rendered cores, RetroArch states, per-core
  options), what does not (hardware rendering, input, audio, zips), and
  that `--frames N` records N real emulated frames.
- **CHANGELOG** `## Unreleased`: the feature, the `render::RenderOptions`
  break (`image` replaced by `source`, new `warmup`), the new `libretro`
  module, `state::decode` and `slot_path`, `config::read_all`. Release
  as 0.4.0.

## Toolchain and dependencies

| crate | version | use |
|---|---|---|
| libretro-sys | 0.1 | libretro types, constants, and the `CoreAPI` function table |
| libloading | 0.9 | dlopen the core and resolve `retro_*` symbols |
| flate2 | 1 | inflate RZIP chunks |

## Known risks

- **Other cores want other environment answers.** SameBoy ran on six
  answered commands. Another core may require one more (a controller
  port, a memory map, a performance level) before `retro_load_game`
  succeeds. Each is a small addition to `env.rs`; the failure mode is a
  clear `retro_load_game failed` for that core, not a crash.
- **Cores that need the log interface.** Declined because stable Rust
  cannot define a C-variadic function. A core that dereferences the
  callback without checking the return would crash; none of the common
  ones do, and a one-file C shim through `cc` is the fix if one does.
- **`need_fullpath` cores.** The ROM is passed both as bytes and as a
  path, which satisfies both kinds of core. Cores that open sibling
  files (CD images, multi-file games) are outside the tested set.
- **Global callback state.** One `Core` per process is enforced; the
  statics are a consequence of the libretro ABI, not a shortcut.
- **Parity with the RetroArch backend is by construction, not by
  measurement.** The same state and `--advance` should record the same
  emulated frame; RetroArch's core options file makes the core render
  it the same way. Confirmed once for SameBoy in the spike; other cores
  are confirmed by their first real run.
