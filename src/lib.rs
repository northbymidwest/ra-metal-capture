//! ra-metal-capture: record a Metal frame trace (`.gputrace`) of a
//! RetroArch shader preset. The binary of the same name is the intended
//! interface; this library is its building blocks, so that each piece can
//! be tested on its own and reused.
//!
//! With the `librashader` feature (on by default) the tool renders inside
//! its own process: a static image or a hosted libretro core (`libretro`)
//! is fed through the preset by librashader's Metal runtime (`render`),
//! and the trace is written with Metal's capture API. `state` decodes
//! RetroArch save states for that path. No RetroArch process is involved.
//!
//! The RetroArch backend, selected with `--backend retroarch`, assembles a
//! [`launch::LaunchPlan`] and an [`config::AppendConfig`], turns them into
//! a [`launch::LaunchCommand`], and hands that to [`capture::run`] with a
//! [`capture::CaptureOptions`]. Paused capture drives RetroArch over its
//! UDP command interface through [`remote::Remote`].
//!
//! Requires macOS 27 and Xcode 27 at run time; nothing here touches Metal,
//! RetroArch, or `gpucapture` until a run starts.
//!
//! docs.rs builds this crate without default features, because the
//! librashader dependency tree's C++ does not cross-build there, so the
//! `render` and `libretro` modules are absent from the published
//! documentation. The README describes them.
#![deny(unsafe_code)]

pub mod app;
pub mod capture;
pub mod config;
pub mod core;
pub mod display;
pub mod image;
pub mod launch;
#[cfg(feature = "librashader")]
pub mod layout;
#[cfg(feature = "librashader")]
pub mod libretro;
pub mod remote;
#[cfg(feature = "librashader")]
pub mod render;
pub mod state;
