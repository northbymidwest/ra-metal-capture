//! ra-metal-capture: capture a Metal frame trace (`.gputrace`) from
//! RetroArch. The binary of the same name is the intended interface; this
//! library is its building blocks, so that each piece can be tested on its
//! own and reused.
//!
//! A run assembles a [`launch::LaunchPlan`] and an [`config::AppendConfig`],
//! turns them into a [`launch::LaunchCommand`], and hands that to
//! [`capture::run`] with a [`capture::CaptureOptions`]. Paused capture drives
//! RetroArch over its UDP command interface through [`remote::Remote`].
//!
//! Requires macOS 27 and Xcode 27 at run time; nothing here touches
//! RetroArch or `gpucapture` until [`capture::run`] is called.
#![deny(unsafe_code)]

pub mod app;
pub mod capture;
pub mod config;
pub mod core;
pub mod display;
pub mod image;
pub mod launch;
pub mod remote;
pub mod state;
