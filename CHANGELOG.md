# Changelog

Notable changes per release. Dates are the publish date.

## 0.5.0 - 2026-09-11

### Added

- `backend` module: `Request`, `Source`, `StateSource`, `Interrupted`, and
  the `Backend` trait, the one interface both backends present to the
  binary.
- `retroarch::RetroArch` and `hosted::Hosted` (feature `librashader`), the
  two backends, holding everything that used to live in the binary.
- `bundle` module: `is_gputrace_bundle` and `prepare_output`, shared by
  both backends; `image_file`, the `--image` check both backends run
  against their own extension list.

### Changed

- `--shader` is required for every run; the RetroArch backend always
  passes it as `--set-shader`, so a capture of whatever shader RetroArch
  had configured is no longer a thing this tool does.
- `--core-options` applies to both backends. Without it a core under
  RetroArch now runs on its built-in defaults, as a hosted core always
  did, instead of the per-core options RetroArch had saved.
- Both backends consult the user's `retroarch.cfg` for one thing only:
  locating a bare core name, `--slot`, or the system directory when it is
  not in RetroArch's default place. `--slot N` under RetroArch resolves
  the states directory that way and lets RetroArch find the slot in it.
- `Request.config` is an `Option`: `None` consults no `retroarch.cfg` at
  all, for a machine without RetroArch; the binary passes its default.
- An output path whose directory does not exist is refused before
  anything is rendered or launched, naming the directory, instead of by
  Metal or RetroArch at the end of the run.
- `--settle` is capped at 3600 seconds; larger values were accepted and
  saturated into billions of warm-up frames.
- Ctrl-C (SIGINT or SIGTERM) ends either backend cleanly with exit status
  130: the RetroArch backend kills the RetroArch it launched, the hosted
  backend stops its capture between frames; both remove a partial bundle.
  A free-running (settle) RetroArch capture is also bounded by a timeout,
  after which RetroArch is killed to release gpucapture.
- A capture that fails after writing part of a bundle removes it under
  either backend, so the next run is not refused for a directory this
  tool created.
- `--help` lists the librashader backend's value and flag heading first,
  and `--app` and `--cmd-port` show their defaults the way every other
  flag does.
- The binary only parses arguments, builds a `Request`, and picks a
  backend. Behaviour is otherwise unchanged.
- Breaking library API: the source tree is grouped by backend. `app`,
  `capture`, `launch`, `remote`, and `image` are now `retroarch::app` and
  so on; `render` and `libretro` are `hosted::render` and
  `hosted::libretro`. `capture::prepare_output` and
  `capture::is_gputrace_bundle` moved to `bundle`; `layout` is no longer
  behind the `librashader` feature. `config::AppendConfig`,
  `PausedConfig`, and `is_config_safe` became `retroarch::runconfig`'s
  `RunConfig` (whose `render` returns a `Result`), `PausedConfig` (which
  carries a `StateDirs`), and `is_config_safe`; `LaunchPlan.appendconfig`
  is `config`, and `LaunchPlan.shader` and `Request.shader` are plain
  `PathBuf`s. `DirResolver` gained `locate_layout`, `core`, and
  `system_dir`, which both backends use. `config::read_keys` is gone;
  `read_all` replaced its every use. `libretro::Core::load_game` takes the
  `SystemInfo` the caller already fetched. `render::FrameSource::next`
  lends a slice instead of returning a fresh `Vec` per frame;
  `libretro::refuse_zip` is the one zip check. `AvInfo` lost its unused
  `max` and `aspect_ratio`.

#### RetroArch backend

- RetroArch is launched on a `retroarch.cfg` written for the run (`-c`)
  instead of an appendconfig layered over the user's. The file holds only
  what the run needs (the Vulkan driver, shaders on, the window, the
  system directory, a per-run save directory, a per-run core options
  file, the state slot when there is one); every other key takes
  RetroArch's compiled default. Nothing in the user's config reaches the
  run, so a RetroArch whose `video_driver` is not `vulkan` needs no
  change, and per-core option files, config overrides, remaps, auto shader
  presets, and the content history are all out of the picture.
- RetroArch is launched with `--sram-mode noload-nosave` and a per-run
  save directory, so it never reads or writes `.srm`/`.rtc` files.
  Earlier versions let RetroArch flush SRAM into the user's saves on
  exit, which after a state load rewrote the game's `.srm` with the
  state's SRAM.
- The run config turns off RetroArch's first-run asset-bundle extraction.
  On a config that has never recorded one, RetroArch extracts its bundled
  assets at startup and, a few seconds in, reinitialises every driver and
  saves the config; under a paused capture that tore down the Vulkan
  device and failed the run.
- Image mode needs no config file: `retroarch.cfg` is read only when a
  bare core name is not in the default cores directory.

#### Hosted backend

- A missing `--slot`, a ROM of the wrong kind, and an unreadable state are
  refused before the core boots, so the error is not buried in the core's
  own output.
- `--image` is validated against the formats the `image` crate decodes
  rather than RetroArch's image-viewer list.
- An output size above Metal's 16384-pixel limit is an error naming the
  limit instead of an assertion failure inside Metal.
- The default fill size uses the whole visible display area; only the
  RetroArch window subtracts the title bar.
- An rzip state container with a version other than 1 is refused by
  version rather than parsed as version 1.
- A core option whose value contains a NUL byte is reported on stderr
  when dropped.
- A core that sets `need_fullpath` is handed the ROM path with no buffer,
  as RetroArch does.
- A library caller passing `advance: 0` gets no warm-up frames instead of
  an integer underflow.

## 0.4.0 - 2026-09-11

### Added

- `--backend librashader --core CORE --rom ROM [--state FILE | --slot N]`:
  host a software-rendered libretro core in this process, restore a
  RetroArch save state, and record emulated frames through the shader
  preset with no RetroArch process. `--frames N` records N consecutive
  emulated frames; `--advance` and `--settle` keep their meanings.
- `libretro` module (feature `librashader`): `Core`, `Context`, `AvInfo`,
  `Frame`.
- `layout` module (feature `librashader`): `RetroArchDirs`, `DirResolver`,
  `Located`, `describe_tried`, `settle_frames`, the macOS-layout resolution
  the binary uses, now reachable from the library.
- `config::Size` implements `FromStr` (`WxH`); `config::is_config_safe`.
- `render::FrameSource` and `render::ImageSource` (`from_image` for an image
  already in memory, `open` for a file).
- `--core-options FILE`: a RetroArch-format options file for a hosted
  core. Without it the core runs on its built-in defaults; RetroArch's
  per-core `.opt` file is never read implicitly.
- `state::decode`, `state::slot_path`, `state::StateDirs`; `config::read_all`.

### Changed

- `fixtures/sample.png` (moved from the repo root) is a new scene, generated by `scripts/gen-sample.py` within
  Game Boy Color tile, palette, and sprite limits, and is now shipped in
  the crate so its tests run from the published package.

- Breaking library API: `render::RenderOptions.image` is replaced by
  `source: Box<dyn FrameSource>` and the struct gains `warmup: u32`;
  `render::run` takes `RenderOptions` by value.
- Breaking: `librashader` is the default backend whenever the feature is
  compiled in. `--core --rom` without `--backend` now hosts the core in
  this process, and `--shader` is required by default; pass `--backend
  retroarch` for the RetroArch capture. A `--no-default-features` build
  still defaults to `retroarch`.
- `--backend librashader` no longer requires `--image`.
- Flags that only one backend honours are now rejected under the other,
  naming the flag and the backend, instead of being ignored: `--app`,
  `--cmd-port`, and `--keep-running` under librashader; `--core-options`
  and `--skip-extension-check` under retroarch.
- A hosted core's ROM must carry an extension the core declares
  (`valid_extensions`); a mismatch is an error naming what the core
  accepts. `--skip-extension-check` loads it anyway.
- When a core is hosted, this process's stdout is pointed at stderr for
  the rest of the run and the output path is written through the original
  descriptor, so a core that prints to stdout cannot corrupt the path the
  tool reports; `$(ra-metal-capture ...)` is one line again.
- `--help` describes every flag for both backends; the crate-level docs
  lead with the in-process backend and say what docs.rs omits.
- The librashader backend resolves a bare core name, `--slot`, and the
  system directory against RetroArch's macOS default layout first and
  parses `retroarch.cfg` only when a default location is missing, so it
  works on a machine without RetroArch.
- New optional dependencies behind the `librashader` feature:
  `libretro-sys`, `libloading`, `flate2`; `deny.toml` allows ISC.

## 0.3.0 - 2026-09-10

### Added

- `--backend librashader`: render `--image` through the shader preset
  inside this process with the librashader crate's Metal runtime and write
  the `.gputrace` with Metal's `MTLCaptureManager`. RetroArch is not
  launched. Requires `--shader`; `--frames N` records N successive frames
  with the frame count advancing. Behind the `librashader` Cargo feature,
  on by default; `--no-default-features` gives the RetroArch-only build.
- `display::Screen` and `display::main_screen()`: the main display's full
  frame and backing scale alongside the visible frame.
- `render` module (feature `librashader`): `RenderOptions`, `run`,
  `output_size`, `Trace`, `CAPTURE_ENV`.

### Changed

- `capture::prepare_output` and `capture::is_gputrace_bundle` are now
  public.
- Dependency policy: `deny.toml` allows MPL-2.0 and BSD-3-Clause for the
  librashader tree and warns rather than fails on duplicate crate versions.
- docs.rs documents the crate without default features, since the
  librashader tree's C++ does not cross-build there.

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
