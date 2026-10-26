//! The dashboard's side of the daemon's socket: one connection per request, and
//! one long-lived connection for the terminal it shows.

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::daemon;
use crate::error::{Error, Result};
use crate::protocol::{Reply, Request, Session};

/// How long to wait for a daemon just started to listen.
const STARTING_FOR: Duration = Duration::from_secs(3);

/// The daemon, by its socket.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: &Path) -> Client {
        Client {
            socket: socket.to_path_buf(),
        }
    }

    /// The daemon on its usual socket.
    pub fn daemon() -> Result<Client> {
        Ok(Client::new(&daemon::socket_path()?))
    }

    /// Whether a daemon listens.
    pub fn is_up(&self) -> bool {
        UnixStream::connect(&self.socket).is_ok()
    }

    /// Starts the daemon unless one listens: this program as `tatou daemon`, in a
    /// process group of its own and without the terminal, so that it outlives
    /// this process and the terminal closing.
    pub fn start_daemon(&self) -> Result<()> {
        if self.is_up() {
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|source| Error::Spawn {
            program: "tatou daemon".into(),
            source,
        })?;
        Command::new(&exe)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|source| Error::Spawn {
                program: "tatou daemon".into(),
                source,
            })?;
        let mut waited = Duration::ZERO;
        while !self.is_up() {
            if waited >= STARTING_FOR {
                return Err(Error::Daemon(
                    "the daemon did not start; see the log".into(),
                ));
            }
            thread::sleep(Duration::from_millis(50));
            waited += Duration::from_millis(50);
        }
        tracing::info!("daemon started");
        Ok(())
    }

    /// Sends `request` on a connection of its own and returns the reply; the
    /// daemon's errors as `Error::Daemon`.
    pub fn request(&self, request: &Request) -> Result<Reply> {
        let io = |err: io::Error| Error::Daemon(format!("cannot reach the daemon: {err}"));
        let mut stream = UnixStream::connect(&self.socket).map_err(io)?;
        request.write(&mut stream).map_err(io)?;
        match Reply::read(&mut stream).map_err(io)? {
            Some(Reply::Error(message)) => Err(Error::Daemon(message)),
            Some(reply) => Ok(reply),
            None => Err(Error::Daemon("the daemon hung up".into())),
        }
    }

    pub fn sessions(&self) -> Result<Vec<Session>> {
        match self.request(&Request::List)? {
            Reply::Sessions(sessions) => Ok(sessions),
            other => Err(unexpected(other)),
        }
    }

    /// Starts `command` (`None`: the user's shell) in `path`, in `session` or a new
    /// one named `name`; returns the new terminal's id.
    pub fn spawn(
        &self,
        session: Option<&str>,
        name: &str,
        path: &Path,
        command: Option<&str>,
        agent: bool,
    ) -> Result<String> {
        let request = Request::Spawn {
            session: session.map(String::from),
            name: name.to_string(),
            path: path.to_path_buf(),
            command: command.map(String::from),
            agent,
        };
        match self.request(&request)? {
            Reply::Spawned { terminal, .. } => Ok(terminal),
            other => Err(unexpected(other)),
        }
    }

    pub fn kill(&self, terminal: &str) -> Result<()> {
        let terminal = terminal.to_string();
        self.request(&Request::Kill { terminal }).map(|_| ())
    }

    pub fn kill_session(&self, session: &str) -> Result<()> {
        let session = session.to_string();
        self.request(&Request::KillSession { session }).map(|_| ())
    }

    pub fn stop(&self) -> Result<()> {
        self.request(&Request::Stop).map(|_| ())
    }

    /// The long-lived connection for the shown terminal: requests go out through
    /// the returned `Stream`, what the daemon streams back arrives on the receiver,
    /// read by a thread of its own; it closes when the daemon hangs up.
    pub fn stream(&self) -> Result<(Stream, Receiver<Reply>)> {
        let io = |err: io::Error| Error::Daemon(format!("cannot reach the daemon: {err}"));
        let stream = UnixStream::connect(&self.socket).map_err(io)?;
        let mut input = stream.try_clone().map_err(io)?;
        let (replies, received) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(reply)) = Reply::read(&mut input) {
                if replies.send(reply).is_err() {
                    break;
                }
            }
        });
        let (requests, outgoing) = mpsc::channel::<Request>();
        let mut output = stream;
        thread::spawn(move || {
            for request in outgoing {
                if request.write(&mut output).is_err() {
                    break;
                }
            }
        });
        Ok((Stream { requests }, received))
    }
}

/// Requests for the long-lived connection, written by a thread of its own so
/// that the UI never waits on the socket.
#[derive(Debug, Clone)]
pub struct Stream {
    requests: Sender<Request>,
}

impl Stream {
    pub fn send(&self, request: Request) {
        // The writer ends with the connection: the daemon is gone, and nothing shows
        let _ = self.requests.send(request);
    }
}

fn unexpected(reply: Reply) -> Error {
    Error::Daemon(format!("unexpected reply from the daemon: {reply:?}"))
}

/// A daemon for tests.
#[cfg(test)]
pub mod testing {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::Client;
    use crate::daemon;

    /// A daemon in this process on a socket of its own, which never ends the
    /// process; it serves until the test ends.
    pub fn daemon() -> (tempfile::TempDir, Client) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let path = socket.clone();
        thread::spawn(move || daemon::serve(&path, false));
        let client = Client::new(&socket);
        let started = Instant::now();
        while !client.is_up() {
            assert!(started.elapsed() < Duration::from_secs(3), "no daemon");
            thread::sleep(Duration::from_millis(10));
        }
        (dir, client)
    }

    /// Waits until `client` lists `n` sessions, up to 5 s; programs end, and their
    /// terminals with them, a moment after they are killed.
    pub fn sessions_become(client: &Client, n: usize) {
        let started = Instant::now();
        while client.sessions().unwrap().len() != n {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "never {n} sessions"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::testing::{daemon, sessions_become};
    use super::*;

    /// Replies from `received` until `until` holds of what was printed, or 5 s.
    fn printed(received: &Receiver<Reply>, until: impl Fn(&str) -> bool) -> (String, bool) {
        let mut text = String::new();
        let mut exited = false;
        let started = Instant::now();
        while !until(&text) && started.elapsed() < Duration::from_secs(5) {
            match received.recv_timeout(Duration::from_millis(100)) {
                Ok(Reply::Screen(bytes) | Reply::Output(bytes)) => {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
                Ok(Reply::Exited) => exited = true,
                Ok(_) | Err(_) => {}
            }
            if exited {
                break;
            }
        }
        (text, exited)
    }

    #[test]
    fn spawns_attaches_types_and_sees_the_program_end() {
        let (dir, client) = daemon();
        let terminal = client
            .spawn(None, "app-fix", dir.path(), Some("cat"), true)
            .unwrap();
        let sessions = client.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "app-fix");
        assert_eq!(sessions[0].agent.as_deref(), Some(terminal.as_str()));
        assert_eq!(sessions[0].windows[0].command, "cat");

        let (stream, received) = client.stream().unwrap();
        stream.send(Request::Attach {
            terminal: terminal.clone(),
            rows: 10,
            cols: 40,
        });
        stream.send(Request::Input(b"hello\r".to_vec()));
        let (text, _) = printed(&received, |text| text.contains("hello"));
        assert!(text.contains("hello"), "{text:?}");

        // Attached again elsewhere: the screen as it is
        let (_again, received_again) = client.stream().unwrap();
        _again.send(Request::Attach {
            terminal: terminal.clone(),
            rows: 10,
            cols: 40,
        });
        let (screen, _) = printed(&received_again, |text| text.contains("hello"));
        assert!(screen.contains("hello"), "{screen:?}");

        // Ctrl-D ends cat, and the terminal and its session with it
        stream.send(Request::Input(vec![4]));
        let (_, exited) = printed(&received, |_| false);
        assert!(exited);
        sessions_become(&client, 0);
    }

    #[test]
    fn terminals_of_a_worktree_share_its_session() {
        let (dir, client) = daemon();
        let first = client
            .spawn(None, "app-fix", dir.path(), Some("sleep 100"), true)
            .unwrap();
        let session = client.sessions().unwrap()[0].id.clone();
        let second = client
            .spawn(Some(&session), "app-fix", dir.path(), None, false)
            .unwrap();
        // A new session with the same name gets a number
        client
            .spawn(None, "app-fix", dir.path(), Some("sleep 100"), false)
            .unwrap();
        let sessions = client.sessions().unwrap();
        let names: Vec<_> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["app-fix", "app-fix-2"]);
        let windows: Vec<_> = sessions[0]
            .windows
            .iter()
            .map(|w| (w.id.clone(), w.current))
            .collect();
        assert_eq!(windows, [(first, false), (second, true)]);

        client.kill_session(&session).unwrap();
        sessions_become(&client, 1);
    }

    #[test]
    fn the_daemons_errors_come_back_as_errors() {
        let (_dir, client) = daemon();
        let err = client.kill("%99").map(|_| ());
        // Killing a terminal that is gone is nothing to do
        assert!(err.is_ok());
        let err = client.kill_session("$99").unwrap_err();
        assert_eq!(err.to_string(), "no session $99");
        let nobody = Client::new(Path::new("/nonexistent/tatou.sock"));
        assert!(!nobody.is_up());
        assert!(nobody.sessions().is_err());
    }
}
