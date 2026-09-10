# Paused Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a save state is given, capture a frame that is a pure function of the state plus a fixed number of frame advances, so two runs with different shaders record the identical emulated frame.

**Architecture:** RetroArch's UDP command interface (enabled only in the per-run appendconfig) drives the sequence: wait for `PLAYING`, `PAUSE_TOGGLE`, `LOAD_STATE`, `FRAMEADVANCE` N minus 1 times, arm `gpucapture start` on a thread, send the final `FRAMEADVANCE` (a paused RetroArch re-presents but never yields a capturable boundary; a frame advance does), join, then `QUIT` twice. A new `remote` module owns the socket and is tested against a fake RetroArch responder on loopback. Runs without a state keep the existing settle flow.

**Tech Stack:** Rust 1.98 edition 2024, std `UdpSocket` and threads, existing deps only (clap, anyhow, tempfile, nix, objc2).

**Spec:** `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md` (this plan amends its capture flow; the spec edit is Task 4). Design was approved in chat on 2026-09-10 after a measured spike:
- `GET_STATUS` replies `GET_STATUS PLAYING <core>,<content>,crc32=<hex>` or `GET_STATUS PAUSED ...` or `GET_STATUS CONTENTLESS`.
- `PAUSE_TOGGLE`, `LOAD_STATE`, `FRAMEADVANCE`, `QUIT` produce no reply. `QUIT` must be sent twice (RetroArch's press-twice default).
- `LOAD_STATE` while paused loads the file, runs one frame, and re-pauses (`retroarch.c` near line 3750).
- `gpucapture start --count 1` never completes while paused (measured: 30 s at "0 / 1 CAMetalDrawable") but completes 0.6 s after one `FRAMEADVANCE`.

## Global Constraints

- `rust-version = "1.98"`, `edition = "2024"`, no `rust-toolchain.toml`. No new dependencies.
- No `unsafe` outside `src/display.rs`.
- Never write to the user's `retroarch.cfg` or savestate directory. The UDP port is enabled only through the temp appendconfig.
- Never leak a launched RetroArch on any failure path (the existing `ChildGuard` stays the last line of defence).
- No em dashes or en dashes anywhere. Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- `cargo test` and `cargo clippy --all-targets -- -D warnings` clean after every task.
- Branch: `paused-capture`, forked from `main` at c75632f.

---

## File structure

```
src/remote.rs      NEW  UDP client: Status, parse_status, Remote (connect, status, wait_playing, pause, load_state, frame_advance, quit)
src/config.rs      MOD  AppendConfig gains `paused: Option<PausedConfig>` rendering the three UDP/state keys
src/capture.rs     MOD  Trigger enum (Settle | Paused); gpucapture_start extracted; paused sequence; QUIT shutdown
src/launch.rs      MOD  `slot` and `-e` removed (state loading is now always via LOAD_STATE)
src/main.rs        MOD  --advance, --cmd-port; state/slot map to Trigger::Paused; --settle only for no-state runs
docs/superpowers/specs/2026-09-09-retroarch-capture-design.md  MOD  capture flow and CLI sections
README.md          MOD  flags, how it works, verified section
```

---

### Task 1: `remote` module with a fake-RetroArch test harness

**Files:**
- Create: `src/remote.rs`
- Modify: `src/main.rs` (add `mod remote;` in alphabetical order, between `launch` and `state`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Status { Playing, Paused, Contentless, Other(String) }
  pub fn parse_status(reply: &str) -> Status
  pub struct Remote { .. }
  impl Remote {
      pub fn connect(port: u16) -> Result<Remote>
      pub fn status(&self) -> Result<Status>
      pub fn wait_playing(&self, timeout: Duration) -> Result<()>
      pub fn pause(&self) -> Result<()>
      pub fn load_state(&self) -> Result<()>
      pub fn frame_advance(&self) -> Result<()>
      pub fn quit(&self) -> Result<()>
  }
  pub const COMMAND_SETTLE: Duration = Duration::from_millis(250);
  ```

- [ ] **Step 1: Add `mod remote;` to `src/main.rs`**

The module list becomes `app, capture, config, core, display, launch, remote, state`.

- [ ] **Step 2: Write `src/remote.rs` with stubs and the failing tests**

```rust
//! RetroArch's UDP command interface (`network_cmd_enable`). Commands are
//! plain ASCII datagrams; only `GET_STATUS` replies.

use anyhow::{Context, Result, bail};
use std::net::UdpSocket;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Time RetroArch is given to act on a command that produces no reply.
/// A frame is about 17 ms; LOAD_STATE runs one frame and re-pauses.
pub const COMMAND_SETTLE: Duration = Duration::from_millis(250);

const REPLY_TIMEOUT: Duration = Duration::from_secs(1);
const STATUS_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Contentless,
    Other(String),
}

/// Parse a `GET_STATUS` reply such as
/// `GET_STATUS PLAYING game_boy,Zelda,crc32=6887a34`.
pub fn parse_status(reply: &str) -> Status {
    todo!()
}

pub struct Remote {
    socket: UdpSocket,
}

impl Remote {
    /// Bind an ephemeral loopback socket and point it at RetroArch's port.
    pub fn connect(port: u16) -> Result<Remote> {
        todo!()
    }

    fn send(&self, command: &str) -> Result<()> {
        todo!()
    }

    pub fn status(&self) -> Result<Status> {
        todo!()
    }

    /// Poll until content is running.
    pub fn wait_playing(&self, timeout: Duration) -> Result<()> {
        todo!()
    }

    /// PAUSE_TOGGLE, then confirm RetroArch reports PAUSED.
    pub fn pause(&self) -> Result<()> {
        todo!()
    }

    /// LOAD_STATE; RetroArch loads the configured slot, runs one frame, and
    /// re-pauses. Waits COMMAND_SETTLE for that to happen.
    pub fn load_state(&self) -> Result<()> {
        todo!()
    }

    /// FRAMEADVANCE, then wait COMMAND_SETTLE.
    pub fn frame_advance(&self) -> Result<()> {
        todo!()
    }

    /// QUIT twice: RetroArch's default `quit_press_twice` needs a second press.
    pub fn quit(&self) -> Result<()> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A fake RetroArch: answers GET_STATUS from a scripted state, flips
    /// PLAYING to PAUSED on PAUSE_TOGGLE, records every command received.
    struct Fake {
        port: u16,
        received: Arc<Mutex<Vec<String>>>,
    }

    fn fake(initial: &'static str, reply: bool) -> Fake {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&received);
        thread::spawn(move || {
            let mut state = initial.to_string();
            let mut buf = [0u8; 1024];
            while let Ok((n, src)) = socket.recv_from(&mut buf) {
                let cmd = String::from_utf8_lossy(&buf[..n]).to_string();
                log.lock().unwrap().push(cmd.clone());
                match cmd.as_str() {
                    "GET_STATUS" if reply => {
                        let msg = format!("GET_STATUS {state} game_boy,Zelda,crc32=6887a34");
                        socket.send_to(msg.as_bytes(), src).unwrap();
                    }
                    "PAUSE_TOGGLE" => {
                        state = if state == "PAUSED" { "PLAYING".into() } else { "PAUSED".into() };
                    }
                    _ => {}
                }
            }
        });
        Fake { port, received }
    }

    fn received(f: &Fake) -> Vec<String> {
        f.received.lock().unwrap().clone()
    }

    #[test]
    fn parses_status_replies() {
        assert_eq!(parse_status("GET_STATUS PLAYING game_boy,Zelda,crc32=1"), Status::Playing);
        assert_eq!(parse_status("GET_STATUS PAUSED game_boy,Zelda,crc32=1"), Status::Paused);
        assert_eq!(parse_status("GET_STATUS CONTENTLESS"), Status::Contentless);
        assert_eq!(parse_status("GET_STATUS ERROR"), Status::Other("GET_STATUS ERROR".into()));
        assert_eq!(parse_status("garbage"), Status::Other("garbage".into()));
    }

    #[test]
    fn status_round_trips_through_udp() {
        let f = fake("PLAYING", true);
        let r = Remote::connect(f.port).unwrap();
        assert_eq!(r.status().unwrap(), Status::Playing);
    }

    #[test]
    fn status_errors_when_nothing_replies() {
        let f = fake("PLAYING", false);
        let r = Remote::connect(f.port).unwrap();
        let err = r.status().unwrap_err().to_string();
        assert!(err.contains("GET_STATUS"), "{err}");
    }

    #[test]
    fn wait_playing_returns_once_content_runs() {
        let f = fake("PLAYING", true);
        let r = Remote::connect(f.port).unwrap();
        r.wait_playing(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn wait_playing_times_out_while_contentless() {
        let f = fake("CONTENTLESS", true);
        let r = Remote::connect(f.port).unwrap();
        let err = r.wait_playing(Duration::from_millis(500)).unwrap_err().to_string();
        assert!(err.contains("CONTENTLESS"), "{err}");
    }

    #[test]
    fn pause_toggles_and_confirms() {
        let f = fake("PLAYING", true);
        let r = Remote::connect(f.port).unwrap();
        r.pause().unwrap();
        assert_eq!(r.status().unwrap(), Status::Paused);
    }

    #[test]
    fn pause_fails_if_retroarch_stays_playing() {
        // The fake flips on PAUSE_TOGGLE, so start it PAUSED: the toggle
        // makes it PLAYING and pause() must report the mismatch.
        let f = fake("PAUSED", true);
        let r = Remote::connect(f.port).unwrap();
        let err = r.pause().unwrap_err().to_string();
        assert!(err.contains("Playing"), "{err}");
    }

    #[test]
    fn sequence_sends_the_expected_datagrams() {
        let f = fake("PLAYING", true);
        let r = Remote::connect(f.port).unwrap();
        r.pause().unwrap();
        r.load_state().unwrap();
        r.frame_advance().unwrap();
        r.quit().unwrap();
        sleep(Duration::from_millis(50));
        let got = received(&f);
        let commands: Vec<&str> = got.iter().map(String::as_str).filter(|c| *c != "GET_STATUS").collect();
        assert_eq!(commands, ["PAUSE_TOGGLE", "LOAD_STATE", "FRAMEADVANCE", "QUIT", "QUIT"]);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test remote::`
Expected: 8 tests FAIL (panics at `todo!()`).

- [ ] **Step 4: Implement**

```rust
pub fn parse_status(reply: &str) -> Status {
    let mut words = reply.split_whitespace();
    match (words.next(), words.next()) {
        (Some("GET_STATUS"), Some("PLAYING")) => Status::Playing,
        (Some("GET_STATUS"), Some("PAUSED")) => Status::Paused,
        (Some("GET_STATUS"), Some("CONTENTLESS")) => Status::Contentless,
        _ => Status::Other(reply.trim().to_string()),
    }
}

impl Remote {
    pub fn connect(port: u16) -> Result<Remote> {
        let socket = UdpSocket::bind("127.0.0.1:0").context("binding a UDP socket")?;
        socket
            .connect(("127.0.0.1", port))
            .with_context(|| format!("connecting to RetroArch's command port {port}"))?;
        socket
            .set_read_timeout(Some(REPLY_TIMEOUT))
            .context("setting the UDP read timeout")?;
        Ok(Remote { socket })
    }

    fn send(&self, command: &str) -> Result<()> {
        self.socket
            .send(command.as_bytes())
            .with_context(|| format!("sending {command} to RetroArch"))?;
        Ok(())
    }

    pub fn status(&self) -> Result<Status> {
        self.send("GET_STATUS")?;
        let mut buf = [0u8; 1024];
        let n = self.socket.recv(&mut buf).context(
            "no reply to GET_STATUS; is network_cmd_enable on and the port free?",
        )?;
        Ok(parse_status(&String::from_utf8_lossy(&buf[..n])))
    }

    pub fn wait_playing(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        loop {
            match self.status() {
                Ok(Status::Playing) => return Ok(()),
                Ok(other) => last = Some(format!("{other:?}")),
                Err(e) => last = Some(e.to_string()),
            }
            if Instant::now() >= deadline {
                bail!(
                    "RetroArch did not report PLAYING within {timeout:?}; last status: {}",
                    last.unwrap_or_default()
                );
            }
            sleep(STATUS_POLL);
        }
    }

    pub fn pause(&self) -> Result<()> {
        self.send("PAUSE_TOGGLE")?;
        sleep(COMMAND_SETTLE);
        match self.status()? {
            Status::Paused => Ok(()),
            other => bail!("sent PAUSE_TOGGLE but RetroArch reports {other:?}"),
        }
    }

    pub fn load_state(&self) -> Result<()> {
        self.send("LOAD_STATE")?;
        sleep(COMMAND_SETTLE);
        Ok(())
    }

    pub fn frame_advance(&self) -> Result<()> {
        self.send("FRAMEADVANCE")?;
        sleep(COMMAND_SETTLE);
        Ok(())
    }

    pub fn quit(&self) -> Result<()> {
        self.send("QUIT")?;
        sleep(COMMAND_SETTLE);
        self.send("QUIT")
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass, and clippy**

Run: `cargo test remote:: && cargo clippy --all-targets -- -D warnings`
Expected: 8 tests PASS; clippy reports only that `Remote` items are unused (dead_code) until Task 3 wires them. If clippy's `-D warnings` fails on those dead-code warnings, that is expected at this stage: note it in the report and confirm `cargo clippy --all-targets` is otherwise clean; Task 3 clears it.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/remote.rs
git commit -m "Add a UDP client for RetroArch's command interface

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: appendconfig keys for the paused flow

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct PausedConfig { pub port: u16, pub slot: u32 }
  pub struct AppendConfig { pub window: WindowMode, pub staged_states_dir: Option<PathBuf>, pub paused: Option<PausedConfig> }
  ```
  When `paused` is `Some`, `render` appends, after the staged-states block:
  ```
  network_cmd_enable = "true"
  network_cmd_port = "<port>"
  state_slot = "<slot>"
  ```

- [ ] **Step 1: Add the field, fix every existing constructor with `paused: None`, and write the failing test**

Add to the tests module:

```rust
    #[test]
    fn render_paused_keys_after_staged_states() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: Some(PathBuf::from("/tmp/x/states")),
            paused: Some(PausedConfig { port: 55355, slot: 0 }),
        };
        let expected = format!(
            "{COMMON}savestate_directory = \"/tmp/x/states\"\n\
sort_savestates_enable = \"false\"\n\
sort_savestates_by_content_enable = \"false\"\n\
savestates_in_content_dir = \"false\"\n\
network_cmd_enable = \"true\"\n\
network_cmd_port = \"55355\"\n\
state_slot = \"0\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }

    #[test]
    fn render_paused_with_slot_and_no_staging() {
        let cfg = AppendConfig {
            window: WindowMode::Fullscreen,
            staged_states_dir: None,
            paused: Some(PausedConfig { port: 60000, slot: 3 }),
        };
        let expected = format!(
            "{COMMON}network_cmd_enable = \"true\"\n\
network_cmd_port = \"60000\"\n\
state_slot = \"3\"\n"
        );
        assert_eq!(cfg.render(), expected);
    }
```

- [ ] **Step 2: Run to verify the new tests fail and the old ones still pass**

Run: `cargo test config::`
Expected: 2 new FAIL, the rest PASS.

- [ ] **Step 3: Implement**

After the `staged_states_dir` block in `render`:

```rust
        if let Some(paused) = &self.paused {
            lines.push(("network_cmd_enable", "true".into()));
            lines.push(("network_cmd_port", paused.port.to_string()));
            lines.push(("state_slot", paused.slot.to_string()));
        }
```

- [ ] **Step 4: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all PASS (49 tests); clippy may still report the Task 1 dead code until Task 3.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "Render the command-port and state-slot keys for paused capture

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: the paused capture sequence in `capture::run`

**Files:**
- Modify: `src/capture.rs`

**Interfaces:**
- Consumes: `remote::{Remote, COMMAND_SETTLE}`
- Produces:
  ```rust
  pub enum Trigger {
      /// Wait this long after RetroArch is capturable, then capture whatever is on screen.
      Settle(Duration),
      /// Pause, load the configured state slot, advance `advance` frames; the last advance is captured.
      Paused { port: u16, advance: u32 },
  }
  pub struct CaptureOptions { pub trigger: Trigger, pub frames: u32, pub output: PathBuf, pub keep_running: bool, pub ready_timeout: Duration, pub log_path: PathBuf }
  ```
  (`settle: Duration` is replaced by `trigger`.)

- [ ] **Step 1: Extract `gpucapture_start` and restructure `run`**

Replace the inline `gpucapture start` block and the settle logic with:

```rust
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
```

And `run` becomes:

```rust
pub fn run(cmd: &LaunchCommand, opts: &CaptureOptions) -> Result<()> {
    prepare_output(&opts.output)?;
    // (unchanged: log file, spawn, guard, eprintln "launched RetroArch as pid")
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
```

`use crate::remote::Remote;` at the top. Remove the now-unused `settle` handling and make sure the `COMMAND_SETTLE` import is not needed here (it is used inside `remote`).

- [ ] **Step 2: Fix the compile in `main.rs` minimally**

Task 4 does the real wiring. For this task, keep `main.rs` compiling by constructing `trigger: capture::Trigger::Settle(Duration::from_secs_f64(cli.settle))` where `settle:` was. No new flags yet.

- [ ] **Step 3: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all PASS (49 tests), clippy clean (the `remote` items are now used; `Trigger::Paused` is constructed in Task 4, so if clippy flags it as never constructed, add the construction in Task 4 rather than an allow attribute and note it in the report).

- [ ] **Step 4: Commit**

```bash
git add src/capture.rs src/main.rs
git commit -m "Capture on a frame advance after pausing and loading the state

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: CLI wiring, `-e` removal, spec

**Files:**
- Modify: `src/main.rs`, `src/launch.rs`, `docs/superpowers/specs/2026-09-09-retroarch-capture-design.md`

**Interfaces:**
- `LaunchPlan` loses `slot`; `build_command` never emits `-e`. Argument order becomes `-L <core> [-f] [--set-shader <p>] --appendconfig <cfg> [-v] <rom>`.
- New CLI flags:
  ```
  --advance <N>     Frames to run after loading the state; the Nth is captured [default: 1, min 1]
  --cmd-port <PORT> UDP port for RetroArch's command interface, enabled only for this run [default: 55355]
  ```
  `--settle` help gains "(only used when no state is given)".

- [ ] **Step 1: `launch.rs`**

Remove the `slot` field, the `-e` push, and the `slot_zero_still_emits_dash_e` test; in `fullscreen_shader_slot_and_verbose_in_order` drop the `-e`/`3` expectation and `p.slot = Some(3)`, and rename it `fullscreen_shader_and_verbose_in_order`. Update the doc comment's argument order.

- [ ] **Step 2: `main.rs` flags and tests**

Add to `Cli`:

```rust
    /// Frames to run after loading the state; the Nth frame is the one captured
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    advance: u32,

    /// UDP port for RetroArch's command interface (enabled only for this run)
    #[arg(long, default_value_t = 55355)]
    cmd_port: u16,
```

Change `--settle`'s doc comment to `/// Seconds to wait before capturing when no state is given`.

Tests to add:

```rust
    #[test]
    fn advance_and_cmd_port_defaults_and_validation() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.advance, 1);
        assert_eq!(cli.cmd_port, 55355);
        assert!(parse(&["--advance", "0"]).is_err());
        assert_eq!(parse(&["--advance", "12"]).unwrap().advance, 12);
        assert_eq!(parse(&["--cmd-port", "60000"]).unwrap().cmd_port, 60000);
    }
```

- [ ] **Step 3: `main.rs` `run` wiring**

Replace the slot/staging match and the `AppendConfig`, `LaunchPlan`, `CaptureOptions` constructions with:

```rust
    let (paused, staged_states_dir) = match (&cli.state, cli.slot) {
        (Some(state_file), _) => {
            let dir = tmp.path().join("states");
            let slot = state::stage(state_file, &cli.rom, &dir)?;
            (Some(PausedConfig { port: cli.cmd_port, slot }), Some(dir))
        }
        (None, Some(slot)) => (Some(PausedConfig { port: cli.cmd_port, slot }), None),
        (None, None) => (None, None),
    };

    let append = AppendConfig { window: cli.window_mode(), staged_states_dir, paused };
    // ... write appendconfig as before ...

    let plan = LaunchPlan {
        binary,
        core,
        rom: cli.rom.clone(),
        shader: cli.shader.clone(),
        appendconfig,
        fullscreen: cli.fullscreen,
        verbose: cli.verbose,
    };
    // ...
    let trigger = match paused {
        Some(p) => capture::Trigger::Paused { port: p.port, advance: cli.advance },
        None => capture::Trigger::Settle(Duration::from_secs_f64(cli.settle)),
    };
    let opts = capture::CaptureOptions {
        trigger,
        frames: cli.frames,
        output: output.clone(),
        keep_running: cli.keep_running,
        ready_timeout: Duration::from_secs(30),
        log_path: tmp.path().join("retroarch.log"),
    };
```

Import `PausedConfig` from `config`.

- [ ] **Step 4: Run everything and the smoke tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 50 tests PASS, clippy clean.

Smoke (no RetroArch launched): `cargo run -q -- --core nope --rom /nonexistent.gb --output /tmp/x.gputrace` still names `nope_libretro.dylib`; `cargo run -q -- --advance 0 --core c --rom r --output o` is a clap error.

- [ ] **Step 5: Spec**

In the spec: replace the `launch` section's argument order with the new one (no `-e`); replace the `capture` step list's steps after "poll gpucapture list" with the two flows (Settle: as before; Paused: connect UDP, wait PLAYING, PAUSE_TOGGLE and confirm, LOAD_STATE, FRAMEADVANCE N minus 1 times, arm gpucapture on a thread, 500 ms, final FRAMEADVANCE, join; shutdown QUIT twice then SIGTERM/SIGKILL fallback); add `--advance` and `--cmd-port` to the CLI block; in the `config` section list the three paused keys; add a "Why a frame advance" paragraph with the measured facts from this plan's header.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/launch.rs docs/superpowers/specs/2026-09-09-retroarch-capture-design.md
git commit -m "Wire --advance and --cmd-port; state runs use the paused flow, never -e

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: README and two real runs

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Run the same capture twice**

```bash
for i in 1 2; do
cargo run -q -- --core sameboy \
  --rom "/Users/mike/workplace/vibeboy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].gbc" \
  --state "$HOME/Documents/RetroArch/states/SameBoy/Legend of Zelda, The - Link's Awakening DX (U) (V1.2) [C][!].state" \
  --shader "$HOME/Library/Application Support/RetroArch/shaders/shaders_slang/crt/crt-geom.slangp" \
  --output /tmp/ladx-paused-$i.gputrace --verbose
done
du -sh /tmp/ladx-paused-*.gputrace
```

Expected per run: stderr shows "paused on the loaded state; arming capture, then advancing frame 1", the tool prints the output path within about 10 s, RetroArch exits on its own (`pgrep -fl RetroArch` empty, ignoring unrelated matches). Record both bundle sizes. Also run once with `--advance 30` to `/tmp/ladx-paused-30.gputrace` and confirm it completes. Do not modify the tool to work around a failure; record the failure verbatim and report DONE_WITH_CONCERNS.

- [ ] **Step 2: README**

Options table: add `--advance N` and `--cmd-port PORT`; change `--settle` to "wait before capturing when no state is given". Replace "How it works" steps 3 and 4 with the two flows in plain words, including why a frame advance is needed (a paused RetroArch re-presents but gpucapture never sees a boundary; measured). Add a "Reproducible frames" paragraph: same state plus same `--advance` gives the same emulated frame, so change the shader and rerun to compare traces of one frame. Under Verified, add a dated (2026-09-10) subsection with the two paused runs and the `--advance 30` run, their sizes, and any issues.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document paused capture and record the reproducibility runs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Make the paused capture deterministic and hang-free

Added after Task 5's runs: `--advance 30` hung and the default only worked by
timing luck. Measured facts (2026-09-10, stock RetroArch.app, Vulkan):

- `LOAD_STATE` is asynchronous. Real presents continue for a short window
  after the command, and a capture armed inside that window completes on
  the load's own frames. Arming must happen after that window.
- After any `FRAMEADVANCE`, RetroArch presents nothing until the next
  advance. In that mode a capture needs THREE advances after arming: one
  advance never opens the capture, two open it without a drawable (the
  second frame's swapchain image was acquired before the window), three
  complete it. This count may depend on swapchain depth, so the tool must
  advance until the capture completes rather than hard-code 3.
- `gpucapture start` prints `triggering capture of 1 Frame from <pid> @ 0`
  91 ms after launch, flushed even when stdout is a pipe, before it blocks.

New semantics: `--advance N` (min 1, default 1) is the number of frame
advances after the state load and before the capture is armed. The recorded
frame is a fixed small number of advances after that, the same on every run,
because once in post-advance mode nothing depends on wall-clock time.

**Files:**
- Modify: `src/capture.rs`, `src/remote.rs`, `src/main.rs` (help text only), spec, README

**Interfaces:**
- `remote::LOAD_STATE_SETTLE: Duration = 1 s` (new; `load_state` sleeps this instead of `COMMAND_SETTLE`).
- `capture::MAX_CLOSING_ADVANCES: u32 = 6` (new).
- `capture::gpucapture_start` gains a readiness callback: `fn gpucapture_start(pid: u32, frames: u32, output: &Path, on_armed: impl FnOnce()) -> Result<()>`; it spawns gpucapture with stdout piped, reads lines, calls `on_armed` when a line starts with `triggering capture`, and keeps draining stdout until exit (so the child never blocks on a full pipe).
- `Trigger::Paused { port, advance }` unchanged in shape; `advance` now means pre-arm advances.

- [ ] **Step 1: `remote.rs`**

Add `pub const LOAD_STATE_SETTLE: Duration = Duration::from_secs(1);` with a doc comment stating the async-load fact, and use it in `load_state`. In `parse_status`, replace the `(words.next(), words.next())` tuple with two named lets (`command`, `state`) matched as a tuple; behavior identical, existing tests unchanged.

- [ ] **Step 2: `capture.rs`, readiness signal**

```rust
/// Run `gpucapture start`, calling `on_armed` once it reports it is waiting
/// for a boundary. gpucapture flushes that line even into a pipe (measured).
fn gpucapture_start(pid: u32, frames: u32, output: &Path, on_armed: impl FnOnce()) -> Result<()> {
    let mut child = Command::new("gpucapture")
        .args(["start", "--pid", &pid.to_string(), "--count", &frames.to_string()])
        .arg("--output")
        .arg(output)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("running `gpucapture start`")?;
    let stdout = child.stdout.take().context("gpucapture stdout")?;
    let mut on_armed = Some(on_armed);
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading gpucapture output")?;
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
    // (unchanged `index` check)
    Ok(())
}
```

The settle flow calls it with `|| {}`. Note the settle flow previously let gpucapture print progress to the terminal; with stdout piped that progress is no longer shown, which is fine (stderr still is).

- [ ] **Step 3: `capture.rs`, the paused sequence**

Replace the body of `capture_paused` after `remote.load_state()?` with:

```rust
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
    if armed_rx.recv_timeout(ARM_TIMEOUT).is_err() {
        guard.kill_now();
        let _ = capture.join();
        bail!("gpucapture did not report it was armed within {ARM_TIMEOUT:?}");
    }

    // A paused RetroArch presents only on frame advances, and gpucapture
    // needs more than one present to open and close a frame (measured: 3).
    // Advance until the capture thread finishes, with a hard cap.
    let mut closing = 0;
    while !capture.is_finished() {
        if closing == MAX_CLOSING_ADVANCES {
            guard.kill_now();
            let _ = capture.join();
            bail!(
                "capture did not complete after {MAX_CLOSING_ADVANCES} frame advances; \
                 RetroArch was killed to release gpucapture"
            );
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
```

with `const ARM_TIMEOUT: Duration = Duration::from_secs(10);` and `const MAX_CLOSING_ADVANCES: u32 = 6;`. Remove `ARM_DELAY`. `frame_advance` already sleeps `COMMAND_SETTLE` (250 ms) after sending, which gives gpucapture time to react between advances; `is_finished` is checked after each.

- [ ] **Step 4: `main.rs` help text**

`--advance`: `/// Frame advances after loading the state, before the capture is armed`.

- [ ] **Step 5: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: 49 tests PASS, clippy clean.

- [ ] **Step 6: Real runs**

Same command as Task 5, three times: `--advance 1` twice (outputs `/tmp/ladx-det-1.gputrace`, `/tmp/ladx-det-2.gputrace`) and `--advance 30` once (`/tmp/ladx-det-30.gputrace`). Expected stderr per run: "advanced N frame(s) ...", then "capture closed after K further advance(s)" with the same K every run (3 is the measured value), the output path, and RetroArch exiting on its own. Record K and the sizes. Also confirm `pgrep -fl "MacOS/RetroArch"` is empty afterwards.

- [ ] **Step 7: Spec and README**

Spec `capture` section: replace the paused-flow steps with the new sequence (load, 1 s, N advances, arm and wait for the readiness line, advance until complete with cap 6, QUIT) and replace the "Why a frame advance" paragraph with the measured facts above. README: update `--advance`'s row and the "How it works" paused flow accordingly, replace the "Known issues" entry about `--advance 30` with the new Verified results, and state plainly that the recorded frame is a fixed number of advances past `--advance`, identical across runs.

- [ ] **Step 8: Commit**

```bash
git add src/capture.rs src/remote.rs src/main.rs README.md docs/superpowers/specs/2026-09-09-retroarch-capture-design.md docs/superpowers/plans/2026-09-10-paused-capture.md
git commit -m "Advance until gpucapture closes the frame; wait for its readiness line

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
