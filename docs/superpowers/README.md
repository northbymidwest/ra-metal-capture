# Design records

`specs/` holds the design documents, one per feature, each written before
its implementation and amended afterwards with a dated note at the top
where the shipped code moved on. They are the record of why things are the
way they are; the README and the code are the record of what they are.

`plans/` holds the implementation plans those specs were executed from.
They are historical: every task in them has shipped, and their checkboxes
were never ticked because execution was tracked elsewhere. Read them for
the reasoning behind a commit, not as pending work.

Source paths in the specs and plans predate the 2026-09-11 reorganisation
of `src/` by backend: `capture.rs`, `launch.rs`, `remote.rs`, `app.rs`,
and `image.rs` now live under `src/retroarch/`, and `render/` and
`libretro/` under `src/hosted/`.
