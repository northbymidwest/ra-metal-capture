//! ra-metal-capture: record a Metal frame trace (`.gputrace`) of a shader
//! preset, rendered in this process through librashader or captured from
//! a running RetroArch. The binary of the same name is the intended
//! interface; this library is its building blocks, so that each piece can
//! be tested on its own and reused.
//!
//! The binary turns its arguments into a [`backend::Request`] and hands it
//! to a [`backend::Backend`]: [`retroarch::RetroArch`] launches
//! RetroArch.app, and `hosted::Hosted` (behind the `librashader` feature)
//! renders in this process.
//!
//! With the `librashader` feature (on by default) the tool renders inside
//! its own process: a static image or a hosted libretro core
//! (`hosted::libretro`) is fed through the preset by librashader's Metal
//! runtime (`hosted::render`),
//! and the trace is written with Metal's capture API. `state` decodes
//! RetroArch save states for that path. No RetroArch process is involved.
//!
//! The RetroArch backend, selected with `--backend retroarch`, assembles a
//! [`retroarch::launch::LaunchPlan`] and an
//! [`retroarch::runconfig::RunConfig`], turns them into a
//! [`retroarch::launch::LaunchCommand`], and hands that to
//! [`retroarch::capture::run`] with a [`retroarch::capture::CaptureOptions`].
//! Paused capture drives RetroArch over its UDP command interface through
//! [`retroarch::remote::Remote`].
//!
//! Requires macOS 27 and Xcode 27 at run time; nothing here touches Metal,
//! RetroArch, or `gpucapture` until a run starts.
//!
//! docs.rs builds this crate without default features, because the
//! librashader dependency tree's C++ does not cross-build there, so the
//! `hosted` module is absent from the published
//! documentation. The README describes them.
#![deny(unsafe_code)]

pub mod backend;
pub mod bundle;
pub mod config;
pub mod core;
pub mod display;
#[cfg(feature = "librashader")]
pub mod hosted;
pub mod layout;
pub mod retroarch;
pub mod state;
