# libretro.h

`libretro.h` is the libretro API header, copied verbatim from RetroArch at
commit [`65e1840c8a8f`](https://github.com/libretro/RetroArch/blob/65e1840c8a8f24d395ff3094026c4323f60e2ffe/libretro-common/include/libretro.h)
(`libretro-common/include/libretro.h`; the full hash is in `COMMIT`). Its license is in the
comment at the top of the file and applies to that file only.

`src/hosted/libretro/sys.rs` is generated from it. `cargo xtask libretro update` vendors the
header at the newest commit that changed it and regenerates; `cargo xtask
libretro regen` regenerates from what is here. This file is written by
`update`.
