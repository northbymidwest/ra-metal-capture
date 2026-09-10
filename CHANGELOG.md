# Changelog

Notable changes per release. Dates are the publish date.

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
