use crate::launch::LaunchCommand;
use crate::remote::Remote;
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::fs::File;
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
    /// Pause, load the configured state slot, advance `advance` frames; the last advance is captured.
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

/// How long `gpucapture start` needs to arm before the boundary it waits
/// for. Measured: it prints "triggering capture" well under 500 ms.
const ARM_DELAY: Duration = Duration::from_millis(500);

fn gpucapture_start(pid: u32, frames: u32, output: &Path) -> Result<()> {
    let status = Command::new("gpucapture")
        .args(["start", "--pid", &pid.to_string(), "--count", &frames.to_string()])
        .arg("--output")
        .arg(output)
        .status()
        .context("running `gpucapture start`")?;
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

/// Pause, load the state, advance, and capture the final advance.
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
    for _ in 1..advance {
        remote.frame_advance()?;
    }
    eprintln!("paused on the loaded state; arming capture, then advancing frame {advance}");

    let output_owned = output.to_path_buf();
    let capture = std::thread::spawn(move || gpucapture_start(pid, frames, &output_owned));
    sleep(ARM_DELAY);
    remote.frame_advance()?;
    match capture.join() {
        Ok(result) => result?,
        Err(_) => bail!("the gpucapture thread panicked"),
    }
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
            gpucapture_start(pid, opts.frames, &opts.output)?;
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
