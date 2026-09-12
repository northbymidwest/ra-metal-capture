# Review Fixes, `--aspect`, and `--param` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every finding of the 2026-09-12 whole-codebase review and add
two flags both backends honour: `--aspect` (float, `W:H`, or `native`) and a
repeatable `--param NAME=VALUE` that overrides shader preset parameters.

**Architecture:** One shared `interrupt` module serves both backends (flag,
registered child pid, second-signal escalation). Aspect is a shared
`config::Aspect` carried on `Request`; the hosted backend sizes its output
texture to the largest aspect-correct box the window mode allows (what
RetroArch's viewport is), the RetroArch backend writes
`aspect_ratio_index` and `video_aspect_ratio`. Parameter overrides are a
"simple preset" (`#reference` plus `NAME = "VALUE"` lines) written to a temp
dir by a shared `preset` module and handed to either backend as the shader.

**Tech Stack:** Rust 2024, clap derive, objc2, librashader 0.12, RetroArch
1.22 main (git 69a4f0ea) semantics for the run config.

**Spec:** the review report (session scratchpad `review.md`) and the design
agreed in chat on 2026-09-12. Key facts verified against RetroArch source at
69a4f0ea: `enum aspect_ratio` has `ASPECT_RATIO_CONFIG = 20` and
`ASPECT_RATIO_CORE = 22`; `aspect_ratio_index` selects, `video_aspect_ratio`
is the CONFIG value; `DEFAULT_ASPECT_RATIO_IDX` is CORE on macOS;
`video_force_aspect` defaults true; with force aspect a `video_scale` window is
`round(base_height * aspect) * scale` wide and `base_height * scale` tall.

## Global Constraints

- ASCII only in every tracked file (`scripts/check-ascii.sh`).
- `#![deny(unsafe_code)]` outside `src/hosted/render/` and `src/hosted/libretro/`.
- `cargo test` never constructs a Metal device or launches RetroArch.
- Both feature shapes stay green: default and `--no-default-features`.
- Tests assert concrete values; no test restates its implementation.
- No commits are made by the executor; the user reviews the working tree.

---

### Task 1: Shared interrupt module and RetroArch Ctrl-C unwinding

**Files:**
- Create: `src/interrupt.rs` (moved from `src/hosted/interrupt.rs`, extended)
- Delete: `src/hosted/interrupt.rs`
- Modify: `src/lib.rs`, `src/hosted/mod.rs`, `src/hosted/render/mod.rs`, `src/retroarch/capture.rs`

**Interfaces:**
- Produces: `interrupt::install()`, `interrupt::check() -> Result<()>`,
  `interrupt::register_child(pid: u32)`, `interrupt::clear_child()`.

- [ ] Step 1: Write the failing tests in `src/interrupt.rs` (module does not exist yet, so compile fails):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_reports_interrupted_once_the_flag_is_raised() {
        assert!(check().is_ok());
        INTERRUPTED.store(true, Ordering::SeqCst);
        let err = check().unwrap_err();
        assert!(err.is::<Interrupted>());
        INTERRUPTED.store(false, Ordering::SeqCst);
    }
    #[test]
    fn registered_child_is_what_the_handler_would_kill() {
        register_child(4242);
        assert_eq!(CHILD.load(Ordering::SeqCst), 4242);
        clear_child();
        assert_eq!(CHILD.load(Ordering::SeqCst), 0);
    }
}
```

- [ ] Step 2: `cargo test --lib interrupt` fails to compile (no module).
- [ ] Step 3: Implement `src/interrupt.rs`: statics `INTERRUPTED: AtomicBool`,
  `CHILD: AtomicU32`, `INSTALL: Once`. `install()` sets one ctrlc handler:
  on first signal store the flag and SIGKILL `CHILD` if non-zero
  (`nix::sys::signal::kill`); if the flag was already set, `std::process::exit(130)`.
  `check()`, `register_child`, `clear_child` as above. Module doc explains
  both backends and the second-signal escalation. Remove `pub mod interrupt`
  from `hosted/mod.rs`, add to `lib.rs`, fix the `use` in render and hosted.
- [ ] Step 4: In `capture.rs` delete `LAUNCHED_PID`, `CTRLC_HANDLER`,
  `install_ctrlc_handler`. `run` calls `interrupt::install()` and
  `interrupt::register_child(pid)` after spawn; `ChildGuard::drop` and the
  keep-running path call `interrupt::clear_child()`. Rename the body of
  `run` to `run_inner` and wrap: on `Err`, `discard_partial(&opts.output)`;
  delete the per-path `discard_partial` calls in `gpucapture_start`,
  `capture_settled`, `capture_paused`. Poll `interrupt::check()?` in
  `wait_capturable`, in a new `settle_wait(guard, settle, log_path)` that
  sleeps in 100 ms ticks and also calls `bail_if_exited`, in the
  `capture_settled` wait loop, in the arm wait (use `recv_timeout` in 100 ms
  slices), and in the closing-advance loop. `Interrupted` propagates through
  `?` and the guards run.
- [ ] Step 5: `cargo test` and `cargo test --no-default-features` pass; clippy both shapes.

### Task 2: `config::Aspect`, `Request.aspect`, `--aspect`

**Files:**
- Modify: `src/config.rs`, `src/backend.rs`, `src/main.rs`

**Interfaces:**
- Produces: `pub enum Aspect { Native, Ratio(f64) }` with `FromStr`,
  `Aspect::ratio_or(&self, native: f64) -> f64`, `Display`.

- [ ] Step 1: Tests in `config.rs`:

```rust
#[test]
fn aspect_parses_native_floats_and_ratios() {
    assert_eq!("native".parse::<Aspect>().unwrap(), Aspect::Native);
    assert_eq!("1.5".parse::<Aspect>().unwrap(), Aspect::Ratio(1.5));
    let r = "4:3".parse::<Aspect>().unwrap();
    assert!(matches!(r, Aspect::Ratio(v) if (v - 4.0 / 3.0).abs() < 1e-9));
}
#[test]
fn aspect_rejects_zero_negative_and_malformed() {
    for bad in ["0", "-1", "4:0", "0:3", "abc", "4:3:2", "", "nan", "inf"] {
        assert!(bad.parse::<Aspect>().is_err(), "{bad}");
    }
}
#[test]
fn aspect_ratio_or_uses_native_only_for_native() {
    assert_eq!(Aspect::Native.ratio_or(1.25), 1.25);
    assert_eq!(Aspect::Ratio(2.0).ratio_or(1.25), 2.0);
}
```

- [ ] Step 2: Run, fail to compile. Step 3: implement. `FromStr`: trim;
  `"native"` (case-insensitive) -> Native; `W:H` splits once on `:`, both
  finite positive f64 -> Ratio(w/h); else a finite positive f64 -> Ratio.
  Error strings name the input. Add `pub aspect: Aspect` to `Request`
  (after `window`). Add `--aspect` to `Cli` with `default_value = "native"`,
  help: "Viewport aspect: native (the core's, or the image's pixels), a
  ratio like 4:3, or a number like 1.3333". Wire into `build`. Test in
  `main.rs`: `parse_rom(&["--aspect", "4:3"])` gives `Aspect::Ratio(..)`,
  default is `Native`, `"--aspect", "0"` errors.
- [ ] Step 4: both shapes green.

### Task 3: Hosted sizing honours aspect; `--scale` in points; input size check

**Files:**
- Modify: `src/hosted/libretro/mod.rs` (AvInfo gains `aspect_ratio: f64`),
  `src/hosted/render/mod.rs`, `src/hosted/mod.rs`

**Interfaces:**
- `RenderOptions` gains `aspect: Aspect`. `FrameSource` gains
  `fn aspect_ratio(&self) -> f64` (native aspect; image = w/h; core = AV
  info aspect, or w/h when it is 0).
- `pub fn output_size(mode: &WindowMode, image: Size, aspect: f64, screen: &Screen) -> Size`.
- `pub fn display_size(image: Size, aspect: f64) -> Size`: `(round(h*aspect), h)`,
  the shape RetroArch sizes a window from.

- [ ] Step 1: Replace the sizing tests. With `GB = 160x144`, aspect 10/9
  (1.1111): `display_size(GB, 10.0/9.0) == 160x144`; `display_size(256x224, 4.0/3.0) == 299x224`.
  `output_size(Scale(4), GB, 10/9, screen(2.0)) == 1280x1152` (points times 2).
  `output_size(Exact(1600x1440), 256x224, 4/3, s) == 1600x1200` (the box inside).
  `output_size(Fullscreen, 256x224, 4/3, screen(2.0)) == 3840x2880` inside 5120x2880.
  `output_size(Fill{max 2488x1382}, GB, 10/9, screen(2.0)) == 3071x2764`
  (unchanged from today since GB's native aspect is its pixel aspect).
  Degenerate max falls back to `display_size`.
- [ ] Step 2: fail. Step 3: implement `display_size`, `fit(display, box)`
  for every mode (Exact: `fit(display, size)`; Scale: `to_pixels(display * n)`;
  Fullscreen: `fit(display, pixels(full))`; Fill: as before with display).
  In `run`: `let native = opts.source.aspect_ratio(); let aspect = opts.aspect.ratio_or(native);`
  and `check_output_size(image_size).context("the source image")?` before
  creating the input texture (rename the function's message to say
  "size {w}x{h}" without "output").
  `Core::load_game` keeps `av.geometry.aspect_ratio` in `AvInfo`; `Core`
  stores it; `impl FrameSource for Core` returns it (or w/h if <= 0).
  `hosted/mod.rs` passes `aspect` through; verbose line prints the aspect used.
- [ ] Step 4: `--scale` help becomes "Integer multiple of the source's
  native size, in points, like RetroArch's window scale". Both shapes green.

### Task 4: RetroArch run config writes the aspect

**Files:**
- Modify: `src/retroarch/runconfig.rs`, `src/retroarch/mod.rs`

- [ ] Step 1: `RunConfig` gains `pub aspect: Aspect`. Tests: `COMMON` grows
  two lines after `video_font_enable`: `video_force_aspect = "true"` and
  `aspect_ratio_index = "22"`. New test `render_aspect_ratio_override`:
  with `Aspect::Ratio(4.0/3.0)` the two lines are
  `aspect_ratio_index = "20"` then `video_aspect_ratio = "1.333333"` (format `{:.6}`).
- [ ] Step 2: fail. Step 3: implement, placing the lines right after
  `video_font_enable` and before the window block. `mod.rs` passes `request.aspect.clone()`.
- [ ] Step 4: green.

### Task 5: `--param NAME=VALUE` via a wrapper preset

**Files:**
- Create: `src/preset.rs`
- Modify: `src/lib.rs`, `src/backend.rs`, `src/main.rs`, `src/retroarch/mod.rs`, `src/hosted/mod.rs`

**Interfaces:**
- `pub struct Param { pub name: String, pub value: f64 }` with `FromStr`
  (`NAME=VALUE`; name non-empty, no whitespace or `=`; value finite f64).
- `pub fn write_override_preset(original: &Path, params: &[Param], dir: &Path) -> Result<PathBuf>`
  writes `dir/override.<ext>` (ext of original) containing
  `#reference "<absolute original>"\n` then one `NAME = "VALUE"\n` per param
  (value formatted with `{}` of f64), returns the path. Fails if the
  original path contains `"` or a newline.
- `Request.params: Vec<Param>`. A backend with params writes the wrapper
  into its temp dir and uses it as the shader; hosted creates a temp dir
  only when params is non-empty.

- [ ] Step 1: Tests: `Param` parsing (`"CRT_GAMMA=2.4"`, rejects `"=1"`,
  `"a b=1"`, `"a=x"`, `"a"`); `write_override_preset` in a tempdir with two
  params asserts the exact file text and the returned path name
  `override.slangp`; original with a quote errors.
- [ ] Step 2: fail. Step 3: implement. `--param` repeatable
  (`Vec<Param>`, `value_name = "NAME=VALUE"`). Help: "Override a preset
  parameter (repeatable); the run uses a wrapper preset that references
  --shader with these values". Hosted: if non-empty, `tempfile::tempdir()`
  held for the run; RetroArch: write into its existing `tmp`. Under `-v`
  print the wrapper path.
- [ ] Step 4: green both shapes. Real run (hosted, image): `--param` with a
  parameter of the vectorscale preset; confirm librashader accepts the
  `#reference` wrapper.

### Task 6: `--slot` verified under RetroArch; `-v` keeps the run dir

**Files:**
- Modify: `src/state.rs` (add `find_slot`), `src/retroarch/mod.rs`

- [ ] Step 1: Test `find_slot_searches_the_layout_and_one_level_of_subdirs`
  in `state.rs`: tempdir as `savestate_directory` with `sameboy/game.state3`;
  `find_slot(&dirs, Path::new("/r/game.gbc"), 3)` returns that path;
  slot 4 returns `None`; a file at the top level `game.state` is found for slot 0.
- [ ] Step 2: fail. Step 3: implement `pub fn find_slot(dirs: &StateDirs, rom: &Path, slot: u32) -> Option<PathBuf>`:
  candidates are `slot_path`'s directory logic with `core_name` unknown,
  so: the flat directory, every immediate subdirectory of it, and the
  content dir when `in_content_dir`. In `retroarch/mod.rs`, after locating
  the states directory, `find_slot(...)` must be `Some`, else
  `bail!("slot {n} for {rom} not found under {dir} (or one level below); pass --state with the file instead")`.
- [ ] Step 4: `-v`: after a successful run, `tmp.keep()` and print
  `kept {dir} (retroarch.cfg, retroarch.log)` when verbose, sharing the
  keep-running branch. Green.

### Task 7: CLI polish and no-default-features help

**Files:**
- Modify: `src/main.rs`, `src/config.rs`

- [ ] `--shader` and `--output` move to the top of `Cli` (after `command`).
- [ ] `--advance` help: "Frames to run after loading the state. librashader:
  the last one is the first recorded. retroarch: the capture is armed after
  the last one and closes a few advances later; the tool prints how many".
  README gets the same sentence.
- [ ] `BackendChoice::Librashader`, `DEFAULT_BACKEND`, `--skip-extension-check`,
  `librashader_only_flags`, and `hosted_backend` gated with
  `#[cfg(feature = "librashader")]`; without it `Source::Core.skip_extension_check`
  is `false` and `--backend` lists only `retroarch`. Test under
  `--no-default-features`: `parse_rom(&["--backend", "librashader"])` is an error.
- [ ] `config::expand_tilde(path: &Path) -> PathBuf` using `strip_prefix("~")`;
  update the three callers and its test.
- [ ] `main.rs`: module doc first, then `#![deny(unsafe_code)]`.
- [ ] `WindowMode` doc comments backend-neutral, pointing at `runconfig`
  and `render::output_size`.
- [ ] green both shapes.

### Task 8: Library hygiene

**Files:**
- Delete: `src/core.rs`; Create: `src/hosted/state.rs`
- Modify: `src/layout.rs`, `src/state.rs`, `src/hosted/mod.rs`, `src/hosted/render/mod.rs`,
  `src/hosted/libretro/mod.rs`, `src/retroarch/mod.rs`, `Cargo.toml`, `src/lib.rs`

- [ ] Fold `resolve_core` into `layout.rs` as a private `fn core_in(dir, arg) -> Option<PathBuf>`
  returning the first existing candidate; `DirResolver::core` uses it in
  both the `exists` closure and the `Found` arm; move its tests. Delete `core.rs` and the `pub mod core`.
- [ ] `system_dir`: replace the dead `unwrap_or_else` with
  `.expect("locate always tries the default layout first")`.
- [ ] Move `decode`, `u32_at`, `MAX_STATE_BYTES`, `unrzip`, `rastate_mem`
  and their tests to `src/hosted/state.rs` (no cfg attributes needed there);
  `hosted/mod.rs` calls `state::decode` from the new module. Top-level
  `state.rs` keeps `stage`, `StateDirs`, `slot_path`, `find_slot`, unconditional.
- [ ] `stage` returns `Result<()>`; the caller uses slot 0 explicitly. Test updated.
- [ ] `unrzip`: wrap the decoder in `.take((total - out.len() + 1) as u64)`,
  and after each chunk `if out.len() > total { bail!("rzip chunk inflates past the {total} bytes the header promises") }`.
  New test: a chunk whose inflated size exceeds the header's total is rejected
  with "inflates past". New test: `#RZIPv\x02#` errors with "version 2".
- [ ] `config::read_all` test: a value containing `=` keeps everything after the first.
- [ ] `hosted/mod.rs`: comment at the first `refuse_zip` explaining the
  second call in `load_game` is the library-level guard; return the
  `StdoutToStderr` as a third tuple element instead of the `mut` slot;
  the zip test's output path becomes `tmp.path().join("x.gputrace")`.
- [ ] `hosted/render/mod.rs` doc: "one of the two modules (with
  `hosted::libretro`) that allow `unsafe`". `libretro/mod.rs` dlopen SAFETY
  comment: state the constructor assumption as an assumption.
- [ ] `Cargo.toml`: `objc2-foundation` features become `["NSGeometry"]`
  with `"objc2-foundation/NSString"`, `"objc2-foundation/NSURL"`,
  `"objc2-foundation/NSError"` and `"nix/fs"` added to the `librashader`
  feature list; `nix` features `["signal"]`.
- [ ] green both shapes, clippy, doc, machete, deny.

### Task 9: Docs

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `CLAUDE.md`

- [ ] README "Making RetroArch.app capturable" shrinks to: one sentence that
  the RetroArch backend needs RetroArch.app to carry the
  `com.apple.security.get-task-allow` entitlement, which libretro's builds
  lack, and that the tool adds it; the `ra-metal-capture entitle` example;
  one sentence on `--app`, repeating after updates, and the Gatekeeper note.
  No manual recipe.
- [ ] README: `--aspect` and `--param` in Usage with one example each; the
  `--advance` sentence from Task 7; a pointer to
  https://github.com/libretro/slang-shaders for a machine without RetroArch's shader pack.
- [ ] CHANGELOG Unreleased: Added `--aspect`, `--param`; Changed: hosted
  output sized to the aspect (frame may be smaller than `--size`), `--scale`
  in points, RetroArch Ctrl-C cleanup, partial bundle discard on every
  failure, `--slot` verified under RetroArch, `-v` keeps the run dir, second
  Ctrl-C exits at once, no-default build hides librashader; library API
  changes (`interrupt`, `preset`, `core` removed, `hosted::state`,
  `AvInfo.aspect_ratio`, `RunConfig.aspect`, `Request.aspect`/`params`).
- [ ] CLAUDE.md: `git config core.hooksPath .githooks` under Commands;
  architecture paragraph updates (interrupt shared, preset module, aspect
  sizing rule, no `core` module).
- [ ] `./scripts/check-ascii.sh`, `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`.

### Task 10: Real runs

- [ ] Hosted image run with `--aspect 4:3 --param <name>=<value> -v`; confirm
  the printed output size is the 4:3 box and the bundle has an `index`.
- [ ] Hosted core run if a core and ROM are at hand (`--core sameboy`), else skip and say so.
- [ ] RetroArch backend image run with `--aspect 4:3`; Ctrl-C a second run
  during settle and confirm exit 130, "interrupted", no temp dir, no partial bundle.
