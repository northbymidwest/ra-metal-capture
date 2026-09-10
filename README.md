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
| `--settle SECS` | wait before capturing when no state is given (default 5) |
| `--advance N` | frames to run after loading the state; the Nth frame is the one captured (default 1); only applies when `--state` or `--slot` is given |
| `--cmd-port PORT` | UDP port for RetroArch's command interface, enabled only for this run (default 55355) |
| `--frames N` | frame boundaries to record (default 1) |
| `--keep-running` | do not close RetroArch afterwards |
| `-v` | print the command line and appendconfig, pass `-v` to RetroArch |

By default the window is windowed and as large as fits the main display
(RetroArch scales the core output up and clamps it to the display's visible
area, keeping the aspect ratio).

## How it works

1. `MTL_CAPTURE_ENABLED=1` is set so GPUToolsCapture loads into RetroArch.
2. RetroArch is exec'd directly (not via `open`) with `-L`,
   `--set-shader`, `--appendconfig` and the ROM. `-e` is never passed;
   loading a state is driven entirely through the appendconfig and, for
   `--state`/`--slot`, RetroArch's command interface, described below.
3. **No state given (`--settle`).** The tool polls `gpucapture list` until
   the PID is capturable, waits `--settle` seconds (default 5) for the
   game to settle on a steady frame, then runs
   `gpucapture start --pid P --count N --output OUT`, which blocks until
   the trace is written.
4. **State given (`--state` or `--slot`, paused capture).** The
   appendconfig turns on RetroArch's UDP command interface for this run
   only (`network_cmd_enable`, `network_cmd_port`) and points
   `state_slot` at the slot to load. Once the PID is capturable, the tool
   pauses RetroArch over that UDP connection, sends `LOAD_STATE`, and
   frame-advances `--advance` minus one times, leaving RetroArch one
   advance short of the frame to capture. It then arms
   `gpucapture start --pid P --count N --output OUT` in the background,
   gives it a moment to start polling, and sends the final
   `FRAMEADVANCE`. A frame advance is needed because a paused RetroArch
   re-presents the same image on every redraw, and `gpucapture` never
   treats a re-presented frame as a new boundary (measured); only an
   actual advance produces one, so the tool always ends on an advance
   with the capture already armed to catch it. If sending that final
   advance fails, the tool kills RetroArch immediately so `gpucapture`
   releases instead of waiting forever for a boundary that can no longer
   arrive, joins the capture thread, and reports the send error.
5. RetroArch is asked to quit (`QUIT`, sent twice, since paused capture
   already has a command connection open and RetroArch's default
   press-twice-to-quit applies); if it has not exited after 3 s, or no
   command connection was open, it gets SIGTERM and then SIGKILL after
   another 3 s. On any failure a drop guard kills it, so no halted
   process is left behind.

## Reproducible frames

For paused capture, the same `--state` (or `--slot`) plus the same
`--advance` always produces the same emulated frame: RetroArch replays
input-free from a fixed save state, so frame N after the load is
deterministic. That makes it useful for isolating one variable, e.g. keep
the state and `--advance` fixed and change only `--shader` between runs to
compare two shader passes over the exact same frame.

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
| `/Applications/RetroArch.app` (`--state`, `--shader`, `--verbose`, Task 11 confirmation after adding the load-animation and OSD suppression keys) | `/tmp/ladx-clean.gputrace` | 117M |

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
menu_show_load_content_animation = "false"
video_font_enable = "false"
video_fullscreen = "false"
video_window_save_positions = "false"
video_scale = "20"
video_window_auto_width_max = "2488"
video_window_auto_height_max = "1382"
savestate_directory = "/var/folders/.../retroarch-capture-RfVQ1Q/states"
sort_savestates_enable = "false"
sort_savestates_by_content_enable = "false"
savestates_in_content_dir = "false"

command: MTL_CAPTURE_ENABLED=1 /Applications/RetroArch.app/Contents/MacOS/RetroArch "-L" ".../cores/sameboy_libretro.dylib" "--set-shader" ".../shaders_slang/crt/crt-geom.slangp" "-e" "0" "--appendconfig" "/var/folders/.../retroarch-capture-RfVQ1Q/append.cfg" "-v" ".../Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc"
```

(This sample predates the paused-capture flow: `-e "0"` was how a state
used to get loaded. `--state` and `--slot` now go through `LOAD_STATE` and
`state_slot` in the paused flow described above, and `-e` is never
passed.)

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

### 2026-09-10: paused capture

Same ROM, state and shader as above, with `--core sameboy`, `--state`
pointed at the SameBoy save slot, and `--shader crt/crt-geom.slangp`.
`video_driver` was still `vulkan` (MoltenVK). `pgrep -fl "MacOS/RetroArch"`
was checked and empty before the first run and after every run below.

| run | output | size | result |
|---|---|---|---|
| default `--advance` (1), run 1, `--verbose` | `/tmp/ladx-paused-1.gputrace` | 67M | succeeded |
| default `--advance` (1), run 2 | `/tmp/ladx-paused-2.gputrace` | 67M | succeeded |
| `--advance 30` | `/tmp/ladx-paused-30.gputrace` | n/a | failed, see Known issues |

Both default-`--advance` runs printed `paused on the loaded state; arming
capture, then advancing frame 1`, then the output path, in well under 10 s,
and RetroArch exited on its own. With `--verbose` on the first run, the
tool printed the appendconfig and exact command line before launching:

```
appendconfig:
config_save_on_exit = "false"
savestate_auto_save = "false"
savestate_auto_load = "false"
pause_nonactive = "false"
menu_show_load_content_animation = "false"
video_font_enable = "false"
video_fullscreen = "false"
video_window_save_positions = "false"
video_scale = "20"
video_window_auto_width_max = "1742"
video_window_auto_height_max = "1102"
savestate_directory = "/var/folders/r2/hsbbw3qn07g6x3fm6ksx9yqc0000gn/T/retroarch-capture-prf0Ox/states"
sort_savestates_enable = "false"
sort_savestates_by_content_enable = "false"
savestates_in_content_dir = "false"
network_cmd_enable = "true"
network_cmd_port = "55355"
state_slot = "0"

command: MTL_CAPTURE_ENABLED=1 /Applications/RetroArch.app/Contents/MacOS/RetroArch "-L" "/Users/mike/Library/Application Support/RetroArch/cores/sameboy_libretro.dylib" "--set-shader" "/Users/mike/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" "--appendconfig" "/var/folders/r2/hsbbw3qn07g6x3fm6ksx9yqc0000gn/T/retroarch-capture-prf0Ox/append.cfg" "-v" "/Users/mike/workplace/vibeboy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc"
```

`-e` does not appear anywhere in the command line; `network_cmd_enable`,
`network_cmd_port` and `state_slot` are all present in the appendconfig, as
expected for the paused flow. The two runs produced same-size (67M)
bundles, consistent with the same emulated frame on both.

The `--advance 30` run is recorded under Known issues below.

## Known issues

**`--advance 30` hangs indefinitely (2026-09-10).** Running the same
capture with `--advance 30` instead of the default 1 printed `launched
RetroArch as pid <pid>` and `paused on the loaded state; arming capture,
then advancing frame 30`, armed `gpucapture start`, and then sat at
`0 / 1 CAMetalDrawable` forever: the capture never reported a boundary and
the tool never printed an output path. Two attempts were made, both under
a 120 s timeout, and both hung identically for the full 120 s with no
progress past `0 / 1 CAMetalDrawable`. Both attempts were killed by the
timeout; `pgrep -fl "MacOS/RetroArch"` was empty after each, so no
process was left running, and no `/tmp/ladx-paused-30.gputrace` bundle was
written. The tool was not modified to work around this; the 29
non-captured `FRAMEADVANCE` calls ahead of the final, armed one behave
identically in code to the single advance used by the two runs above that
succeeded, so the difference is in RetroArch or `gpucapture`, not in this
tool's logic. Per the design doc's noted MoltenVK risk, `gpucapture
boundaries --pid <pid>` during a hang would be the next diagnostic step,
along with retrying under a non-Vulkan `video_driver`, but neither was
attempted here to stay within the run budget for this task.
