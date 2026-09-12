#!/usr/bin/env bash
# Regenerates src/hosted/libretro/sys.rs from third_party/libretro/libretro.h.
# The generator is the tools/regen-libretro-bindings crate (bindgen as a
# library, with doxygen-bindgen turning the header's Doxygen comments into
# rustdoc); it is not part of the main build, and its output is what the
# crate compiles. bindgen needs libclang, which Xcode provides.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo run --quiet --release \
  --manifest-path tools/regen-libretro-bindings/Cargo.toml \
  --target-dir target/regen-libretro-bindings -- \
  third_party/libretro/libretro.h src/hosted/libretro/sys.rs
rustfmt --edition 2024 src/hosted/libretro/sys.rs
