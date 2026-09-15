//! A libretro core whose content files are images: Radiance `.hdr` as
//! HDR content, PNG or JPEG as SDR content. At load it asks the frontend
//! for `HDR10_2101010`, then `XRGB2101010`, then `XRGB8888`, encodes the
//! image for whichever was accepted with the main crate's `hdr` module,
//! and delivers that frame every `retro_run`. It exists for the paths
//! image mode cannot reach: the pixel-format gate and HDR queries
//! answered to a real dylib, the video-refresh row copy at a real pitch,
//! and the RetroArch backend end to end. Both backends load it, and so
//! does RetroArch.
//!
//! `content` is pure Rust; `abi` holds the `retro_*` exports and is the
//! only unsafe code.
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod abi;
pub mod content;
