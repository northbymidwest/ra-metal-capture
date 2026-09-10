# Releasing

The crate releases through `.github/workflows/release.yml`. Every release is a
`0.x` pre-release.

## Prerequisites (one-time)

- **A macOS runner.** `cargo publish` runs a verify build that links AppKit
  through `objc2-app-kit`, so the `publish` job runs on `macos-latest`.
- **A crates.io trusted-publisher entry** for `ra-metal-capture`: owner
  `northbymidwest`, repo `ra-metal-capture`, workflow `release.yml`,
  environment `release`.
- **A `release` environment** in repo settings with a required reviewer, and
  restricted to `main`.
- **A read-write deploy key for tagging.** The `protect version tags` ruleset
  admits no one but a deploy key, so the workflow pushes the `v<version>` tag
  with one: its public half is registered as a deploy key with write access,
  its private half is the `release` environment secret `RELEASE_TAG_KEY`, and
  no copy exists anywhere else.

## By hand, before dispatching

1. Bump `version` in `Cargo.toml` to the new version.
2. Remove the `publish = false` line (it is the deliberate safety net that
   keeps the crate off crates.io until you mean it).
3. Retitle the `## Unreleased` section of `CHANGELOG.md` to
   `## <version> - <YYYY-MM-DD>`.
4. Commit, push, and wait for CI to go green.

## Dispatch

Actions -> release -> Run workflow. Enter the version without a leading `v`.
Leave `dry_run` ticked for a rehearsal; untick it to publish.

- `preflight` (no approval) validates the version format, that the manifest
  carries it and is no longer `publish = false`, that a non-empty
  `CHANGELOG.md` section exists, and that the newest CI run for the commit is
  green.
- `publish` pauses at the `release` environment's reviewer gate. On approval it
  exchanges an OIDC token for a short-lived crates.io token, publishes the
  crate, then pushes the `v<version>` tag and creates a `--prerelease` GitHub
  Release from the changelog section.

`dry_run` defaults to true on purpose: forgetting to untick costs a re-run;
forgetting to tick would be an irreversible publish.
