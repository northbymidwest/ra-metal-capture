# ra-metal-capture design

A Rust CLI that launches a chosen RetroArch.app with a core, a ROM, a save
state, and a shader preset, sizes the window, and uses Apple's `gpucapture(1)`
to record one (or N) presented frames to a `.gputrace` bundle. No Xcode GUI,
no code injection, and the user's real RetroArch config and save states are
never modified.

## Goals

- One command produces a `.gputrace` of a known game state under a known
  shader preset, repeatably, from any of several installed RetroArch builds.
- Windowed by default, sized as large as fits the main display; exact size,
  integer scale, or fullscreen on request.
- Never write to the user's `retroarch.cfg` or states directory.
- Never leak a RetroArch process on any failure path.

## Non-goals

- Forcing RetroArch's Metal driver. The capture runs under whatever
  `video_driver` the user's config selects (currently Vulkan via MoltenVK).
- Post-processing the trace. Other tools in the workplace do that.
- Linux or Windows support. `gpucapture` and `.app` bundles are macOS-only.

## CLI

```
ra-metal-capture [OPTIONS] --output <OUT.gputrace> (--core <CORE> --rom <ROM> | --image <IMAGE>)

  --app <PATH>        RetroArch .app bundle, or the binary inside it
                      [default: /Applications/RetroArch.app]
  --core <CORE>       Path to a libretro .dylib, or a bare name (e.g. "sameboy")
                      resolved as <libretro_directory>/<name>_libretro.dylib
  --rom <PATH>        Content file to load
  --image <PATH>      Static image to load through RetroArch's built-in image
                      viewer core, in place of --core and --rom. Conflicts
                      with --core, --rom, --state, --slot, --advance.
  --state <PATH>      Save state file to load at launch (copied to a temp
                      savestate dir as slot 0). Conflicts with --slot.
  --slot <N>          Load slot N from the user's configured savestate dir.
                      Conflicts with --state.
  --shader <PATH>     Shader preset (.slangp/.glslp) passed via --set-shader
  --config <PATH>     retroarch.cfg to base the run on
                      [default: ~/Library/Application Support/RetroArch/config/retroarch.cfg]
  --size <WxH>        Exact window size in points. Conflicts with --scale, --fullscreen.
  --scale <N>         Integer scale of core resolution. Conflicts with --size, --fullscreen.
  --fullscreen        Launch with -f. Conflicts with --size, --scale.
  --settle <SECS>     Seconds to wait before capturing when no state is given
                      [default: 5]
  --advance <N>       Frame advances after loading the state, before the
                      capture is armed [default: 1, min 1]
  --cmd-port <PORT>   UDP port for RetroArch's command interface, enabled
                      only for this run [default: 55355]
  --frames <N>        Number of frame boundaries to capture [default: 1]
  --keep-running      Do not terminate RetroArch after the capture
  -v, --verbose       Print the assembled command line and appendconfig
```

Exit status is non-zero, with a one-line reason on stderr, when: the app or
binary is missing, the core cannot be resolved, the ROM or state file does
not exist, RetroArch exits before becoming capturable, the PID never appears
in `gpucapture list`, or `gpucapture start` fails or writes no output.

## Components

Each is a module with a pure interface where possible, so it can be unit
tested without launching anything.

### `app` - locating the binary

`resolve_binary(path) -> Result<PathBuf>`. Accepts a `.app` directory (returns
`<app>/Contents/MacOS/RetroArch`) or an executable file (returned as-is).
Errors if neither exists.

### `core` - resolving the core

`resolve_core(arg, libretro_dir) -> Result<PathBuf>`. If `arg` is an existing
file, use it. Otherwise try `<libretro_dir>/<arg>` and
`<libretro_dir>/<arg>_libretro.dylib`. `libretro_dir` comes from the
`libretro_directory` key in the base config, with `~` expanded, falling back
to `~/Library/Application Support/RetroArch/cores`.

### `config` - reading the base config and writing the appendconfig

`read_keys(cfg_path, keys) -> HashMap<String, String>`: minimal parser for
RetroArch's `key = "value"` lines. Only the key the tool needs is read: `libretro_directory`. The
`--slot` path relies on RetroArch reading its own `savestate_directory`.

`AppendConfig` builder producing the text of the temp appendconfig. Always
emits:

```
config_save_on_exit = "false"
savestate_auto_save = "false"
savestate_auto_load = "false"
pause_nonactive = "false"
menu_show_load_content_animation = "false"
video_font_enable = "false"
```

`pause_nonactive` matters because RetroArch stops rendering when its window
is not focused, which would starve the capture of frame boundaries. The last
two keep the "Load Content" start animation and every on-screen text
notification (state loaded, controller autoconfig, and so on) out of the
captured frame; `video_font_enable` disables all OSD text for the run.

A width and height are always carried together as `Size { width, height }`
rather than a bare tuple, so call sites can't swap them by accident.
`WindowMode` names its two sized variants accordingly: `Fill { max: Size }`
for the clamped scale-to-fit mode, and `Exact(Size)` for `--size WxH`.

Window mode adds:

- Fill (default): `video_fullscreen = "false"`,
  `video_window_save_positions = "false"`, `video_scale = "20"`,
  `video_window_auto_width_max = "<W>"`,
  `video_window_auto_height_max = "<H>"` where W and H are the main display's
  visible frame (NSScreen, excludes menu bar and dock) minus a 28-point title
  bar allowance. RetroArch shrinks the scaled window to fit these maxima while
  keeping aspect ratio (`gfx/video_driver.c`, windowed size computation).
  `video_window_save_positions = "false"` is needed because on macOS a saved
  position in the user's own config would otherwise take precedence over this
  run's maxima (`gfx/video_driver.c`, custom-size branch).
- `--size WxH`: `video_fullscreen = "false"`,
  `video_window_save_positions = "true"`,
  `video_windowed_position_width = "<W>"`,
  `video_windowed_position_height = "<H>"`. On macOS the custom-size branch
  is gated on `video_window_save_positions`, not
  `video_window_custom_size_enable`. `config_save_on_exit=false` prevents the
  position being written back.
- `--scale N`: `video_fullscreen = "false"`,
  `video_window_save_positions = "false"`, `video_scale = "<N>"`,
  `video_window_auto_width_max = "0"`, `video_window_auto_height_max = "0"`,
  `video_fullscreen_x = "0"`, `video_fullscreen_y = "0"` so nothing clamps it.
  `video_window_save_positions = "false"` is needed for the same reason as
  Fill mode: on macOS a saved position in the user's own config would
  otherwise take precedence (`gfx/video_driver.c`, custom-size branch).
- `--fullscreen`: nothing in the config; `-f` on the command line.

State mode adds, for `--state <file>` only:

```
savestate_directory = "<tmpdir>/states"
sort_savestates_enable = "false"
sort_savestates_by_content_enable = "false"
savestates_in_content_dir = "false"
```

Paused mode adds, whenever `--state` or `--slot` was given:

```
network_cmd_enable = "true"
network_cmd_port = "<cmd-port>"
state_slot = "<slot>"
```

`network_cmd_enable` is scoped to this run only, through the temp
appendconfig; the user's own config is never touched. `state_slot` is the
slot `LOAD_STATE` reads: 0 when `--state` staged a file, or the value of
`--slot` when a state already sits in the user's configured savestate
directory.

Image mode adds, for `--image <file>` only:

```
builtin_imageviewer_enable = "true"
```

Rendered as the last line of the appendconfig, after any window-mode keys.
Image mode conflicts with state and paused mode, so this key never appears
alongside the state-mode or paused-mode keys above.

### `display` - main display size

`visible_size() -> Size` via `objc2-app-kit` `NSScreen::mainScreen`
`visibleFrame`. Falls back to 1920x1080 with a warning if no screen is found.

### `state` - staging the save state

`stage(state_path, rom_path, tmpdir) -> Result<u32>` copies the state file to
`<tmpdir>/states/<rom file stem>.state` and returns slot 0. Slot 0 is the
bare `.state` suffix; slot N is `.stateN`. The `.state.png` thumbnail is not
needed and not copied.

`--slot N` stages nothing; `state_slot = "N"` in the appendconfig points
`LOAD_STATE` at the user's own savestate directory instead of a staged copy.

### `launch` - command line assembly

`build_command(plan) -> (PathBuf, Vec<OsString>, Vec<(String,String)>)`: a
pure function from a `LaunchPlan` struct to binary, args, and env. Args, in
order:

```
[-L <core>] [-f] [--set-shader <preset>] --appendconfig <tmp.cfg> [-v] <content>
```

`-L <core>` is omitted in image mode, where `LaunchPlan.core` is `None` and
`content` is the image file rather than a ROM.

`-e` is never passed. Loading a state is now always driven over the UDP
command interface (`LOAD_STATE`, see `capture` below) once RetroArch is
confirmed running, so a state loads at a known point in RetroArch's
startup rather than racing content init.

Env: `MTL_CAPTURE_ENABLED=1`. `MTLCAPTURE_WAIT_FOR_SIGNAL` is deliberately not
set: RetroArch must run freely to load the state and settle.

The binary is executed directly, not via `open`, so the environment reaches
it. The current directory is left alone.

### `capture` - driving gpucapture

1. Before spawning, if `<out>` exists it must be a previous `.gputrace`
   bundle (name suffix plus an `index` entry) and is removed; any other
   existing path is refused so a mistyped `--output` never deletes user data.
2. Spawn RetroArch. Wrap the child in a guard whose `Drop` sends SIGKILL if
   the child is still running, so every early return cleans up.
3. Poll `gpucapture list` every 100 ms, up to 30 s, until the first column of
   some line equals the PID. If the child exits first, fail with its exit
   status and the last lines of its stderr.

From here the flow depends on whether a state was given (`Trigger::Settle`
vs. `Trigger::Paused`):

**Settle** (no `--state` and no `--slot`):

4. Sleep `--settle` seconds.
5. Run `gpucapture start --pid <PID> --count <frames> --output <out>` and wait
   for it. Fail if it exits non-zero or `<out>` does not exist afterwards.
6. Unless `--keep-running`, send SIGTERM, wait up to 3 s, then SIGKILL.
7. Remove the temp dir.

**Paused** (`--state` or `--slot` given):

4. Connect over UDP to `--cmd-port` and wait for `GET_STATUS` to report
   `PLAYING`, polling every 200 ms up to `ready_timeout`. Fail if RetroArch
   exits first.
5. `PAUSE_TOGGLE`, then confirm `GET_STATUS` reports `PAUSED`.
6. `LOAD_STATE`. RetroArch loads the configured slot; wait
   `LOAD_STATE_SETTLE` (1 s) for the short window of real, asynchronous
   presents that follow the load to pass before anything is armed against
   it.
7. `FRAMEADVANCE` `--advance` times (min 1, default 1).
8. Arm `gpucapture start --pid <PID> --count <frames> --output <out>` on a
   background thread with its stdout piped, and wait (up to 10 s) for the
   thread to report the line it flushes once it is waiting for a boundary
   (`triggering capture ...`). Fail, killing RetroArch first, if that line
   never arrives.
9. Send `FRAMEADVANCE` repeatedly, checking after each whether the capture
   thread has finished, up to `MAX_CLOSING_ADVANCES` (6) advances. A paused
   RetroArch presents only on a frame advance, and `gpucapture` needs more
   than one present to open and close a frame (measured: 3, though this may
   depend on swapchain depth, hence advancing until the capture reports
   done rather than hard-coding the count). If an advance command itself
   fails, or the cap is reached, kill RetroArch (which releases
   `gpucapture`) and join the thread rather than leaking it.
10. Join the capture thread; fail if it panicked or `gpucapture start`
    failed.
11. Unless `--keep-running`, send `QUIT` twice (RetroArch's press-twice
    default) and wait up to 3 s for the process to exit; fall back to
    SIGTERM then SIGKILL if it hasn't.
12. Remove the temp dir.

**Why advance until the capture closes.** A spike measured RetroArch's UDP
command interface directly before this design was approved: `GET_STATUS`
replies `GET_STATUS PLAYING <core>,<content>,crc32=<hex>`,
`GET_STATUS PAUSED ...`, or `GET_STATUS CONTENTLESS`; `PAUSE_TOGGLE`,
`LOAD_STATE`, `FRAMEADVANCE`, and `QUIT` produce no reply. `QUIT` must be
sent twice, matching RetroArch's press-twice-to-quit default. A follow-up
measurement (2026-09-10, stock RetroArch.app, Vulkan) found the original
fixed-delay design was unsound: `LOAD_STATE` is asynchronous, and real
presents continue for a short window after the command, so a capture armed
inside that window completes on the load's own frames rather than on a
frame advance. After any `FRAMEADVANCE`, RetroArch presents nothing until
the next advance, and in that mode a capture needs three advances after
arming: one never opens the capture, two open it without a drawable (the
second frame's swapchain image was acquired before the window), and three
complete it. `gpucapture start` prints `triggering capture of 1 Frame from
<pid> @ 0` 91 ms after launch, flushed even when stdout is a pipe, before
it blocks, which is what the readiness callback watches for. Because the
closing-advance count may depend on swapchain depth, the tool advances
until the capture thread finishes rather than hard-coding 3, with
`MAX_CLOSING_ADVANCES` as a backstop against a capture that never closes.

If `gpucapture start` reports no capturable boundary (a MoltenVK risk noted
below), the error is surfaced verbatim; the fallback of `--until-exit` is left
to the user for now.

### `main`

Parses args with `clap`, reads the base config, resolves paths, builds the
appendconfig in a `tempfile::TempDir`, builds the command, runs the capture,
prints the output path on success.

## Image mode

Measured 2026-09-10: `--image sample.png` (160x144) with
`vectorscale.slangp`, under the default 5s settle, produced a 144M bundle.
The appendconfig ended with `builtin_imageviewer_enable = "true"`, the
command line carried no `-L`, and RetroArch exited cleanly with no
leftover process. The image viewer core reports the image's own pixel
size as its content geometry, so the window-sizing and shader machinery
built for an emulator core applies unchanged, with no separate code path.

## Data flow

```
args + retroarch.cfg
  -> resolve binary, core, savestate dir
  -> TempDir { appendconfig.cfg, states/<rom>.state? }
  -> LaunchPlan -> (binary, args, env)
  -> spawn RetroArch (guarded)
  -> gpucapture list poll -> settle -> gpucapture start
  -> terminate RetroArch, drop TempDir
  -> <out>.gputrace
```

## Error handling

`anyhow` throughout, with context on every external step naming the path or
PID involved. Every failure after spawn goes through the child guard. The
temp dir is removed by `TempDir`'s drop on every path.

## Testing

Unit tests, no RetroArch or gpucapture required:

- `app::resolve_binary`: `.app` dir, bare binary, missing path.
- `core::resolve_core`: absolute path, bare name, `_libretro.dylib` suffix,
  missing.
- `config::read_keys`: quoted values, `~` expansion, missing keys, comments.
- `config::AppendConfig`: exact text for each window mode and for
  state-staging on and off.
- `state::stage`: file lands at `<tmp>/states/<stem>.state`, slot 0.
- `launch::build_command`: argument order and env for each option
  combination, including the conflicts being rejected at the clap level.
- `capture`: the `gpucapture list` line parser.

Manual end-to-end check, recorded in the README:

```
ra-metal-capture --core sameboy \
  --rom "<Link's Awakening rom>" \
  --state "~/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state" \
  --shader "~/Library/Application Support/RetroArch/shaders/<preset>" \
  --output /tmp/ladx.gputrace
```

Success is a `.gputrace` bundle that `gputrace-bundle` can list textures
from.

## Known risks

- **MoltenVK boundaries.** Under `video_driver = "vulkan"` the `MTLDevice`
  and `CAMetalLayer` belong to MoltenVK. `gpucapture` should still recognise
  its presents as frame boundaries, but this is unverified until the first
  real run. If not, `gpucapture boundaries --pid` will show what is
  available and the tool gains a `--boundary` option.
- **Hardened runtime.** `RetroArch-nightly.app` is signed with the runtime
  flag but also `disable-library-validation`, so `GPUToolsCapture` should
  load. The other two builds are ad-hoc signed without the runtime flag.
- **Open issue: the post-load wait is a fixed 1 s, not a signal.** RetroArch's
  UDP interface does not reply to `LOAD_STATE`; `LOAD_STATE_SLOT` replies,
  but only after queueing the load task, so it means "scheduled", not
  "loaded". The tool therefore sleeps `LOAD_STATE_SETTLE` (1 s, against an
  observed latency of tens of ms) and confirms nothing: a missing or
  corrupt state file fails silently, and the verbose flag is the only way
  to see the load result in RetroArch's log. Two real signals exist if this
  ever needs closing: poll `READ_CORE_RAM` over UDP until the paused core's
  RAM changes (the load is the only thing that can change it while paused),
  or, with `-v`, watch the temp log for `[State] Loading state`. Left open
  by decision on 2026-09-10.
- **`--frames` above a small number is unverified with the paused flow.**
  The closing-advance cap scales with `--frames`. `--frames 1` and
  `--frames 2` have both been exercised against a real RetroArch and
  succeeded (4 closing advances, 89M and 92M respectively); larger values
  are expected to work the same way (gpucapture waits for that many
  boundaries, a paused RetroArch presents once per advance) but have not
  been measured.
- **The settle flow does not confirm content loaded.** In the settle flow
  (including image mode), nothing checks that the ROM or image actually
  loaded; a corrupt or misnamed file leaves RetroArch on its menu, and the
  menu is what gets captured.

## Toolchain and dependencies

Rust 1.98 as the MSRV (`rust-version = "1.98"`, no toolchain pin file),
edition 2024. Dependencies at their current stable releases as of
2026-09-09, and kept current thereafter:

| crate | version | use |
|---|---|---|
| clap | 4.6 (derive) | argument parsing and conflict rules |
| anyhow | 1.0 | error context |
| tempfile | 3.27 | per-run temp dir |
| objc2-app-kit, objc2-foundation | 0.3 | main display visible frame |
| nix | 0.31 (signal feature) | SIGTERM / SIGKILL to the child |

No `unsafe` outside the display query.
