# retroarch-capture

Launches a RetroArch.app with a core, ROM, save state and shader preset,
sizes the window, and records presented frames to a `.gputrace` using
Apple's `gpucapture(1)`. Nothing in your `retroarch.cfg` or savestate
directory is modified: every override goes into a per-run appendconfig in a
temp dir.

Requires macOS with Xcode (for `gpucapture`) and Rust 1.98.

## Usage

```
cargo run -q -- \
  --app /Applications/RetroArch.app \
  --core sameboy \
  --rom "path/to/game.gb" \
  --state "path/to/game.state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" \
  --output /tmp/game.gputrace
```

Options:

| flag | meaning |
|---|---|
| `--app PATH` | `.app` bundle or its binary. Default `/Applications/RetroArch.app`. |
| `--core CORE` | `.dylib` path, or a bare name resolved in `libretro_directory` (`sameboy` finds `sameboy_libretro.dylib`). |
| `--rom PATH` | content to load |
| `--output PATH` | output `.gputrace` path (required) |
| `--state FILE` | save state to load; copied to a temp states dir as slot 0 |
| `--slot N` | load slot N from your real states dir instead |
| `--shader PRESET` | `.slangp` / `.glslp` passed via `--set-shader` |
| `--config PATH` | `retroarch.cfg` to base the run on; default `~/Library/Application Support/RetroArch/config/retroarch.cfg` |
| `--size WxH` | exact window size in points |
| `--scale N` | integer scale of the core's native resolution |
| `--fullscreen` | launch with `-f` |
| `--settle SECS` | wait after RetroArch is capturable before capturing (default 5) |
| `--frames N` | frame boundaries to record (default 1) |
| `--keep-running` | do not close RetroArch afterwards |
| `-v` | print the command line and appendconfig, pass `-v` to RetroArch |

By default the window is windowed and as large as fits the main display
(RetroArch scales the core output up and clamps it to the display's visible
area, keeping the aspect ratio).

## How it works

1. `MTL_CAPTURE_ENABLED=1` is set so GPUToolsCapture loads into RetroArch.
2. RetroArch is exec'd directly (not via `open`) with `-L`, `-e`,
   `--set-shader`, `--appendconfig` and the ROM.
3. The tool polls `gpucapture list` until the PID is capturable, waits the
   settle time, then runs `gpucapture start --pid P --count N --output OUT`,
   which blocks until the trace is written.
4. RetroArch gets SIGTERM (then SIGKILL after 3 s). On any failure a
   drop guard kills it, so no halted process is left behind.

The appendconfig always sets `pause_nonactive=false` (RetroArch stops
rendering when unfocused, which would starve the capture),
`config_save_on_exit=false`, disables savestate auto-save and auto-load, and
turns off the "Load Content" start animation and all on-screen text
notifications (`menu_show_load_content_animation=false`,
`video_font_enable=false`) so nothing lands in the captured frame.

## Verified

Tested 2026-09-10 against Link's Awakening DX on the SameBoy core
(`--core sameboy`, `--state` pointed at the SameBoy save slot, `--shader`
set to `crt/crt-geom.slangp`), with the machine's `video_driver` left at
`vulkan` (MoltenVK).

All three RetroArch builds produced a valid trace, one at a time, with no
process left behind afterward (`pgrep -fl RetroArch` was checked and empty
before the first run and after every run):

| app | output | size |
|---|---|---|
| `/Applications/RetroArch.app` (`--state`, `--shader`, `--verbose`) | `/tmp/ladx.gputrace` | 95M |
| `/Applications/RetroArch-nightly.app` (`--slot 0`) | `/tmp/ladx-nightly.gputrace` | 94M |
| `/Applications/RetroArch-debug.app` (`--slot 0`) | `/tmp/ladx-debug.gputrace` | 58M |

Each is a `gputrace` bundle directory (`index`, `metadata`, `capture`,
`device-resources-*`, many `MTLHeap-*` files, etc.), consistent with a
readable Metal GPU trace.

With `--verbose` on the first run, the tool printed the appendconfig and
the exact command line before launching:

```
appendconfig:
config_save_on_exit = "false"
savestate_auto_save = "false"
savestate_auto_load = "false"
pause_nonactive = "false"
video_fullscreen = "false"
video_window_save_positions = "false"
video_scale = "20"
video_window_auto_width_max = "2488"
video_window_auto_height_max = "1382"
savestate_directory = "/var/folders/.../retroarch-capture-aPoElL/states"
sort_savestates_enable = "false"
sort_savestates_by_content_enable = "false"
savestates_in_content_dir = "false"

command: MTL_CAPTURE_ENABLED=1 /Applications/RetroArch.app/Contents/MacOS/RetroArch "-L" ".../cores/sameboy_libretro.dylib" "--set-shader" ".../shaders_slang/crt/crt-geom.slangp" "-e" "0" "--appendconfig" "/var/folders/.../retroarch-capture-aPoElL/append.cfg" "-v" ".../Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc"
```

RetroArch launched, `gpucapture list` reported the PID capturable almost
immediately, the tool settled for the default 5 s, then ran the capture
(each capture itself finished in under a second: 0.6 s, 0.2 s, and 0.3 s
for the three runs respectively) before sending RetroArch a clean
SIGTERM. No manual window inspection was done during this run (the agent
does not have a screen to watch), but the process lifecycle matched the
expected behavior exactly: launch, become capturable, settle, capture,
close, with nothing left running.

No known issues were encountered; `gpucapture start` reported a boundary
and produced a trace on every run, so the `gpucapture boundaries`
diagnostic was not needed.
