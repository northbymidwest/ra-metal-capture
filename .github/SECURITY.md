# Security policy

## Supported versions

This tool is in a `0.x` series and every release is a pre-release. There is
no long-term support branch: a fix ships in a new release cut from `main`, and
older versions are not patched. If you hit a security issue, expect the fix in
the next release rather than a backport.

## What is in scope

This tool launches a RetroArch binary with arguments and an appendconfig it
generates, drives it over RetroArch's UDP command interface on localhost,
and runs Xcode's `gpucapture(1)` against that process, so the interesting
failures are on that path:

- A command line or appendconfig built from tool arguments that does
  something other than what the flags say: a value that escapes into a
  different RetroArch option, or a write outside the per-run temp dir
  (`retroarch.cfg`, the savestate directory, and the ROM are meant to be
  read only).
- A RetroArch process left running after the tool exits, on any path,
  including failure paths.
- A panic or memory-safety issue reachable from command-line arguments, a
  `retroarch.cfg`, a `gpucapture` reply, or a UDP reply on the command port.
- The release and publishing path: the publishing workflow, its
  trusted-publishing configuration, or an archive / tag that does not match the
  source it claims to build from.

## What is not in scope

- Anything that requires a modified or hostile RetroArch, core, or
  `gpucapture`, or a modified or hostile OS. The trust boundary is a stock
  RetroArch.app and Apple's shipping Xcode tools on a stock system.
- Behavior on unsupported macOS or Xcode (older than 27), where the tool is
  untested by design.
- Another local process on the RetroArch command port. The port is opened by
  RetroArch for the duration of the run, on the loopback interface, and
  anything that can reach it already runs as you.
- Anything that can only be reproduced with a ROM or save state you cannot
  share. Without a repro there is nothing to fix; see below for what to send.

## Reporting a vulnerability

Please report privately through GitHub's private vulnerability reporting: open
the repository's **Security** tab and choose **Report a vulnerability**. Do not
open a public issue for a suspected vulnerability.

To let a fix happen quickly, include:

- the smallest repro you can manage;
- `rustc -Vv`;
- `sw_vers -productVersion`;
- the RetroArch build (`RetroArch --version`) and the tool version (or git
  SHA);
- the tool's `-v` output, which prints the appendconfig and the exact
  command line it ran.

This is a one-person project. Replies are best-effort and usually land within a
few days.
