# libretro.h

`libretro.h` is the libretro API header, copied verbatim from RetroArch at
commit [`69a4f0ea1e8a`](https://github.com/libretro/RetroArch/blob/69a4f0ea1e8aaf442ae4858f2e7f2b31a1776576/libretro-common/include/libretro.h)
(`libretro-common/include/libretro.h`; the full hash is in `COMMIT`, which
the generator reads). Its license is in the comment at the top of the file
and applies to that file only.

`src/hosted/libretro/sys.rs` is generated from it by
`scripts/regen-libretro-bindings.sh`; after replacing the header, update
`COMMIT` and this link, then regenerate.
