# Image Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture a shader pass over a static image using RetroArch's built-in image viewer core, with no emulator core, ROM, state, or pause dance.

**Architecture:** A new `--image <file>` flag replaces `--core` plus `--rom`. RetroArch loads an image as content with its compiled-in image viewer (`builtin_imageviewer_enable`), reporting the image size as core geometry, so window sizing and shader passes behave exactly as for an emulator frame. The viewer presents continuously, so the existing settle flow captures it. Measured 2026-09-10 (spike, at RetroArch's default window size): `sample.png` (160x144) with vectorscale.slangp captured in 0.5 s, 84M bundle. The tool's own real run, at fill window size, measured 144M instead; see the README and spec for that figure.

**Tech Stack:** unchanged (clap, anyhow, tempfile, nix, objc2).

**Spec:** `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md` (Task 2 amends it).

## Global Constraints

- No new dependencies; no `unsafe` outside `src/display.rs`; never write the user's config or states dir; no em dashes or en dashes; commit trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- `cargo test` and `cargo clippy --all-targets -- -D warnings` clean after every task.
- Branch `image-mode` from `main` at 8f10962 (v0.1.1, library target added).

---

### Task 1: `--image` flag, optional core, image-viewer appendconfig key

**Files:**
- Modify: `src/main.rs`, `src/launch.rs`, `src/config.rs`

**Interfaces:**
- `LaunchPlan { binary, core: Option<PathBuf>, content: PathBuf, shader, appendconfig, fullscreen, verbose }` (`rom` renamed to `content`; `-L <core>` emitted only when `core` is `Some`). Argument order: `[-L <core>] [-f] [--set-shader <p>] --appendconfig <cfg> [-v] <content>`.
- `AppendConfig { window, staged_states_dir, paused, image_viewer: bool }`; when `image_viewer` is true, `render` appends `builtin_imageviewer_enable = "true"` as the last line.
- `Cli`: `core: Option<String>` and `rom: Option<PathBuf>`, each `required_unless_present = "image"` and `conflicts_with = "image"`; new `image: Option<PathBuf>` with `conflicts_with_all = ["core", "rom", "state", "slot", "advance"]`; `core` and `rom` also `requires` each other.
- `pub const IMAGE_EXTENSIONS: [&str; 11] = ["jpg", "jpeg", "png", "bmp", "psd", "tga", "gif", "hdr", "pic", "ppm", "pgm"]` (the image core's list) in `main.rs`, checked case-insensitively against `--image`'s extension.

- [ ] **Step 1: Failing tests**

`src/launch.rs` tests: change the `plan()` helper to `core: Some(..)` and `content:`; add
```rust
    #[test]
    fn image_content_has_no_core_flag() {
        let mut p = plan();
        p.core = None;
        p.content = PathBuf::from("/img/sample.png");
        assert_eq!(
            strs(&build_command(&p)),
            ["--appendconfig", "/tmp/run/append.cfg", "/img/sample.png"]
        );
    }
```
`src/config.rs` tests: add `image_viewer: false` to every existing constructor, and
```rust
    #[test]
    fn render_image_viewer_key_last() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
            paused: None,
            image_viewer: true,
        };
        assert_eq!(cfg.render(), format!("{COMMON}builtin_imageviewer_enable = \"true\"\n"));
    }
```
`src/main.rs` tests: the `parse` helper currently always passes `--core c --rom r --output o`; split it into `parse_rom(args)` (as now) and `parse_raw(args)` (only the program name plus `--output o`), and add
```rust
    #[test]
    fn image_mode_parses_alone_and_conflicts_with_emulator_flags() {
        let cli = parse_raw(&["--image", "sample.png"]).unwrap();
        assert_eq!(cli.image, Some(PathBuf::from("sample.png")));
        assert!(cli.core.is_none() && cli.rom.is_none());
        assert!(parse_raw(&["--image", "s.png", "--core", "c"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--rom", "r"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--state", "x"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--slot", "1"]).is_err());
        assert!(parse_raw(&["--image", "s.png", "--advance", "2"]).is_err());
    }

    #[test]
    fn core_and_rom_are_required_together_without_image() {
        assert!(parse_raw(&[]).is_err());
        assert!(parse_raw(&["--core", "c"]).is_err());
        assert!(parse_raw(&["--rom", "r"]).is_err());
        assert!(parse_raw(&["--core", "c", "--rom", "r"]).is_ok());
    }

    #[test]
    fn image_extension_check() {
        assert!(is_image_path(Path::new("a.PNG")));
        assert!(is_image_path(Path::new("b.jpeg")));
        assert!(!is_image_path(Path::new("c.gbc")));
        assert!(!is_image_path(Path::new("noext")));
    }
```
with `fn is_image_path(path: &Path) -> bool` in `main.rs` (extension lowercased and looked up in `IMAGE_EXTENSIONS`).

- [ ] **Step 2: Run to see them fail** (`cargo test`; compile errors count as failing).

- [ ] **Step 3: Implement**

`main.rs` `run`: resolve `(core, content)`:
```rust
    let (core, content) = if let Some(image) = &cli.image {
        if !is_image_path(image) {
            bail!(
                "{} does not have an image extension the image viewer accepts ({})",
                image.display(),
                IMAGE_EXTENSIONS.join(", ")
            );
        }
        if !image.is_file() {
            bail!("image not found at {}", image.display());
        }
        (None, image.clone())
    } else {
        let core_arg = cli.core.as_deref().context("--core is required without --image")?;
        let rom = cli.rom.clone().context("--rom is required without --image")?;
        let core = core::resolve_core(core_arg, &libretro_dir)?;
        if !rom.is_file() {
            bail!("ROM not found at {}", rom.display());
        }
        (Some(core), rom)
    };
```
Use `content` where `cli.rom` was used (state staging, `LaunchPlan`). Set `image_viewer: cli.image.is_some()` on `AppendConfig`. Only read the base config's `libretro_directory` when a core is needed (or keep reading it unconditionally; either is fine, but the config file must still be read for both modes). Update the `Cli` doc comment: "Launch RetroArch with a ROM and save state, or a static image, plus a shader preset, then capture frames to a .gputrace with gpucapture."

`launch.rs`: `core: Option<PathBuf>`, `content: PathBuf`, `-L` conditional, doc comment order updated.

`config.rs`: field and render line.

- [ ] **Step 4: `cargo test && cargo clippy --all-targets -- -D warnings`** clean. Smoke: `cargo run -q -- --image /nonexistent.png --output /tmp/x.gputrace` reports "image not found"; `cargo run -q -- --image Cargo.toml --output /tmp/x.gputrace` reports the extension message; neither launches RetroArch.

- [ ] **Step 5: Commit**
```bash
git add src/main.rs src/launch.rs src/config.rs
git commit -m "Add --image mode using RetroArch's built-in image viewer

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Real run, README, spec

**Files:**
- Modify: `README.md`, `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md`, `.gitignore` is unchanged (`sample.png` is committed as a fixture)

- [ ] **Step 1: Real run**
```bash
cargo run -q -- --image sample.png \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/vectorscale/vectorscale.slangp" \
  --output /tmp/sample-vectorscale.gputrace --verbose
```
Expected: appendconfig ends with `builtin_imageviewer_enable = "true"`, command line has no `-L`, settle flow, output path printed, RetroArch exits, bundle of tens of MB. Record size. `pgrep -fl "MacOS/RetroArch"` empty afterwards.

- [ ] **Step 2: README**: Usage gains an image example; Options table gains `--image FILE` ("static image via RetroArch's image viewer; replaces --core and --rom; incompatible with --state, --slot, --advance"); new section "Image mode" explaining: the viewer reports the image size as core geometry so a 160x144 PNG behaves like a Game Boy frame; how to get a raw pre-shader frame from a save state (take a RetroArch screenshot with `video_gpu_screenshot = "false"` while the state is loaded; the `.state.png` thumbnail is post-shader and unsuitable); the caveat that frame-count and history-dependent shader passes see a frozen image. Verified section gains the dated run. Commit `sample.png` alongside as the fixture used.

- [ ] **Step 3: Spec**: CLI block gains `--image`; `launch` order updated; `config` section lists `builtin_imageviewer_enable`; a short "Image mode" paragraph with the measured fact.

- [ ] **Step 4: Commit**
```bash
git add README.md docs/superpowers/specs/2026-09-09-retroarch-capture-design.md sample.png docs/superpowers/plans/2026-09-10-image-mode.md
git commit -m "Document image mode and record the sample.png run

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
