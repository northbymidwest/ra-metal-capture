# retroarch-capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust CLI that launches a chosen RetroArch.app with a core, ROM, save state and shader preset, sizes the window, and records N presented frames to a `.gputrace` with Apple's `gpucapture(1)`.

**Architecture:** Small pure modules (binary/core resolution, config parsing, appendconfig rendering, state staging, command assembly) that are unit tested without RetroArch, plus one effectful `capture` module that spawns RetroArch under a kill-on-drop guard, polls `gpucapture list`, and runs `gpucapture start`. `main.rs` only wires them together around a `tempfile::TempDir`.

**Tech Stack:** Rust 1.98 edition 2024, clap 4.6 (derive), anyhow 1.0, tempfile 3.27, nix 0.31 (signal), objc2-app-kit / objc2-foundation 0.3.

**Spec:** `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md`

## Global Constraints

- `rust-version = "1.98"` (MSRV only, no `rust-toolchain.toml`), `edition = "2024"`. Build and test with `cargo` on the default toolchain, which must be 1.98 or newer.
- Dependency versions are the current stable releases as of 2026-09-09: `clap = "4.6.6"`, `anyhow = "1.0.104"`, `tempfile = "3.27.0"`, `nix = "0.31.3"`, `objc2-app-kit = "0.3.2"`, `objc2-foundation = "0.3.2"`. Do not add others.
- No `unsafe` outside `src/display.rs`.
- Never write to the user's `retroarch.cfg` or savestate directory. All generated files go under a `TempDir`.
- No em dashes or en dashes anywhere in code, comments, docs, or commit messages.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Verified facts to rely on: `gpucapture start --pid P --count N --output OUT` blocks until the trace is written and exits 0 (measured on 2026-09-09, 0.4 s for one frame). `gpucapture list` prints a header line then one line per process with the PID in the first column. `NSScreen::mainScreen(mtm).visibleFrame()` works from a plain CLI process and returns points (2488 x 1410 on the dev machine).

---

## File structure

```
Cargo.toml                 crate manifest, pinned deps
.gitignore                 target/
README.md                  usage and the manual end-to-end check
src/main.rs                clap Cli, orchestration only
src/app.rs                 resolve_binary: .app or binary -> executable path
src/config.rs              read_keys, expand_tilde, WindowMode, AppendConfig::render
src/core.rs                resolve_core: path or bare name -> .dylib path
src/display.rs             visible_size via NSScreen (the only unsafe-adjacent code)
src/state.rs               stage: copy a .state file into a temp states dir as slot 0
src/launch.rs              LaunchPlan -> LaunchCommand (program, args, env)
src/capture.rs             ChildGuard, parse_capturable_pids, run
```

---

### Task 1: Crate scaffold and `app::resolve_binary`

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `src/main.rs`, `src/app.rs`

**Interfaces:**
- Produces: `app::resolve_binary(app: &Path) -> anyhow::Result<PathBuf>`

- [ ] **Step 1: Write the manifest and gitignore**

`Cargo.toml`:
```toml
[package]
name = "retroarch-capture"
version = "0.1.0"
edition = "2024"
rust-version = "1.98"
description = "Launch RetroArch with a ROM, save state and shader preset, then capture frames to .gputrace with gpucapture"
license = "0BSD"

[dependencies]
anyhow = "1.0.104"
clap = { version = "4.6.6", features = ["derive"] }
tempfile = "3.27.0"
nix = { version = "0.31.3", features = ["signal"] }
objc2-foundation = { version = "0.3.2", features = ["NSGeometry"] }
objc2-app-kit = { version = "0.3.2", features = ["NSScreen"] }
```

`.gitignore`:
```
target/
```

- [ ] **Step 2: Write a stub `src/main.rs` that declares the module**

```rust
mod app;

fn main() {}
```

- [ ] **Step 3: Write the failing tests in `src/app.rs`**

```rust
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolve a RetroArch `.app` bundle, or a bare executable, to the binary to exec.
pub fn resolve_binary(app: &Path) -> Result<PathBuf> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn app_bundle_resolves_to_inner_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("RetroArch.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/RetroArch"), b"").unwrap();
        assert_eq!(
            resolve_binary(&app).unwrap(),
            app.join("Contents/MacOS/RetroArch")
        );
    }

    #[test]
    fn bare_binary_is_returned_as_is() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("RetroArch");
        fs::write(&bin, b"").unwrap();
        assert_eq!(resolve_binary(&bin).unwrap(), bin);
    }

    #[test]
    fn app_bundle_without_binary_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("RetroArch.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        let err = resolve_binary(&app).unwrap_err().to_string();
        assert!(err.contains("Contents/MacOS/RetroArch"), "{err}");
    }

    #[test]
    fn missing_path_errors() {
        let err = resolve_binary(Path::new("/nonexistent/RetroArch.app"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test app::`
Expected: 4 tests FAIL (panic at `todo!()`).

- [ ] **Step 5: Implement `resolve_binary`**

```rust
pub fn resolve_binary(app: &Path) -> Result<PathBuf> {
    if app.extension().is_some_and(|e| e == "app") && app.is_dir() {
        let bin = app.join("Contents/MacOS/RetroArch");
        if bin.is_file() {
            return Ok(bin);
        }
        bail!("{} has no Contents/MacOS/RetroArch", app.display());
    }
    if app.is_file() {
        return Ok(app.to_path_buf());
    }
    bail!("RetroArch app not found at {}", app.display());
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test app::`
Expected: 4 tests PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/main.rs src/app.rs
git commit -m "Scaffold crate and resolve RetroArch binary from .app

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `config::read_keys` and `config::expand_tilde`

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Interfaces:**
- Produces: `config::expand_tilde(s: &str) -> PathBuf`
- Produces: `config::read_keys(text: &str, keys: &[&str]) -> HashMap<String, String>` (values unquoted, not tilde-expanded)

- [ ] **Step 1: Add `mod config;` to `src/main.rs`**

```rust
mod app;
mod config;

fn main() {}
```

- [ ] **Step 2: Write the failing tests in `src/config.rs`**

```rust
use std::collections::HashMap;
use std::path::PathBuf;

/// Expand a leading `~` or `~/` using `$HOME`. Other paths are returned unchanged.
pub fn expand_tilde(s: &str) -> PathBuf {
    todo!()
}

/// Read the named keys from RetroArch `key = "value"` config text.
/// Missing keys are simply absent from the map. Surrounding quotes are stripped.
pub fn read_keys(text: &str, keys: &[&str]) -> HashMap<String, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_values() {
        let text = "video_driver = \"vulkan\"\nlibretro_directory = \"~/cores\"\n";
        let m = read_keys(text, &["libretro_directory", "video_driver"]);
        assert_eq!(m["libretro_directory"], "~/cores");
        assert_eq!(m["video_driver"], "vulkan");
    }

    #[test]
    fn skips_comments_blank_lines_and_unrequested_keys() {
        let text = "# comment\n\nfoo = \"1\"\nbar = \"2\"\n";
        let m = read_keys(text, &["bar"]);
        assert_eq!(m.len(), 1);
        assert_eq!(m["bar"], "2");
    }

    #[test]
    fn missing_key_is_absent() {
        let m = read_keys("a = \"1\"\n", &["b"]);
        assert!(m.get("b").is_none());
    }

    #[test]
    fn tolerates_unquoted_values_and_extra_whitespace() {
        let m = read_keys("  x   =   3  \n", &["x"]);
        assert_eq!(m["x"], "3");
    }

    #[test]
    fn expands_tilde_prefix() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/a/b"), PathBuf::from(format!("{home}/a/b")));
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel/~x"), PathBuf::from("rel/~x"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test config::`
Expected: 5 tests FAIL.

- [ ] **Step 4: Implement both functions**

```rust
pub fn expand_tilde(s: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (s, home) {
        ("~", Some(home)) => home,
        (s, Some(home)) if s.starts_with("~/") => home.join(&s[2..]),
        (s, _) => PathBuf::from(s),
    }
}

pub fn read_keys(text: &str, keys: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if !keys.contains(&k) {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        out.insert(k.to_string(), v.to_string());
    }
    out
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test config::`
Expected: 5 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/config.rs
git commit -m "Parse the RetroArch config keys the tool needs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `core::resolve_core`

**Files:**
- Create: `src/core.rs`
- Modify: `src/main.rs` (add `mod core;`)

**Interfaces:**
- Produces: `core::resolve_core(arg: &str, libretro_dir: &Path) -> anyhow::Result<PathBuf>`

- [ ] **Step 1: Add `mod core;` to `src/main.rs`**

```rust
mod app;
mod config;
mod core;

fn main() {}
```

- [ ] **Step 2: Write the failing tests in `src/core.rs`**

```rust
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Resolve a core argument. An existing file path is used as-is. Otherwise
/// `<libretro_dir>/<arg>` and `<libretro_dir>/<arg>_libretro.dylib` are tried.
pub fn resolve_core(arg: &str, libretro_dir: &Path) -> Result<PathBuf> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn existing_path_is_used_directly() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("x_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(
            resolve_core(core.to_str().unwrap(), Path::new("/nowhere")).unwrap(),
            core
        );
    }

    #[test]
    fn bare_name_gets_libretro_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("sameboy_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(resolve_core("sameboy", tmp.path()).unwrap(), core);
    }

    #[test]
    fn file_name_in_dir_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let core = tmp.path().join("sameboy_libretro.dylib");
        fs::write(&core, b"").unwrap();
        assert_eq!(
            resolve_core("sameboy_libretro.dylib", tmp.path()).unwrap(),
            core
        );
    }

    #[test]
    fn unknown_core_errors_listing_what_was_tried() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_core("nope", tmp.path()).unwrap_err().to_string();
        assert!(err.contains("nope_libretro.dylib"), "{err}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test core::`
Expected: 4 tests FAIL.

- [ ] **Step 4: Implement `resolve_core`**

```rust
pub fn resolve_core(arg: &str, libretro_dir: &Path) -> Result<PathBuf> {
    let direct = PathBuf::from(arg);
    if direct.is_file() {
        return Ok(direct);
    }
    let candidates = [
        libretro_dir.join(arg),
        libretro_dir.join(format!("{arg}_libretro.dylib")),
    ];
    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Ok(found.clone());
    }
    bail!(
        "core {arg:?} not found; tried {} and {}",
        candidates[0].display(),
        candidates[1].display()
    );
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test core::`
Expected: 4 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/core.rs
git commit -m "Resolve a libretro core by path or bare name

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `config::WindowMode` and `config::AppendConfig::render`

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum WindowMode {
      Fill { max_width: u32, max_height: u32 },
      Size { width: u32, height: u32 },
      Scale(u32),
      Fullscreen,
  }
  pub struct AppendConfig { pub window: WindowMode, pub staged_states_dir: Option<PathBuf> }
  impl AppendConfig { pub fn render(&self) -> String }
  ```

- [ ] **Step 1: Add the types and failing tests to `src/config.rs`**

Append to the file (above the existing `#[cfg(test)]` block, and add the new tests inside that block):

```rust
/// How the RetroArch window is sized for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMode {
    /// Windowed, scaled up and then clamped to these maxima (points).
    Fill { max_width: u32, max_height: u32 },
    /// Windowed, exactly this size (points).
    Size { width: u32, height: u32 },
    /// Windowed, an integer multiple of the core's native resolution, unclamped.
    Scale(u32),
    /// `-f` on the command line; nothing in the config.
    Fullscreen,
}

/// The per-run appendconfig. Rendered text overrides the user's config for
/// this launch only; the user's file is never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendConfig {
    pub window: WindowMode,
    /// When a `--state` file was staged, the temp savestate dir to point RetroArch at.
    pub staged_states_dir: Option<PathBuf>,
}

impl AppendConfig {
    pub fn render(&self) -> String {
        todo!()
    }
}
```

Tests to add inside `mod tests`:

```rust
    const COMMON: &str = "config_save_on_exit = \"false\"\n\
savestate_auto_save = \"false\"\n\
savestate_auto_load = \"false\"\n\
pause_nonactive = \"false\"\n";

    #[test]
    fn render_fill_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Fill { max_width: 2488, max_height: 1382 },
            staged_states_dir: None,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"20\"\n\
video_window_auto_width_max = \"2488\"\n\
video_window_auto_height_max = \"1382\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_size_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Size { width: 1600, height: 1440 },
            staged_states_dir: None,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"true\"\n\
video_windowed_position_width = \"1600\"\n\
video_windowed_position_height = \"1440\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_scale_mode() {
        let cfg = AppendConfig {
            window: WindowMode::Scale(4),
            staged_states_dir: None,
        };
        let expected = format!(
            "{COMMON}video_fullscreen = \"false\"\n\
video_window_save_positions = \"false\"\n\
video_scale = \"4\"\n\
video_window_auto_width_max = \"0\"\n\
video_window_auto_height_max = \"0\"\n\
video_fullscreen_x = \"0\"\n\
video_fullscreen_y = \"0\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_fullscreen_adds_nothing() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
        };
        assert_eq!(cfg.render(), COMMON);
    }

    #[test]
    fn render_staged_states_dir() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
        };
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test config::tests::render`
Expected: 5 tests FAIL.

- [ ] **Step 3: Implement `render`**

```rust
impl AppendConfig {
    pub fn render(&self) -> String {
        let mut lines: Vec<(&str, String)> = vec![
            ("config_save_on_exit", "false".into()),
            ("savestate_auto_save", "false".into()),
            ("savestate_auto_load", "false".into()),
            ("pause_nonactive", "false".into()),
        ];
        match &self.window {
            WindowMode::Fill { max_width, max_height } => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "false".into()));
                lines.push(("video_scale", "20".into()));
                lines.push(("video_window_auto_width_max", max_width.to_string()));
                lines.push(("video_window_auto_height_max", max_height.to_string()));
            }
            WindowMode::Size { width, height } => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "true".into()));
                lines.push(("video_windowed_position_width", width.to_string()));
                lines.push(("video_windowed_position_height", height.to_string()));
            }
            WindowMode::Scale(n) => {
                lines.push(("video_fullscreen", "false".into()));
                lines.push(("video_window_save_positions", "false".into()));
                lines.push(("video_scale", n.to_string()));
                lines.push(("video_window_auto_width_max", "0".into()));
                lines.push(("video_window_auto_height_max", "0".into()));
                lines.push(("video_fullscreen_x", "0".into()));
                lines.push(("video_fullscreen_y", "0".into()));
            }
            WindowMode::Fullscreen => {}
        }
        if let Some(dir) = &self.staged_states_dir {
            lines.push(("savestate_directory", dir.display().to_string()));
            lines.push(("sort_savestates_enable", "false".into()));
            lines.push(("sort_savestates_by_content_enable", "false".into()));
            lines.push(("savestates_in_content_dir", "false".into()));
        }
        lines
            .into_iter()
            .map(|(k, v)| format!("{k} = \"{v}\"\n"))
            .collect()
    }
}
```

Why these keys: RetroArch's windowed size is `core geometry * video_scale`, shrunk to fit `video_window_auto_width_max` / `height_max` (`gfx/video_driver.c`). On macOS the custom-size branch is gated on `video_window_save_positions`, not `video_window_custom_size_enable`. `pause_nonactive=false` keeps frames flowing when the window is not focused, otherwise gpucapture would see no boundaries.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test config::`
Expected: 10 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "Render the per-run appendconfig for each window mode

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `display::visible_size` and `display::fill_mode`

**Files:**
- Create: `src/display.rs`
- Modify: `src/main.rs` (add `mod display;`)

**Interfaces:**
- Consumes: `config::WindowMode`
- Produces: `display::visible_size() -> (u32, u32)`, `display::fill_mode(visible: (u32, u32)) -> WindowMode`, `display::TITLE_BAR_POINTS: u32`

- [ ] **Step 1: Add `mod display;` to `src/main.rs`**

```rust
mod app;
mod config;
mod core;
mod display;

fn main() {}
```

- [ ] **Step 2: Write the failing test for the pure part in `src/display.rs`**

```rust
use crate::config::WindowMode;
use objc2_app_kit::NSScreen;
use objc2_foundation::MainThreadMarker;

/// Allowance for the window title bar, which the visible frame does not exclude.
pub const TITLE_BAR_POINTS: u32 = 28;

const FALLBACK: (u32, u32) = (1920, 1080);

/// Width and height in points of the main display's visible frame
/// (excludes the menu bar and dock). Falls back to 1920x1080 with a
/// warning on stderr when no screen can be queried.
pub fn visible_size() -> (u32, u32) {
    todo!()
}

/// The fill-the-screen window mode for a given visible size.
pub fn fill_mode(visible: (u32, u32)) -> WindowMode {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_mode_subtracts_title_bar() {
        assert_eq!(
            fill_mode((2488, 1410)),
            WindowMode::Fill { max_width: 2488, max_height: 1382 }
        );
    }

    #[test]
    fn fill_mode_saturates_on_tiny_heights() {
        assert_eq!(
            fill_mode((100, 10)),
            WindowMode::Fill { max_width: 100, max_height: 0 }
        );
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test display::`
Expected: 2 tests FAIL.

- [ ] **Step 4: Implement both functions**

```rust
pub fn visible_size() -> (u32, u32) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("warning: not on the main thread; assuming a 1920x1080 display");
        return FALLBACK;
    };
    let Some(screen) = NSScreen::mainScreen(mtm) else {
        eprintln!("warning: no main screen; assuming a 1920x1080 display");
        return FALLBACK;
    };
    let frame = screen.visibleFrame();
    (frame.size.width as u32, frame.size.height as u32)
}

pub fn fill_mode(visible: (u32, u32)) -> WindowMode {
    WindowMode::Fill {
        max_width: visible.0,
        max_height: visible.1.saturating_sub(TITLE_BAR_POINTS),
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass, and sanity-run the query**

Run: `cargo test display::`
Expected: 2 tests PASS.

Temporarily change `main` to `fn main() { println!("{:?}", display::visible_size()); }`, run `cargo run -q`, expect a plausible size such as `(2488, 1410)`, then restore `fn main() {}`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/display.rs
git commit -m "Query the main display's visible size for fill mode

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: `state::stage`

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

**Interfaces:**
- Produces: `state::stage(state: &Path, rom: &Path, states_dir: &Path) -> anyhow::Result<u32>` (returns the slot to pass to `-e`, always 0)

- [ ] **Step 1: Add `mod state;` to `src/main.rs`**

```rust
mod app;
mod config;
mod core;
mod display;
mod state;

fn main() {}
```

- [ ] **Step 2: Write the failing tests in `src/state.rs`**

```rust
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Copy a save state into `states_dir` under the name RetroArch expects for
/// this ROM's slot 0 (`<rom stem>.state`). Returns that slot number.
pub fn stage(state: &Path, rom: &Path, states_dir: &Path) -> Result<u32> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_state_as_slot_zero_named_after_rom_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("anything.state3");
        fs::write(&state, b"STATE").unwrap();
        let rom = Path::new("/roms/Zelda (U) [!].gb");
        let states_dir = tmp.path().join("states");

        let slot = stage(&state, rom, &states_dir).unwrap();

        assert_eq!(slot, 0);
        let dest = states_dir.join("Zelda (U) [!].state");
        assert_eq!(fs::read(dest).unwrap(), b"STATE");
    }

    #[test]
    fn missing_state_file_errors_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = stage(
            Path::new("/nonexistent/x.state"),
            Path::new("/roms/a.gb"),
            tmp.path(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("/nonexistent/x.state"), "{err}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test state::`
Expected: 2 tests FAIL.

- [ ] **Step 4: Implement `stage`**

```rust
pub fn stage(state: &Path, rom: &Path, states_dir: &Path) -> Result<u32> {
    let stem = rom
        .file_stem()
        .with_context(|| format!("ROM path {} has no file name", rom.display()))?;
    fs::create_dir_all(states_dir)
        .with_context(|| format!("creating {}", states_dir.display()))?;
    let dest = states_dir.join(format!("{}.state", stem.to_string_lossy()));
    fs::copy(state, &dest).with_context(|| {
        format!("copying state {} to {}", state.display(), dest.display())
    })?;
    Ok(0)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test state::`
Expected: 2 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/state.rs
git commit -m "Stage a save state file as slot 0 in a temp states dir

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: `launch::build_command`

**Files:**
- Create: `src/launch.rs`
- Modify: `src/main.rs` (add `mod launch;`)

**Interfaces:**
- Produces:
  ```rust
  pub struct LaunchPlan {
      pub binary: PathBuf, pub core: PathBuf, pub rom: PathBuf, pub slot: u32,
      pub shader: Option<PathBuf>, pub appendconfig: PathBuf,
      pub fullscreen: bool, pub verbose: bool,
  }
  pub struct LaunchCommand { pub program: PathBuf, pub args: Vec<OsString>, pub env: Vec<(String, String)> }
  pub fn build_command(plan: &LaunchPlan) -> LaunchCommand
  impl LaunchCommand { pub fn to_command(&self) -> std::process::Command; pub fn display(&self) -> String }
  ```

- [ ] **Step 1: Add `mod launch;` to `src/main.rs`**

```rust
mod app;
mod config;
mod core;
mod display;
mod launch;
mod state;

fn main() {}
```

- [ ] **Step 2: Write the failing tests in `src/launch.rs`**

```rust
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Everything needed to assemble the RetroArch command line.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub binary: PathBuf,
    pub core: PathBuf,
    pub rom: PathBuf,
    pub slot: u32,
    pub shader: Option<PathBuf>,
    pub appendconfig: PathBuf,
    pub fullscreen: bool,
    pub verbose: bool,
}

/// A fully assembled command: program, arguments in order, extra environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
}

/// Pure assembly of the RetroArch invocation.
/// Order: `-L <core> [-f] [--set-shader <p>] -e <slot> --appendconfig <cfg> [-v] <rom>`.
/// Env: `MTL_CAPTURE_ENABLED=1` so GPUToolsCapture loads into RetroArch.
pub fn build_command(plan: &LaunchPlan) -> LaunchCommand {
    todo!()
}

impl LaunchCommand {
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).envs(self.env.iter().cloned());
        cmd
    }

    /// One-line rendering for `--verbose` output.
    pub fn display(&self) -> String {
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| format!("{:?}", a.to_string_lossy()))
            .collect();
        format!("{} {} {}", env.join(" "), self.program.display(), args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> LaunchPlan {
        LaunchPlan {
            binary: PathBuf::from("/Applications/RetroArch.app/Contents/MacOS/RetroArch"),
            core: PathBuf::from("/cores/sameboy_libretro.dylib"),
            rom: PathBuf::from("/roms/z.gb"),
            slot: 0,
            shader: None,
            appendconfig: PathBuf::from("/tmp/run/append.cfg"),
            fullscreen: false,
            verbose: false,
        }
    }

    fn strs(cmd: &LaunchCommand) -> Vec<String> {
        cmd.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn minimal_windowed_launch() {
        let cmd = build_command(&plan());
        assert_eq!(cmd.program, plan().binary);
        assert_eq!(
            strs(&cmd),
            [
                "-L", "/cores/sameboy_libretro.dylib",
                "-e", "0",
                "--appendconfig", "/tmp/run/append.cfg",
                "/roms/z.gb",
            ]
        );
        assert_eq!(cmd.env, vec![("MTL_CAPTURE_ENABLED".to_string(), "1".to_string())]);
    }

    #[test]
    fn fullscreen_shader_slot_and_verbose_in_order() {
        let mut p = plan();
        p.fullscreen = true;
        p.shader = Some(PathBuf::from("/shaders/crt.slangp"));
        p.slot = 3;
        p.verbose = true;
        let cmd = build_command(&p);
        assert_eq!(
            strs(&cmd),
            [
                "-L", "/cores/sameboy_libretro.dylib",
                "-f",
                "--set-shader", "/shaders/crt.slangp",
                "-e", "3",
                "--appendconfig", "/tmp/run/append.cfg",
                "-v",
                "/roms/z.gb",
            ]
        );
    }

    #[test]
    fn display_shows_env_program_and_quoted_args() {
        let s = build_command(&plan()).display();
        assert!(s.starts_with("MTL_CAPTURE_ENABLED=1 /Applications/RetroArch.app"), "{s}");
        assert!(s.ends_with("\"/roms/z.gb\""), "{s}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test launch::`
Expected: 3 tests FAIL.

- [ ] **Step 4: Implement `build_command`**

```rust
pub fn build_command(plan: &LaunchPlan) -> LaunchCommand {
    let mut args: Vec<OsString> = Vec::new();
    args.push("-L".into());
    args.push(plan.core.as_os_str().into());
    if plan.fullscreen {
        args.push("-f".into());
    }
    if let Some(shader) = &plan.shader {
        args.push("--set-shader".into());
        args.push(shader.as_os_str().into());
    }
    args.push("-e".into());
    args.push(plan.slot.to_string().into());
    args.push("--appendconfig".into());
    args.push(plan.appendconfig.as_os_str().into());
    if plan.verbose {
        args.push("-v".into());
    }
    args.push(plan.rom.as_os_str().into());
    LaunchCommand {
        program: plan.binary.clone(),
        args,
        env: vec![("MTL_CAPTURE_ENABLED".to_string(), "1".to_string())],
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test launch::`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/launch.rs
git commit -m "Assemble the RetroArch command line and capture env

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: `capture`: list parser, child guard, and the capture run

**Files:**
- Create: `src/capture.rs`
- Modify: `src/main.rs` (add `mod capture;`)

**Interfaces:**
- Consumes: `launch::LaunchCommand`
- Produces:
  ```rust
  pub fn parse_capturable_pids(list_output: &str) -> Vec<u32>
  pub struct CaptureOptions {
      pub settle: Duration, pub frames: u32, pub output: PathBuf,
      pub keep_running: bool, pub ready_timeout: Duration, pub log_path: PathBuf,
  }
  pub fn run(cmd: &LaunchCommand, opts: &CaptureOptions) -> anyhow::Result<()>
  ```

- [ ] **Step 1: Add `mod capture;` to `src/main.rs`**

```rust
mod app;
mod capture;
mod config;
mod core;
mod display;
mod launch;
mod state;

fn main() {}
```

- [ ] **Step 2: Write the failing tests for the pure parser in `src/capture.rs`**

```rust
use crate::launch::LaunchCommand;
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// PIDs listed by `gpucapture list`. The first line is a header; each
/// following line starts with the PID.
pub fn parse_capturable_pids(list_output: &str) -> Vec<u32> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pids_from_first_column() {
        let out = "    PID  Device  Name  GPU(ms)  \n  12084       0  loop     0.02  \n  99  0  RetroArch  1.0\n";
        assert_eq!(parse_capturable_pids(out), vec![12084, 99]);
    }

    #[test]
    fn empty_and_header_only_give_nothing() {
        assert!(parse_capturable_pids("").is_empty());
        assert!(parse_capturable_pids("    PID  Device  Name  GPU(ms)  \n").is_empty());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test capture::`
Expected: 2 tests FAIL.

- [ ] **Step 4: Implement the parser**

```rust
pub fn parse_capturable_pids(list_output: &str) -> Vec<u32> {
    list_output
        .lines()
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test capture::`
Expected: 2 tests PASS.

- [ ] **Step 6: Add the guard, options, and `run`**

Append to `src/capture.rs` above the tests module:

```rust
/// Kills the child on drop unless disarmed. Every early return from `run`
/// goes through this, so a launched RetroArch is never leaked.
struct ChildGuard {
    child: Child,
    armed: bool,
}

impl ChildGuard {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Some(status) if the child has exited.
    fn poll(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().context("polling RetroArch")
    }

    /// SIGTERM, wait up to `grace`, then SIGKILL via drop.
    fn terminate(&mut self, grace: Duration) {
        let _ = kill(Pid::from_raw(self.pid() as i32), Signal::SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.armed = false;
                return;
            }
            sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub struct CaptureOptions {
    /// Time to wait after RetroArch becomes capturable before capturing.
    pub settle: Duration,
    /// Number of frame boundaries to record.
    pub frames: u32,
    /// Absolute path of the `.gputrace` to write.
    pub output: PathBuf,
    /// Leave RetroArch running after the capture.
    pub keep_running: bool,
    /// How long to wait for the PID to appear in `gpucapture list`.
    pub ready_timeout: Duration,
    /// Where RetroArch's stdout and stderr are written.
    pub log_path: PathBuf,
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].join("\n")
}

fn gpucapture_list() -> Result<Vec<u32>> {
    let out = Command::new("gpucapture")
        .arg("list")
        .output()
        .context("running `gpucapture list`; is Xcode installed?")?;
    Ok(parse_capturable_pids(&String::from_utf8_lossy(&out.stdout)))
}

fn wait_capturable(guard: &mut ChildGuard, timeout: Duration, log_path: &Path) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let pid = guard.pid();
    loop {
        if let Some(status) = guard.poll()? {
            bail!(
                "RetroArch exited ({status}) before becoming capturable. Last log lines:\n{}",
                log_tail(log_path)
            );
        }
        if gpucapture_list()?.contains(&pid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "pid {pid} never appeared in `gpucapture list` within {timeout:?}. \
                 Is MTL_CAPTURE_ENABLED honoured by this RetroArch build? Last log lines:\n{}",
                log_tail(log_path)
            );
        }
        sleep(Duration::from_millis(100));
    }
}

/// Launch RetroArch, wait until it is capturable, settle, capture, terminate.
pub fn run(cmd: &LaunchCommand, opts: &CaptureOptions) -> Result<()> {
    let log = File::create(&opts.log_path)
        .with_context(|| format!("creating {}", opts.log_path.display()))?;
    let log_err = log.try_clone().context("cloning log handle")?;
    let child = cmd
        .to_command()
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("launching {}", cmd.program.display()))?;
    let mut guard = ChildGuard { child, armed: true };
    let pid = guard.pid();
    eprintln!("launched RetroArch as pid {pid}");

    wait_capturable(&mut guard, opts.ready_timeout, &opts.log_path)?;
    eprintln!("pid {pid} is capturable; settling for {:?}", opts.settle);
    sleep(opts.settle);
    if let Some(status) = guard.poll()? {
        bail!(
            "RetroArch exited ({status}) during settle. Last log lines:\n{}",
            log_tail(&opts.log_path)
        );
    }

    let status = Command::new("gpucapture")
        .args(["start", "--pid", &pid.to_string(), "--count", &opts.frames.to_string()])
        .arg("--output")
        .arg(&opts.output)
        .status()
        .context("running `gpucapture start`")?;
    if !status.success() {
        bail!("`gpucapture start` failed with {status}");
    }
    if !opts.output.exists() {
        bail!(
            "`gpucapture start` succeeded but wrote nothing at {}",
            opts.output.display()
        );
    }

    if opts.keep_running {
        guard.armed = false;
    } else {
        guard.terminate(Duration::from_secs(3));
    }
    Ok(())
}
```

- [ ] **Step 7: Build to make sure it compiles cleanly**

Run: `cargo build && cargo test capture::`
Expected: builds with no warnings other than dead-code warnings for `run` (it is not wired yet); 2 tests PASS.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/capture.rs
git commit -m "Drive gpucapture against a guarded RetroArch child

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: `main.rs` CLI and orchestration

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes every module above.

- [ ] **Step 1: Write the failing CLI-parsing tests in `src/main.rs`**

Replace `src/main.rs` with:

```rust
mod app;
mod capture;
mod config;
mod core;
mod display;
mod launch;
mod state;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

use config::{AppendConfig, WindowMode};
use launch::{LaunchPlan, build_command};

fn default_config() -> PathBuf {
    config::expand_tilde("~/Library/Application Support/RetroArch/config/retroarch.cfg")
}

fn parse_size(s: &str) -> std::result::Result<(u32, u32), String> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| format!("expected WxH, got {s:?}"))?;
    let w: u32 = w.parse().map_err(|_| format!("bad width in {s:?}"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad height in {s:?}"))?;
    if w == 0 || h == 0 {
        return Err("width and height must be non-zero".into());
    }
    Ok((w, h))
}

/// Launch RetroArch with a ROM, save state and shader preset, then capture
/// frames to a .gputrace with gpucapture.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// RetroArch .app bundle, or the binary inside it
    #[arg(long, default_value = "/Applications/RetroArch.app")]
    app: PathBuf,

    /// Path to a libretro .dylib, or a bare name resolved in libretro_directory
    #[arg(long)]
    core: String,

    /// Content file to load
    #[arg(long)]
    rom: PathBuf,

    /// Save state file to load at launch (staged as slot 0 in a temp dir)
    #[arg(long, conflicts_with = "slot")]
    state: Option<PathBuf>,

    /// Slot to load from the configured savestate directory
    #[arg(long, conflicts_with = "state")]
    slot: Option<u32>,

    /// Shader preset (.slangp / .glslp)
    #[arg(long)]
    shader: Option<PathBuf>,

    /// retroarch.cfg to base the run on
    #[arg(long, default_value_os_t = default_config())]
    config: PathBuf,

    /// Exact window size in points, e.g. 1600x1440
    #[arg(long, value_parser = parse_size, conflicts_with_all = ["scale", "fullscreen"])]
    size: Option<(u32, u32)>,

    /// Integer scale of the core's native resolution
    #[arg(long, conflicts_with_all = ["size", "fullscreen"])]
    scale: Option<u32>,

    /// Launch fullscreen (-f)
    #[arg(long, conflicts_with_all = ["size", "scale"])]
    fullscreen: bool,

    /// Seconds to wait after RetroArch is capturable before capturing
    #[arg(long, default_value_t = 5.0)]
    settle: f64,

    /// Number of frame boundaries to capture
    #[arg(long, default_value_t = 1)]
    frames: u32,

    /// Output .gputrace path
    #[arg(long)]
    output: PathBuf,

    /// Leave RetroArch running after the capture
    #[arg(long)]
    keep_running: bool,

    /// Print the command line and appendconfig, and pass -v to RetroArch
    #[arg(short, long)]
    verbose: bool,
}

impl Cli {
    fn window_mode(&self) -> WindowMode {
        if self.fullscreen {
            WindowMode::Fullscreen
        } else if let Some((width, height)) = self.size {
            WindowMode::Size { width, height }
        } else if let Some(n) = self.scale {
            WindowMode::Scale(n)
        } else {
            display::fill_mode(display::visible_size())
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec!["retroarch-capture", "--core", "c", "--rom", "r", "--output", "o"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn minimal_args_parse_with_defaults() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.app, PathBuf::from("/Applications/RetroArch.app"));
        assert_eq!(cli.settle, 5.0);
        assert_eq!(cli.frames, 1);
        assert!(cli.state.is_none() && cli.slot.is_none());
    }

    #[test]
    fn state_and_slot_conflict() {
        assert!(parse(&["--state", "s", "--slot", "1"]).is_err());
    }

    #[test]
    fn window_modes_conflict_pairwise() {
        assert!(parse(&["--size", "1x1", "--scale", "2"]).is_err());
        assert!(parse(&["--size", "1x1", "--fullscreen"]).is_err());
        assert!(parse(&["--scale", "2", "--fullscreen"]).is_err());
    }

    #[test]
    fn size_parses_and_maps_to_window_mode() {
        let cli = parse(&["--size", "1600x1440"]).unwrap();
        assert_eq!(cli.window_mode(), WindowMode::Size { width: 1600, height: 1440 });
        assert!(parse(&["--size", "1600"]).is_err());
        assert!(parse(&["--size", "0x10"]).is_err());
    }

    #[test]
    fn scale_and_fullscreen_map_to_window_modes() {
        assert_eq!(parse(&["--scale", "4"]).unwrap().window_mode(), WindowMode::Scale(4));
        assert_eq!(parse(&["--fullscreen"]).unwrap().window_mode(), WindowMode::Fullscreen);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail or pass as expected**

Run: `cargo test tests::`
Expected: all 5 PASS (they test clap only). If any fails, fix the `Cli` attributes before moving on. `cargo build` must succeed with `run` still `todo!()`.

- [ ] **Step 3: Implement `run`**

```rust
fn run(cli: Cli) -> Result<()> {
    let binary = app::resolve_binary(&cli.app)?;

    let cfg_text = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("reading {}", cli.config.display()))?;
    let keys = config::read_keys(&cfg_text, &["libretro_directory"]);
    let libretro_dir = keys
        .get("libretro_directory")
        .map(|s| config::expand_tilde(s))
        .unwrap_or_else(|| config::expand_tilde("~/Library/Application Support/RetroArch/cores"));
    let core = core::resolve_core(&cli.core, &libretro_dir)?;

    if !cli.rom.is_file() {
        bail!("ROM not found at {}", cli.rom.display());
    }
    if let Some(shader) = &cli.shader
        && !shader.is_file()
    {
        bail!("shader preset not found at {}", shader.display());
    }

    let tmp = tempfile::Builder::new()
        .prefix("retroarch-capture-")
        .tempdir()
        .context("creating temp dir")?;

    let (slot, staged_states_dir) = match (&cli.state, cli.slot) {
        (Some(state_file), _) => {
            let dir = tmp.path().join("states");
            let slot = state::stage(state_file, &cli.rom, &dir)?;
            (slot, Some(dir))
        }
        (None, Some(n)) => (n, None),
        (None, None) => (0, None),
    };

    let append = AppendConfig {
        window: cli.window_mode(),
        staged_states_dir,
    };
    let appendconfig = tmp.path().join("append.cfg");
    std::fs::write(&appendconfig, append.render())
        .with_context(|| format!("writing {}", appendconfig.display()))?;

    let plan = LaunchPlan {
        binary,
        core,
        rom: cli.rom.clone(),
        slot,
        shader: cli.shader.clone(),
        appendconfig,
        fullscreen: cli.fullscreen,
        verbose: cli.verbose,
    };
    let cmd = build_command(&plan);

    if cli.verbose {
        eprintln!("appendconfig:\n{}", append.render());
        eprintln!("command: {}", cmd.display());
    }

    let output = std::path::absolute(&cli.output)
        .with_context(|| format!("resolving {}", cli.output.display()))?;
    let opts = capture::CaptureOptions {
        settle: Duration::from_secs_f64(cli.settle),
        frames: cli.frames,
        output: output.clone(),
        keep_running: cli.keep_running,
        ready_timeout: Duration::from_secs(30),
        log_path: tmp.path().join("retroarch.log"),
    };
    capture::run(&cmd, &opts)?;

    println!("{}", output.display());
    Ok(())
}
```

Note: with `--keep-running` the temp dir is kept (via `tmp.keep()`) instead of being removed when `run` returns, since RetroArch is still running and its `savestate_directory` may point into it.

- [ ] **Step 4: Build, run the full test suite, and check clippy**

Run: `cargo build && cargo test && cargo clippy -- -D warnings`
Expected: build succeeds, all 32 tests PASS, clippy clean. Fix any clippy findings before committing.

- [ ] **Step 5: Smoke-test the error paths without RetroArch**

```bash
cargo run -q -- --core nope --rom /nonexistent.gb --output /tmp/x.gputrace; echo "rc=$?"
```
Expected: non-zero exit and a message containing `nope_libretro.dylib`.

```bash
cargo run -q -- --app /nonexistent.app --core sameboy --rom /nonexistent.gb --output /tmp/x.gputrace; echo "rc=$?"
```
Expected: non-zero exit and `RetroArch app not found at /nonexistent.app`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Wire the CLI: resolve inputs, stage the run, capture

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: README and the manual end-to-end capture

**Files:**
- Create: `README.md`

- [ ] **Step 1: Run the real capture against Link's Awakening on SameBoy**

Find a ROM first (`--rom` must be an actual `.gb`/`.gbc` file; ask the user if none is obvious). The state exists at `~/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state`. Pick any preset under `~/Library/Application Support/RetroArch/shaders/shaders_slang/` (for example `crt/crt-geom.slangp`).

```bash
cargo run -q -- \
  --core sameboy \
  --rom "<path to the LADX rom>" \
  --state "$HOME/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" \
  --output /tmp/ladx.gputrace \
  --verbose
```

Expected: RetroArch opens windowed, nearly filling the screen, showing the loaded state with the shader applied; after about 5 s the tool prints `/tmp/ladx.gputrace` and RetroArch closes. `du -sh /tmp/ladx.gputrace` shows a bundle of at least a few hundred KB.

Verify the trace is readable with the workplace tooling:

```bash
cd /Users/mike/workplace/gputrace-bundle && cargo run -q --example list -- /tmp/ladx.gputrace 2>/dev/null | head || true
```
If there is no `list` example, `ls /tmp/ladx.gputrace` showing the bundle's files is enough.

If `gpucapture start` reports no boundary: run `gpucapture boundaries --pid <pid>` while RetroArch is up (use `--keep-running` and a second terminal) and record what it lists in the README under a "Known issues" heading. Do not silently change the tool; report it.

Also try the other two builds to confirm the `--app` argument works:

```bash
cargo run -q -- --app /Applications/RetroArch-nightly.app --core sameboy --rom "<rom>" --slot 0 --output /tmp/ladx-nightly.gputrace
cargo run -q -- --app /Applications/RetroArch-debug.app   --core sameboy --rom "<rom>" --slot 0 --output /tmp/ladx-debug.gputrace
```
Expected: both produce a trace. Note any that fail, with the error text, in the README.

- [ ] **Step 2: Write `README.md`**

````markdown
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
| `--state FILE` | save state to load; copied to a temp states dir as slot 0 |
| `--slot N` | load slot N from your real states dir instead |
| `--shader PRESET` | `.slangp` / `.glslp` passed via `--set-shader` |
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
`config_save_on_exit=false`, and disables savestate auto-save and auto-load.

## Verified

<fill in from Step 1: date, which apps produced a trace, trace size, and any
issues seen>
````

Replace the `<fill in ...>` line with the actual results from Step 1 before committing.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document usage and record the end-to-end capture result

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: Suppress on-screen notifications in the appendconfig

Added after the first end-to-end run: the "Load Content" start animation and
the on-screen text notifications (state loaded, controller autoconfig) were
visible at launch and would land in a captured frame.

**Files:**
- Modify: `src/config.rs` (the common block in `AppendConfig::render` and the `COMMON` test constant)
- Modify: `README.md` (the sentence listing what the appendconfig always sets)

**Interfaces:**
- Consumes: `config::AppendConfig::render`
- Produces: no signature changes; two more lines in the rendered common block.

- [ ] **Step 1: Update the `COMMON` test constant so the render tests fail**

In `src/config.rs`, inside `mod tests`, change `COMMON` to:

```rust
    const COMMON: &str = "config_save_on_exit = \"false\"\n\
savestate_auto_save = \"false\"\n\
savestate_auto_load = \"false\"\n\
pause_nonactive = \"false\"\n\
menu_show_load_content_animation = \"false\"\n\
video_font_enable = \"false\"\n";
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test config::tests::render`
Expected: all 5 render tests FAIL (rendered text lacks the two new lines).

- [ ] **Step 3: Add the two keys to the common block in `render`**

In `AppendConfig::render`, extend the initial `lines` vector so the common block reads:

```rust
        let mut lines: Vec<(&str, String)> = vec![
            ("config_save_on_exit", "false".into()),
            ("savestate_auto_save", "false".into()),
            ("savestate_auto_load", "false".into()),
            ("pause_nonactive", "false".into()),
            ("menu_show_load_content_animation", "false".into()),
            ("video_font_enable", "false".into()),
        ];
```

Why: `menu_show_load_content_animation` is the "Load Content" start animation; `video_font_enable=false` disables all OSD text rendering for the run, so no notification of any kind can appear in the captured frame. Both apply only to the per-run appendconfig.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test && cargo clippy -- -D warnings`
Expected: 32 tests PASS, clippy clean.

- [ ] **Step 5: Update the README sentence**

Replace the paragraph beginning "The appendconfig always sets" so it reads:

```
The appendconfig always sets `pause_nonactive=false` (RetroArch stops
rendering when unfocused, which would starve the capture),
`config_save_on_exit=false`, disables savestate auto-save and auto-load, and
turns off the "Load Content" start animation and all on-screen text
notifications (`menu_show_load_content_animation=false`,
`video_font_enable=false`) so nothing lands in the captured frame.
```

- [ ] **Step 6: Re-run one capture to confirm the notifications are gone**

```bash
rm -rf /tmp/ladx-clean.gputrace
cargo run -q -- --core sameboy \
  --rom "/Users/mike/workplace/vibeboy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc" \
  --state "$HOME/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" \
  --output /tmp/ladx-clean.gputrace --verbose
```
Expected: the printed appendconfig contains both new lines, the tool prints `/tmp/ladx-clean.gputrace`, and `du -sh` shows a bundle of tens of MB. `pgrep -fl RetroArch` is empty afterwards.

- [ ] **Step 7: Commit**

```bash
git add src/config.rs README.md docs/superpowers/specs/2026-09-09-retroarch-capture-design.md docs/superpowers/plans/2026-09-09-retroarch-capture.md
git commit -m "Suppress the load animation and OSD text during capture

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: Replace bare `(u32, u32)` sizes with a `Size` struct

Requested by the user after reading the code: a tuple gives no field names at
use sites and lets callers swap width and height. Rendered appendconfig text
is unchanged, so every exact-string test stays as it is.

**Files:**
- Modify: `src/config.rs` (add `Size`, change the `WindowMode` variants)
- Modify: `src/display.rs` (`FALLBACK`, `visible_size`, `fill_mode`, tests)
- Modify: `src/main.rs` (`parse_size`, the `size` arg, `window_mode`, tests)
- Modify: `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md` (the `display` and `config` component text)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Size { pub width: u32, pub height: u32 }
  pub enum WindowMode { Fill { max: Size }, Exact(Size), Scale(u32), Fullscreen }
  pub fn visible_size() -> Size
  pub fn fill_mode(visible: Size) -> WindowMode
  fn parse_size(s: &str) -> Result<Size, String>
  ```
- `TITLE_BAR_POINTS` stays a `u32` constant: it is a height offset, not a size.

- [ ] **Step 1: Add `Size` and change the variants in `src/config.rs`**

Above `WindowMode`:

```rust
/// A width and height in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}
```

Change the two variants (doc comments unchanged in meaning):

```rust
    /// Windowed, scaled up and then clamped to this maximum (points).
    Fill { max: Size },
    /// Windowed, exactly this size (points).
    Exact(Size),
```

In `render`, the match arms become `WindowMode::Fill { max } => { ... max.width.to_string() ... max.height.to_string() }` and `WindowMode::Exact(size) => { ... size.width ... size.height ... }`. Update the three test constructions in `config.rs` tests: `WindowMode::Fill { max: Size { width: 2488, height: 1382 } }` and `WindowMode::Exact(Size { width: 1600, height: 1440 })`.

- [ ] **Step 2: Update `src/display.rs`**

```rust
use crate::config::{Size, WindowMode};

const FALLBACK: Size = Size { width: 1920, height: 1080 };

pub fn visible_size() -> Size {
    // same body; the final line becomes
    Size { width: frame.size.width as u32, height: frame.size.height as u32 }
}

pub fn fill_mode(visible: Size) -> WindowMode {
    WindowMode::Fill {
        max: Size {
            width: visible.width,
            height: visible.height.saturating_sub(TITLE_BAR_POINTS),
        },
    }
}
```

Tests: `fill_mode(Size { width: 2488, height: 1410 })` expects `WindowMode::Fill { max: Size { width: 2488, height: 1382 } }`; `fill_mode(Size { width: 100, height: 10 })` expects `max: Size { width: 100, height: 0 }`.

- [ ] **Step 3: Update `src/main.rs`**

`parse_size` returns `Result<Size, String>` and ends with `Ok(Size { width: w, height: h })`. The arg is `size: Option<Size>`. `window_mode` uses `else if let Some(size) = self.size { WindowMode::Exact(size) }`. Import `Size` alongside `AppendConfig, WindowMode`. The CLI test expects `WindowMode::Exact(Size { width: 1600, height: 1440 })`.

- [ ] **Step 4: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 36 tests PASS (same count; no test added or removed), clippy clean. Rendered text unchanged, so the render tests prove the refactor did not alter output.

- [ ] **Step 5: Update the spec**

In the `display` section: `visible_size() -> Size`. In the `config` section where `WindowMode` is described, mention `Size { width, height }` and the `Fill { max: Size }` / `Exact(Size)` variants.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/display.rs src/main.rs docs/superpowers/specs/2026-09-09-retroarch-capture-design.md docs/superpowers/plans/2026-09-09-retroarch-capture.md
git commit -m "Name window dimensions with a Size struct

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 13: Only ever delete a path that is a gputrace bundle

The fix wave made `capture::run` remove any existing directory at `--output`
before capturing. That is right for a stale bundle and wrong for a typo such
as `--output ~/Documents`. The removal must be limited to things that are
unmistakably a previous capture.

**Files:**
- Modify: `src/capture.rs`
- Modify: `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md` (the `capture` step list)

**Interfaces:** no signature changes. New private helper:

```rust
/// True when `path` looks like a bundle this tool (or Xcode) wrote: a
/// directory whose name ends in `.gputrace` and which contains an `index`.
fn is_gputrace_bundle(path: &Path) -> bool
```

- [ ] **Step 1: Write the failing tests**

In `src/capture.rs` tests, using `tempfile::tempdir()`:

```rust
    #[test]
    fn recognises_a_bundle_only_with_suffix_and_index() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("a.gputrace");
        std::fs::create_dir_all(bundle.join("index")).unwrap();
        assert!(is_gputrace_bundle(&bundle));

        let no_index = tmp.path().join("b.gputrace");
        std::fs::create_dir_all(&no_index).unwrap();
        assert!(!is_gputrace_bundle(&no_index));

        let wrong_suffix = tmp.path().join("c");
        std::fs::create_dir_all(wrong_suffix.join("index")).unwrap();
        assert!(!is_gputrace_bundle(&wrong_suffix));

        assert!(!is_gputrace_bundle(&tmp.path().join("missing.gputrace")));
    }

    #[test]
    fn prepare_output_refuses_a_directory_that_is_not_a_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("docs");
        std::fs::create_dir_all(&dir).unwrap();
        let err = prepare_output(&dir).unwrap_err().to_string();
        assert!(err.contains("docs"), "{err}");
        assert!(dir.exists(), "must not delete a non-bundle directory");
    }

    #[test]
    fn prepare_output_removes_a_stale_bundle_and_tolerates_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("old.gputrace");
        std::fs::create_dir_all(bundle.join("index")).unwrap();
        prepare_output(&bundle).unwrap();
        assert!(!bundle.exists());
        prepare_output(&tmp.path().join("new.gputrace")).unwrap();
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test capture::`
Expected: 3 new tests FAIL (functions undefined).

- [ ] **Step 3: Implement**

```rust
fn is_gputrace_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "gputrace")
        && path.is_dir()
        && path.join("index").exists()
}

/// Make room for a new capture at `output`. A previous bundle is removed;
/// anything else that exists there is refused, so a mistyped path never
/// deletes user data.
fn prepare_output(output: &Path) -> Result<()> {
    if !output.exists() {
        return Ok(());
    }
    if !is_gputrace_bundle(output) {
        bail!(
            "{} exists and is not a .gputrace bundle; refusing to overwrite it",
            output.display()
        );
    }
    std::fs::remove_dir_all(output)
        .with_context(|| format!("removing stale bundle {}", output.display()))
}
```

Replace the inline `remove_dir_all` block in `run` with `prepare_output(&opts.output)?;`. Keep this call where the removal was (after settle, before `gpucapture start`), or move it to the top of `run` before spawning RetroArch so a refused path fails fast without launching anything; the latter is preferred.

- [ ] **Step 4: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 39 tests PASS, clippy clean.

- [ ] **Step 5: Spec**

In the `capture` step list, replace the stale-bundle sentence with: "Before spawning, if `<out>` exists it must be a previous `.gputrace` bundle (name suffix plus an `index` entry) and is removed; any other existing path is refused so a mistyped `--output` never deletes user data."

- [ ] **Step 6: Commit**

```bash
git add src/capture.rs docs/superpowers/specs/2026-09-09-retroarch-capture-design.md docs/superpowers/plans/2026-09-09-retroarch-capture.md
git commit -m "Refuse to overwrite anything at --output that is not a gputrace bundle

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
