//! RetroArch's UDP command interface (`network_cmd_enable`). Commands are
//! plain ASCII datagrams; only `GET_STATUS` replies.

use anyhow::{Context, Result, bail};
use std::net::UdpSocket;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Time RetroArch is given to act on a command that produces no reply.
/// A frame is about 17 ms.
pub const COMMAND_SETTLE: Duration = Duration::from_millis(250);

/// Time given to LOAD_STATE before anything else happens. Measured:
/// LOAD_STATE is asynchronous and real presents continue for a short
/// window after the command; a capture armed inside that window completes
/// on the load's own frames instead of a frame advance's, so arming must
/// wait until this window has passed.
pub const LOAD_STATE_SETTLE: Duration = Duration::from_secs(1);

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
    let mut words = reply.split_whitespace();
    let command = words.next();
    let state = words.next();
    match (command, state) {
        (Some("GET_STATUS"), Some("PLAYING")) => Status::Playing,
        (Some("GET_STATUS"), Some("PAUSED")) => Status::Paused,
        (Some("GET_STATUS"), Some("CONTENTLESS")) => Status::Contentless,
        _ => Status::Other(reply.trim().to_string()),
    }
}

/// A connected UDP socket to one RetroArch process's command port.
pub struct Remote {
    socket: UdpSocket,
}

impl Remote {
    /// Bind an ephemeral loopback socket and point it at RetroArch's port.
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
        let n = self
            .socket
            .recv(&mut buf)
            .context("no reply to GET_STATUS; is network_cmd_enable on and the port free?")?;
        Ok(parse_status(&String::from_utf8_lossy(&buf[..n])))
    }

    /// Poll until content is running.
    pub fn wait_playing(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let last = match self.status() {
                Ok(Status::Playing) => return Ok(()),
                Ok(Status::Paused) => "PAUSED".to_string(),
                Ok(Status::Contentless) => "CONTENTLESS".to_string(),
                Ok(Status::Other(s)) => s,
                Err(e) => e.to_string(),
            };
            if Instant::now() >= deadline {
                bail!("RetroArch did not report PLAYING within {timeout:?}; last status: {last}");
            }
            sleep(STATUS_POLL);
        }
    }

    /// PAUSE_TOGGLE, then confirm RetroArch reports PAUSED.
    pub fn pause(&self) -> Result<()> {
        self.send("PAUSE_TOGGLE")?;
        sleep(COMMAND_SETTLE);
        match self.status()? {
            Status::Paused => Ok(()),
            other => bail!("sent PAUSE_TOGGLE but RetroArch reports {other:?}"),
        }
    }

    /// LOAD_STATE; RetroArch loads the configured slot and keeps presenting
    /// real frames for a short window afterward (measured; LOAD_STATE is
    /// asynchronous). Waits LOAD_STATE_SETTLE so that window passes before
    /// anything is armed against it.
    pub fn load_state(&self) -> Result<()> {
        self.send("LOAD_STATE")?;
        sleep(LOAD_STATE_SETTLE);
        Ok(())
    }

    /// FRAMEADVANCE, then wait COMMAND_SETTLE.
    pub fn frame_advance(&self) -> Result<()> {
        self.send("FRAMEADVANCE")?;
        sleep(COMMAND_SETTLE);
        Ok(())
    }

    /// QUIT twice: RetroArch's default `quit_press_twice` needs a second press.
    pub fn quit(&self) -> Result<()> {
        self.send("QUIT")?;
        sleep(COMMAND_SETTLE);
        self.send("QUIT")
    }
}

/// Probe whether `port` already belongs to somebody else's RetroArch
/// command interface, before we ask our own RetroArch to bind it. If the
/// port is already bound, our run config's `network_cmd_port` fails to
/// bind and our datagrams go to whatever already holds it instead, so this
/// guards against `PAUSE_TOGGLE`, `LOAD_STATE` and `QUIT` reaching an
/// unrelated, running RetroArch. Any reply to `GET_STATUS` is treated as
/// evidence the port is taken; a read timeout means it is free.
pub fn probe_free(port: u16) -> Result<()> {
    let remote = Remote::connect(port)?;
    remote.send("GET_STATUS")?;
    let mut buf = [0u8; 1024];
    match remote.socket.recv(&mut buf) {
        Ok(_) => bail!(
            "UDP port {port} already answers GET_STATUS; another RetroArch \
             has the command interface on it, choose --cmd-port"
        ),
        Err(_) => Ok(()),
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
                        state = if state == "PAUSED" {
                            "PLAYING".into()
                        } else {
                            "PAUSED".into()
                        };
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
        assert_eq!(
            parse_status("GET_STATUS PLAYING game_boy,Zelda,crc32=1"),
            Status::Playing
        );
        assert_eq!(
            parse_status("GET_STATUS PAUSED game_boy,Zelda,crc32=1"),
            Status::Paused
        );
        assert_eq!(parse_status("GET_STATUS CONTENTLESS"), Status::Contentless);
        assert_eq!(
            parse_status("GET_STATUS ERROR"),
            Status::Other("GET_STATUS ERROR".into())
        );
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
        let err = r
            .wait_playing(Duration::from_millis(500))
            .unwrap_err()
            .to_string();
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
    fn probe_free_errors_when_something_answers() {
        let f = fake("PLAYING", true);
        let err = probe_free(f.port).unwrap_err().to_string();
        assert!(err.contains("already answers"), "{err}");
    }

    #[test]
    fn probe_free_succeeds_when_nothing_listens() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        probe_free(port).unwrap();
    }

    #[test]
    fn stray_datagram_from_another_socket_is_ignored() {
        let f = fake("PLAYING", false);
        let r = Remote::connect(f.port).unwrap();
        let local_port = r.socket.local_addr().unwrap().port();

        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        stranger
            .send_to(
                b"GET_STATUS PLAYING game_boy,Zelda,crc32=0",
                ("127.0.0.1", local_port),
            )
            .unwrap();

        // The connected socket only accepts datagrams from the fake
        // RetroArch's address, so this must still time out rather than
        // reading the stranger's message.
        let err = r.status().unwrap_err().to_string();
        assert!(err.contains("GET_STATUS"), "{err}");
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
        let commands: Vec<&str> = got
            .iter()
            .map(String::as_str)
            .filter(|c| *c != "GET_STATUS")
            .collect();
        assert_eq!(
            commands,
            ["PAUSE_TOGGLE", "LOAD_STATE", "FRAMEADVANCE", "QUIT", "QUIT"]
        );
    }
}
