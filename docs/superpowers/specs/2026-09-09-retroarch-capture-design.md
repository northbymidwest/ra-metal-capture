# retroarch-capture design

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
retroarch-capture [OPTIONS] --core <CORE> --rom <ROM> --output <OUT.gputrace>

  --app <PATH>        RetroArch .app bundle, or the binary inside it
                      [default: /Applications/RetroArch.app]
  --core <CORE>       Path to a libretro .dylib, or a bare name (e.g. "sameboy")
                      resolved as <libretro_directory>/<name>_libretro.dylib
  --rom <PATH>        Content file to load
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
  --settle <SECS>     Seconds to wait after RetroArch becomes capturable
                      before starting the capture [default: 5]
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
RetroArch's `key = "value"` lines. Only the keys the tool needs are read:
`libretro_directory`, `savestate_directory`.

`AppendConfig` builder producing the text of the temp appendconfig. Always
emits:

```
config_save_on_exit = "false"
savestate_auto_save = "false"
savestate_auto_load = "false"
pause_nonactive = "false"
```

`pause_nonactive` matters because RetroArch stops rendering when its window
is not focused, which would starve the capture of frame boundaries.

Window mode adds:

- Fill (default): `video_fullscreen = "false"`, `video_scale = "20"`,
  `video_window_auto_width_max = "<W>"`,
  `video_window_auto_height_max = "<H>"` where W and H are the main display's
  visible frame (NSScreen, excludes menu bar and dock) minus a 28-point title
  bar allowance. RetroArch shrinks the scaled window to fit these maxima while
  keeping aspect ratio (`gfx/video_driver.c`, windowed size computation).
- `--size WxH`: `video_fullscreen = "false"`,
  `video_window_save_positions = "true"`,
  `video_windowed_position_width = "<W>"`,
  `video_windowed_position_height = "<H>"`. On macOS the custom-size branch
  is gated on `video_window_save_positions`, not
  `video_window_custom_size_enable`. `config_save_on_exit=false` prevents the
  position being written back.
- `--scale N`: `video_fullscreen = "false"`, `video_scale = "<N>"`,
  `video_window_auto_width_max = "0"`, `video_window_auto_height_max = "0"`,
  `video_fullscreen_x = "0"`, `video_fullscreen_y = "0"` so nothing clamps it.
- `--fullscreen`: nothing in the config; `-f` on the command line.

State mode adds, for `--state <file>` only:

```
savestate_directory = "<tmpdir>/states"
sort_savestates_enable = "false"
sort_savestates_by_content_enable = "false"
savestates_in_content_dir = "false"
```

### `display` - main display size

`visible_size() -> (u32, u32)` via `objc2-app-kit` `NSScreen::mainScreen`
`visibleFrame`. Falls back to 1920x1080 with a warning if no screen is found.

### `state` - staging the save state

`stage(state_path, rom_path, tmpdir) -> Result<u32>` copies the state file to
`<tmpdir>/states/<rom file stem>.state` and returns slot 0. Slot 0 is the
bare `.state` suffix; slot N is `.stateN`. The `.state.png` thumbnail is not
needed and not copied.

`--slot N` stages nothing and passes `-e N`.

### `launch` - command line assembly

`build_command(plan) -> (PathBuf, Vec<OsString>, Vec<(String,String)>)`: a
pure function from a `LaunchPlan` struct to binary, args, and env. Args, in
order:

```
-L <core> [-f] [--set-shader <preset>] -e <slot> --appendconfig <tmp.cfg> [-v] <rom>
```

Env: `MTL_CAPTURE_ENABLED=1`. `MTLCAPTURE_WAIT_FOR_SIGNAL` is deliberately not
set: RetroArch must run freely to load the state and settle.

The binary is executed directly, not via `open`, so the environment reaches
it. The current directory is left alone.

### `capture` - driving gpucapture

1. Spawn RetroArch. Wrap the child in a guard whose `Drop` sends SIGKILL if
   the child is still running, so every early return cleans up.
2. Poll `gpucapture list` every 100 ms, up to 30 s, until the first column of
   some line equals the PID. If the child exits first, fail with its exit
   status and the last lines of its stderr.
3. Sleep `--settle` seconds.
4. Run `gpucapture start --pid <PID> --count <frames> --output <out>` and wait
   for it. Fail if it exits non-zero or `<out>` does not exist afterwards.
5. Unless `--keep-running`, send SIGTERM, wait up to 3 s, then SIGKILL.
6. Remove the temp dir.

If `gpucapture start` reports no capturable boundary (a MoltenVK risk noted
below), the error is surfaced verbatim; the fallback of `--until-exit` is left
to the user for now.

### `main`

Parses args with `clap`, reads the base config, resolves paths, builds the
appendconfig in a `tempfile::TempDir`, builds the command, runs the capture,
prints the output path on success.

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
retroarch-capture --core sameboy \
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
- **`-e` timing.** RetroArch loads the entry slot after content init. If a
  core needs a frame or two before a state load succeeds, the settle delay
  hides it, but a failed state load is silent. The verbose flag passes `-v`
  to RetroArch so its log shows the load result.

## Toolchain and dependencies

Rust 1.98 (`rust-version = "1.98"`, `rust-toolchain.toml` pinning `1.98.0`),
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
