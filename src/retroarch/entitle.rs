//! Re-signing a RetroArch.app so `gpucapture` can attach to it.
//!
//! `gpucapture` only captures a debuggable process, which on macOS means
//! one whose binary carries the `com.apple.security.get-task-allow`
//! entitlement. libretro's RetroArch.app builds do not, so [`run`] signs
//! the app again, ad hoc, with a plist that grants exactly that. Nothing
//! is signed that already has it.

use crate::retroarch::app;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

/// The entitlement `gpucapture` requires of a process it attaches to.
pub const ENTITLEMENT: &str = "com.apple.security.get-task-allow";

/// The entitlements file handed to `codesign`: the one key, and nothing else.
const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.get-task-allow</key>
    <true/>
</dict>
</plist>
"#;

/// Whether a `codesign -d --entitlements -` dump grants [`ENTITLEMENT`]:
/// the key is present and the value that follows it is `true`.
pub fn has_get_task_allow(dump: &str) -> bool {
    let mut lines = dump.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line == format!("[Key] {ENTITLEMENT}") {
            return lines.find(|l| l.starts_with("[Bool]")) == Some("[Bool] true");
        }
    }
    false
}

/// The current entitlements of `app` as `codesign` prints them. An
/// unsigned binary makes `codesign -d` fail, which reads as no
/// entitlements, so the status is not checked; a missing `codesign` is.
fn entitlements(app: &Path) -> Result<String> {
    let out = Command::new("codesign")
        .args(["-d", "--entitlements", "-"])
        .arg(app)
        .stdin(Stdio::null())
        .output()
        .context("running `codesign`; is Xcode installed?")?;
    let mut dump = String::from_utf8_lossy(&out.stdout).into_owned();
    dump.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(dump)
}

/// Sign `app` (a `.app` bundle or the binary inside it) ad hoc with
/// [`ENTITLEMENT`], unless it already has it. The signature libretro
/// shipped is replaced, so this must be repeated after a RetroArch update.
pub fn run(app: &Path) -> Result<()> {
    app::resolve_binary(app)?;
    if has_get_task_allow(&entitlements(app)?) {
        eprintln!("{} already has {ENTITLEMENT}; nothing to do", app.display());
        return Ok(());
    }
    let plist = tempfile::Builder::new()
        .prefix("ra-metal-capture-")
        .suffix(".entitlements")
        .tempfile()
        .context("creating the entitlements file")?;
    std::fs::write(plist.path(), PLIST)
        .with_context(|| format!("writing {}", plist.path().display()))?;
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-", "--entitlements"])
        .arg(plist.path())
        .arg(app)
        .stdin(Stdio::null())
        .status()
        .context("running `codesign`; is Xcode installed?")?;
    if !status.success() {
        bail!("`codesign` failed with {status} signing {}", app.display());
    }
    if !has_get_task_allow(&entitlements(app)?) {
        bail!(
            "`codesign` succeeded but {} still lacks {ENTITLEMENT}",
            app.display()
        );
    }
    eprintln!(
        "re-signed {} ad hoc with {ENTITLEMENT}; a running RetroArch needs a relaunch, \
         and a RetroArch update needs this again",
        app.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTITLED: &str = "Executable=/Applications/RetroArch.app/Contents/MacOS/RetroArch\n\
        [Dict]\n\t[Key] com.apple.security.get-task-allow\n\t[Value]\n\t\t[Bool] true\n";

    #[test]
    fn entitled_dump_is_recognised() {
        assert!(has_get_task_allow(ENTITLED));
    }

    #[test]
    fn dump_without_the_key_is_not() {
        let dump = "Executable=/Applications/RetroArch.app/Contents/MacOS/RetroArch\n\
            [Dict]\n\t[Key] com.apple.security.cs.allow-jit\n\t[Value]\n\t\t[Bool] true\n";
        assert!(!has_get_task_allow(dump));
        assert!(!has_get_task_allow(""));
    }

    #[test]
    fn key_set_to_false_is_not() {
        let dump =
            "[Dict]\n\t[Key] com.apple.security.get-task-allow\n\t[Value]\n\t\t[Bool] false\n";
        assert!(!has_get_task_allow(dump));
    }
}
