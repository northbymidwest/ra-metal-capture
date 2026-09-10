use crate::launch::LaunchCommand;
use crate::remote::Remote;
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// PIDs listed by `gpucapture list`. The first line is a header; each
/// following line starts with the PID.
pub fn parse_capturable_pids(list_output: &str) -> Vec<u32> {
    list_output
        .lines()
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Kills the child on drop unless disarmed. Every early return from `run`
/// goes through this, so a launched RetroArch is never leaked.
struct ChildGuard {
    child: Child,
    armed: bool,
}

impl ChildGuard {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Some(status) if the child has exited.
    fn poll(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().context("polling RetroArch")
    }

    /// SIGTERM, wait up to `grace`, then SIGKILL via drop.
    fn terminate(&mut self, grace: Duration) {
        let _ = kill(Pid::from_raw(self.pid() as i32), Signal::SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.armed = false;
                return;
            }
            sleep(Duration::from_millis(50));
        }
    }

    /// SIGKILL immediately, wait for it, and disarm. Used to release a
    /// `gpucapture start` that is blocked waiting on a boundary this pid
    /// will never reach again.
    fn kill_now(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// What to wait for before capturing.
#[derive(Clone, Copy)]
pub enum Trigger {
    /// Wait this long after RetroArch is capturable, then capture whatever is on screen.
    Settle(Duration),
    /// Pause, load the configured state slot, advance `advance` frames before
    /// arming the capture; the recorded frame is a fixed number of further
    /// advances after that (see `MAX_CLOSING_ADVANCES`).
    Paused { port: u16, advance: u32 },
}

pub struct CaptureOptions {
    /// What to wait for before capturing.
    pub trigger: Trigger,
    /// Number of frame boundaries to record.
    pub frames: u32,
    /// Absolute path of the `.gputrace` to write.
    pub output: PathBuf,
    /// Leave RetroArch running after the capture.
    pub keep_running: bool,
    /// How long to wait for the PID to appear in `gpucapture list`.
    pub ready_timeout: Duration,
    /// Where RetroArch's stdout and stderr are written.
    pub log_path: PathBuf,
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(20);
    lines[start..].join("\n")
}

fn gpucapture_list() -> Result<Vec<u32>> {
    let out = Command::new("gpucapture")
        .arg("list")
        .output()
        .context("running `gpucapture list`; is Xcode installed?")?;
    if !out.status.success() {
        bail!(
            "`gpucapture list` failed with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_capturable_pids(&String::from_utf8_lossy(&out.stdout)))
}

fn wait_capturable(guard: &mut ChildGuard, timeout: Duration, log_path: &Path) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let pid = guard.pid();
    loop {
        if let Some(status) = guard.poll()? {
            bail!(
                "RetroArch exited ({status}) before becoming capturable. Last log lines:\n{}",
                log_tail(log_path)
            );
        }
        if gpucapture_list()?.contains(&pid) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "pid {pid} never appeared in `gpucapture list` within {timeout:?}. \
                 Is MTL_CAPTURE_ENABLED honoured by this RetroArch build? Last log lines:\n{}",
                log_tail(log_path)
            );
        }
        sleep(Duration::from_millis(100));
    }
}

/// True when `path` looks like a bundle this tool (or Xcode) wrote: a
/// directory whose name ends in `.gputrace` and which contains an `index`.
fn is_gputrace_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "gputrace")
        && path.is_dir()
        && path.join("index").exists()
}

/// Make room for a new capture at `output`. A previous bundle is removed;
/// anything else that exists there is refused, so a mistyped path never
/// deletes user data.
fn prepare_output(output: &Path) -> Result<()> {
    if !output.exists() {
        return Ok(());
    }
    if !is_gputrace_bundle(output) {
        bail!(
            "{} exists and is not a .gputrace bundle; refusing to overwrite it",
            output.display()
        );
    }
    std::fs::remove_dir_all(output)
        .with_context(|| format!("removing stale bundle {}", output.display()))
}

/// Run `gpucapture start`, calling `on_armed` once it reports it is waiting
/// for a boundary. gpucapture flushes that line even into a pipe (measured).
fn gpucapture_start(pid: u32, frames: u32, output: &Path, on_armed: impl FnOnce()) -> Result<()> {
    let mut child = Command::new("gpucapture")
        .args([
            "start",
            "--pid",
            &pid.to_string(),
            "--count",
            &frames.to_string(),
        ])
        .arg("--output")
        .arg(output)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("running `gpucapture start`")?;
    // Every early return below kills and waits the child first so a failed
    // read never leaves a gpucapture zombie behind.
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("gpucapture stdout was not piped");
        }
    };
    let mut on_armed = Some(on_armed);
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).context("reading gpucapture output");
            }
        };
        if line.starts_with("triggering capture")
            && let Some(f) = on_armed.take()
        {
            f();
        }
    }
    let status = child.wait().context("waiting for gpucapture")?;
    if !status.success() {
        bail!("`gpucapture start` failed with {status}");
    }
    if !output.join("index").exists() {
        bail!(
            "`gpucapture start` succeeded but {} has no `index` entry; the bundle looks incomplete",
            output.display()
        );
    }
    Ok(())
}

fn bail_if_exited(guard: &mut ChildGuard, when: &str, log_path: &Path) -> Result<()> {
    if let Some(status) = guard.poll()? {
        bail!(
            "RetroArch exited ({status}) {when}. Last log lines:\n{}",
            log_tail(log_path)
        );
    }
    Ok(())
}

/// How long to wait for `gpucapture start` to report it is armed and
/// waiting for a boundary, once it has been launched.
const ARM_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-frame allowance of advances sent while waiting for an armed capture
/// to close, added to `frames` to form the actual cap. Measured: a paused
/// RetroArch needs three advances after arming to close one frame boundary
/// (one never opens the capture, two open it without a drawable, three
/// complete it); this constant is that measurement with headroom, since it
/// may depend on swapchain depth. The tool advances until the capture
/// finishes rather than hard-coding the count, with `frames +
/// MAX_CLOSING_ADVANCES` as a backstop against a capture that never closes.
const MAX_CLOSING_ADVANCES: u32 = 6;

/// How long to wait, once the closing-advance cap is reached, for gpucapture
/// to finish writing the bundle before concluding the capture is stalled.
/// gpucapture writes the bundle (tens of MB) to disk after the last frame
/// boundary, and `is_finished()` only turns true once that write completes
/// and the `index` check in `gpucapture_start` runs, so reaching the cap
/// does not by itself mean the capture stalled.
const BUNDLE_WRITE_GRACE: Duration = Duration::from_secs(15);

/// Pause, load the state, advance `advance` times, then advance until the
/// armed capture closes.
fn capture_paused(
    guard: &mut ChildGuard,
    port: u16,
    advance: u32,
    frames: u32,
    output: &Path,
    ready_timeout: Duration,
    log_path: &Path,
) -> Result<Remote> {
    let pid = guard.pid();
    let remote = Remote::connect(port)?;
    remote.wait_playing(ready_timeout)?;
    bail_if_exited(guard, "before it could be paused", log_path)?;
    remote.pause()?;
    remote.load_state()?;

    for _ in 0..advance {
        remote.frame_advance()?;
    }
    eprintln!("advanced {advance} frame(s) from the loaded state; arming capture");

    let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
    let output_owned = output.to_path_buf();
    let capture = std::thread::spawn(move || {
        gpucapture_start(pid, frames, &output_owned, move || {
            let _ = armed_tx.send(());
        })
    });
    // A paused RetroArch never releases gpucapture on its own, so both
    // failure sub-cases below kill it first: on `Timeout` to stop it
    // waiting for a boundary that will never come, and on `Disconnected`
    // as cleanup, since the thread has already finished by then.
    match armed_rx.recv_timeout(ARM_TIMEOUT) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            guard.kill_now();
            let _ = capture.join();
            bail!("gpucapture did not report it was armed within {ARM_TIMEOUT:?}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The sender was dropped because gpucapture_start returned
            // early (missing binary, failed spawn, a read error); join and
            // propagate its actual error instead of reporting a timeout
            // that did not happen.
            guard.kill_now();
            match capture.join() {
                Ok(Ok(())) => {
                    bail!("gpucapture exited before reporting it was armed")
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => bail!("the gpucapture thread panicked"),
            }
        }
    }

    // A paused RetroArch presents only on frame advances, and gpucapture
    // needs more than one present to open and close a frame (measured: 3
    // per boundary). Advance until the capture thread finishes, with a cap
    // that scales with the number of boundaries gpucapture is waiting for.
    let max_closing = frames + MAX_CLOSING_ADVANCES;
    let mut closing = 0;
    while !capture.is_finished() {
        if closing == max_closing {
            // The cap may have been reached while gpucapture is still
            // flushing the bundle to disk; give it a grace period before
            // concluding the capture stalled.
            let deadline = Instant::now() + BUNDLE_WRITE_GRACE;
            while !capture.is_finished() && Instant::now() < deadline {
                sleep(Duration::from_millis(100));
            }
            if !capture.is_finished() {
                guard.kill_now();
                let _ = capture.join();
                bail!(
                    "capture did not complete after {max_closing} frame advances \
                     and a {BUNDLE_WRITE_GRACE:?} grace period; RetroArch was \
                     killed to release gpucapture"
                );
            }
            break;
        }
        if let Err(e) = remote.frame_advance() {
            guard.kill_now();
            let _ = capture.join();
            return Err(e.context("advancing a frame to close the capture; RetroArch was killed to release gpucapture"));
        }
        closing += 1;
    }
    match capture.join() {
        Ok(result) => result?,
        Err(_) => bail!("the gpucapture thread panicked"),
    }
    eprintln!("capture closed after {closing} further advance(s)");
    Ok(remote)
}

/// Launch RetroArch, wait until it is capturable, run the trigger, capture, terminate.
pub fn run(cmd: &LaunchCommand, opts: &CaptureOptions) -> Result<()> {
    prepare_output(&opts.output)?;
    let log = File::create(&opts.log_path)
        .with_context(|| format!("creating {}", opts.log_path.display()))?;
    let log_err = log.try_clone().context("cloning log handle")?;
    let child = cmd
        .to_command()
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("launching {}", cmd.program.display()))?;
    let mut guard = ChildGuard { child, armed: true };
    let pid = guard.pid();
    eprintln!("launched RetroArch as pid {pid}");

    wait_capturable(&mut guard, opts.ready_timeout, &opts.log_path)?;

    let remote = match opts.trigger {
        Trigger::Settle(settle) => {
            eprintln!("pid {pid} is capturable; settling for {settle:?}");
            sleep(settle);
            bail_if_exited(&mut guard, "during settle", &opts.log_path)?;
            gpucapture_start(pid, opts.frames, &opts.output, || {})?;
            None
        }
        Trigger::Paused { port, advance } => Some(capture_paused(
            &mut guard,
            port,
            advance,
            opts.frames,
            &opts.output,
            opts.ready_timeout,
            &opts.log_path,
        )?),
    };

    if opts.keep_running {
        guard.armed = false;
        return Ok(());
    }
    if let Some(remote) = remote {
        // Ask nicely first; the guard's SIGTERM/SIGKILL remains the fallback.
        let _ = remote.quit();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(guard.poll(), Ok(Some(_))) {
                guard.armed = false;
                return Ok(());
            }
            sleep(Duration::from_millis(50));
        }
    }
    guard.terminate(Duration::from_secs(3));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pids_from_first_column() {
        let out = "    PID  Device  Name  GPU(ms)  \n  12084       0  loop     0.02  \n  99  0  RetroArch  1.0\n";
        assert_eq!(parse_capturable_pids(out), vec![12084, 99]);
    }

    #[test]
    fn empty_and_header_only_give_nothing() {
        assert!(parse_capturable_pids("").is_empty());
        assert!(parse_capturable_pids("    PID  Device  Name  GPU(ms)  \n").is_empty());
    }

    #[test]
    fn recognises_a_bundle_only_with_suffix_and_index() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("a.gputrace");
        std::fs::create_dir_all(bundle.join("index")).unwrap();
        assert!(is_gputrace_bundle(&bundle));

        let no_index = tmp.path().join("b.gputrace");
        std::fs::create_dir_all(&no_index).unwrap();
        assert!(!is_gputrace_bundle(&no_index));

        let wrong_suffix = tmp.path().join("c");
        std::fs::create_dir_all(wrong_suffix.join("index")).unwrap();
        assert!(!is_gputrace_bundle(&wrong_suffix));

        assert!(!is_gputrace_bundle(&tmp.path().join("missing.gputrace")));
    }

    #[test]
    fn prepare_output_refuses_a_directory_that_is_not_a_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("docs");
        std::fs::create_dir_all(&dir).unwrap();
        let err = prepare_output(&dir).unwrap_err().to_string();
        assert!(err.contains("docs"), "{err}");
        assert!(dir.exists(), "must not delete a non-bundle directory");
    }

    #[test]
    fn prepare_output_removes_a_stale_bundle_and_tolerates_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("old.gputrace");
        std::fs::create_dir_all(bundle.join("index")).unwrap();
        prepare_output(&bundle).unwrap();
        assert!(!bundle.exists());
        prepare_output(&tmp.path().join("new.gputrace")).unwrap();
    }
}
