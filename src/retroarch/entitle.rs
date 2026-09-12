//! Re-signing a RetroArch.app so `gpucapture` can attach to it.
//!
//! `gpucapture` only captures a debuggable process, which on macOS means
//! one whose binary carries the `com.apple.security.get-task-allow`
//! entitlement. libretro's RetroArch.app builds do not, so [`run`] signs
//! the app again, ad hoc, with a plist that grants exactly that. Nothing
//! is signed that already has it.

use crate::retroarch::app;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// What [`run`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The app already had the entitlement; nothing was touched.
    AlreadyEntitled,
    /// The app was re-signed and now has it.
    Signed,
    /// The confirmation was declined; nothing was touched.
    Declined,
}

/// Whether a prompt answer means yes: an empty line (Enter) or a `y`/`yes`,
/// case-insensitively. Anything else is no.
pub fn answer_is_yes(line: &str) -> bool {
    let a = line.trim();
    a.is_empty() || a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes")
}

/// The interactive confirmation: a `[Y/n]` question on stderr, one line
/// from stdin. A closed stdin (no line at all) is a no, so a script that
/// did not pass `--yes` never signs by accident.
pub fn prompt_on_stdin(app: &Path) -> Result<bool> {
    eprint!(
        "About to re-sign {} ad hoc with {ENTITLEMENT}, replacing its current signature \
         (and any notarization). Continue? [Y/n] ",
        app.display()
    );
    std::io::stderr().flush().context("flushing stderr")?;
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the answer")?;
    Ok(read > 0 && answer_is_yes(&line))
}

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
/// [`ENTITLEMENT`], unless it already has it, asking `confirm` first
/// (see [`prompt_on_stdin`]; `--yes` passes one that says so). The
/// signature libretro shipped is replaced, so this must be repeated
/// after a RetroArch update.
pub fn run(app: &Path, confirm: &mut dyn FnMut() -> Result<bool>) -> Result<Outcome> {
    app::resolve_binary(app)?;
    if has_get_task_allow(&entitlements(app)?) {
        eprintln!("{} already has {ENTITLEMENT}; nothing to do", app.display());
        return Ok(Outcome::AlreadyEntitled);
    }
    if !confirm()? {
        eprintln!("left {} as it is", app.display());
        return Ok(Outcome::Declined);
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
    Ok(Outcome::Signed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_y_answers_are_yes_everything_else_is_no() {
        for yes in ["", "\n", "y", "Y", "yes", " YES \n"] {
            assert!(answer_is_yes(yes), "{yes:?}");
        }
        for no in ["n", "N", "no", "nope", "x", "y n"] {
            assert!(!answer_is_yes(no), "{no:?}");
        }
    }

    #[test]
    fn a_declined_prompt_signs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("RetroArch.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let bin = app.join("Contents/MacOS/RetroArch");
        std::fs::write(&bin, b"not a real binary").unwrap();
        let mut asked = 0;
        let outcome = run(&app, &mut || {
            asked += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(outcome, Outcome::Declined);
        assert_eq!(asked, 1);
        assert_eq!(std::fs::read(&bin).unwrap(), b"not a real binary");
        assert!(!app.join("Contents/_CodeSignature").exists());
    }

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
