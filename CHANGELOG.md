# Changelog

Notable changes per release. Dates are the publish date.

## 0.2.0 - 2026-09-10

### Added

- `--image FILE`: capture a static image through RetroArch's built-in
  image viewer core instead of an emulator core and ROM. It replaces
  `--core` and `--rom`, and is incompatible with `--state`, `--slot`, and
  `--advance`, since the viewer has no state to pause and load. The
  viewer reports the image's own pixel size as its content geometry, so
  window sizing and shader passes behave the same as with an emulator
  core, and the existing settle flow (no `--state` or `--slot`) captures
  it.

### Changed

- Breaking library API changes: `launch::LaunchPlan.core` is now
  `Option<PathBuf>` (was `PathBuf`); `launch::LaunchPlan.rom` is renamed to
  `content`; `config::AppendConfig` gained the public field
  `image_viewer: bool`. Any external code constructing either struct will
  need updating, which is why this is 0.2.0 rather than 0.1.2.

## 0.1.1 - 2026-09-10

### Added

- A library target, `ra_metal_capture`, exposing the modules the binary is
  built from, with a docs.rs configuration that documents the Apple target.
  docs.rs only documents library targets, so 0.1.0 had no documentation
  build.

## 0.1.0 - 2026-09-10

### Added

- Initial release. Launches a RetroArch.app with a core, ROM, save state and shader preset,
  sizes the window, and records presented frames to a `.gputrace` with
  Apple's `gpucapture(1)`.
- Paused capture: with `--state` or `--slot`, the state is loaded over
  RetroArch's command interface, the emulator is frame-advanced a fixed
  number of times, and the capture is armed, so the same inputs record the
  same emulated frame every run.
- Every override goes into a per-run appendconfig in a temp dir; nothing in
  `retroarch.cfg` or the savestate directory is modified.
