use crate::launch::LaunchCommand;
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

pub struct CaptureOptions {
    /// Time to wait after RetroArch becomes capturable before capturing.
    pub settle: Duration,
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

/// Launch RetroArch, wait until it is capturable, settle, capture, terminate.
pub fn run(cmd: &LaunchCommand, opts: &CaptureOptions) -> Result<()> {
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
    eprintln!("pid {pid} is capturable; settling for {:?}", opts.settle);
    sleep(opts.settle);
    if let Some(status) = guard.poll()? {
        bail!(
            "RetroArch exited ({status}) during settle. Last log lines:\n{}",
            log_tail(&opts.log_path)
        );
    }

    match std::fs::remove_dir_all(&opts.output) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("removing stale bundle at {}", opts.output.display()));
        }
    }

    let status = Command::new("gpucapture")
        .args(["start", "--pid", &pid.to_string(), "--count", &opts.frames.to_string()])
        .arg("--output")
        .arg(&opts.output)
        .status()
        .context("running `gpucapture start`")?;
    if !status.success() {
        bail!("`gpucapture start` failed with {status}");
    }
    let index = opts.output.join("index");
    if !index.exists() {
        bail!(
            "`gpucapture start` succeeded but {} has no `index` entry; the bundle looks incomplete",
            opts.output.display()
        );
    }

    if opts.keep_running {
        guard.armed = false;
    } else {
        guard.terminate(Duration::from_secs(3));
    }
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
}
