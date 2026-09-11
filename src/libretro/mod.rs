//! Hosting a libretro core in this process: loading the dylib, answering
//! its environment queries, and turning each `retro_run` into a BGRA8
//! frame for the render module. The libretro ABI carries no user pointer,
//! so the callbacks' state lives in process-wide statics (`env`), and one
//! `Core` per process is enforced. This module and `render` are the only
//! ones that allow `unsafe`; this one is also the only place with a
//! hand-written C ABI (the six callbacks a core calls back into).
#![allow(unsafe_code)]

pub mod pixels;
