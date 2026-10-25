//! What the dashboard and the daemon say to each other over the daemon's Unix
//! socket: frames of a 4-byte big-endian length then a payload, whose first byte
//! says what it is. Strings and byte strings are a 4-byte length then the bytes.

use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Frames larger than this are refused: a broken peer, not a real message.
const MAX_FRAME: u32 = 64 * 1024 * 1024;

/// What the dashboard asks; each gets one `Reply`, but `Attach`, which streams.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Every session and its terminals.
    List,
    /// Starts `command` (`None`: the user's shell) in a terminal in `path`: in the
    /// session `session` (an id), or a new one named `name` (`name-2`, … when taken).
    Spawn {
        session: Option<String>,
        name: String,
        path: PathBuf,
        command: Option<String>,
        /// The session's agent, which its status follows.
        agent: bool,
    },
    /// Ends the terminal and what runs in it.
    Kill { terminal: String },
    /// Ends the session and every terminal in it.
    KillSession { session: String },
    /// From now on, this connection gets `terminal`'s screen, then its output, at
    /// this size; input and resizes may follow on it.
    Attach {
        terminal: String,
        rows: u16,
        cols: u16,
    },
    /// Bytes for the attached terminal's program, as typed.
    Input(Vec<u8>),
    /// The attached terminal's new size.
    Resize { rows: u16, cols: u16 },
    /// Ends every terminal, then the daemon.
    Stop,
}

/// What the daemon answers, or streams to an attached connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok,
    Error(String),
    Sessions(Vec<Session>),
    /// The terminal started, and its session.
    Spawned {
        session: String,
        terminal: String,
    },
    /// The attached terminal's whole screen, as escape codes that draw it.
    Screen(Vec<u8>),
    /// What the attached terminal's program printed since.
    Output(Vec<u8>),
    /// The attached terminal's program ended, and the terminal with it.
    Exited,
}

/// A session: the terminals of one worktree, or of one folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `$1`.
    pub id: String,
    pub name: String,
    /// The folder it was started in: the worktree it belongs to.
    pub path: PathBuf,
    /// Its agent's terminal.
    pub agent: Option<String>,
    /// When its agent last printed, or any terminal without one, in seconds since
    /// the Unix epoch.
    pub activity: u64,
    /// Its terminals, in the order they were opened.
    pub windows: Vec<Window>,
}

/// A terminal of a session, which the UI calls a window.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// `%3`, the terminal's id.
    pub id: String,
    pub index: u32,
    /// The one opened last.
    pub current: bool,
    /// What it was started with, by name: `claude`, `zsh`, `vi`.
    pub command: String,
}

/// Writes `payload` as one frame.
fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| invalid("frame too large"))?;
    out.write_all(&len.to_be_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

/// Reads one frame; `None` at the end of the stream.
fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    let mut payload = vec![0; len as usize];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

impl Request {
    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut p = Payload::default();
        match self {
            Request::List => p.u8(1),
            Request::Spawn {
                session,
                name,
                path,
                command,
                agent,
            } => {
                p.u8(2);
                p.option(session.as_deref());
                p.str(name);
                p.str(&path.to_string_lossy());
                p.option(command.as_deref());
                p.u8(u8::from(*agent));
            }
            Request::Kill { terminal } => {
                p.u8(3);
                p.str(terminal);
            }
            Request::KillSession { session } => {
                p.u8(4);
                p.str(session);
            }
            Request::Attach {
                terminal,
                rows,
                cols,
            } => {
                p.u8(5);
                p.str(terminal);
                p.u16(*rows);
                p.u16(*cols);
            }
            Request::Input(bytes) => {
                p.u8(6);
                p.bytes(bytes);
            }
            Request::Resize { rows, cols } => {
                p.u8(7);
                p.u16(*rows);
                p.u16(*cols);
            }
            Request::Stop => p.u8(8),
        }
        write_frame(out, &p.0)
    }

    /// The next request; `None` once the dashboard has hung up.
    pub fn read(input: &mut impl Read) -> io::Result<Option<Request>> {
        let Some(frame) = read_frame(input)? else {
            return Ok(None);
        };
        let mut r = Reader(&frame);
        let request = match r.u8()? {
            1 => Request::List,
            2 => Request::Spawn {
                session: r.option()?,
                name: r.str()?,
                path: PathBuf::from(r.str()?),
                command: r.option()?,
                agent: r.u8()? == 1,
            },
            3 => Request::Kill { terminal: r.str()? },
            4 => Request::KillSession { session: r.str()? },
            5 => Request::Attach {
                terminal: r.str()?,
                rows: r.u16()?,
                cols: r.u16()?,
            },
            6 => Request::Input(r.bytes()?),
            7 => Request::Resize {
                rows: r.u16()?,
                cols: r.u16()?,
            },
            8 => Request::Stop,
            _ => return Err(invalid("unknown request")),
        };
        Ok(Some(request))
    }
}

impl Reply {
    pub fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut p = Payload::default();
        match self {
            Reply::Ok => p.u8(1),
            Reply::Error(message) => {
                p.u8(2);
                p.str(message);
            }
            Reply::Sessions(sessions) => {
                p.u8(3);
                p.u32(sessions.len() as u32);
                for session in sessions {
                    p.str(&session.id);
                    p.str(&session.name);
                    p.str(&session.path.to_string_lossy());
                    p.option(session.agent.as_deref());
                    p.u64(session.activity);
                    p.u32(session.windows.len() as u32);
                    for window in &session.windows {
                        p.str(&window.id);
                        p.u32(window.index);
                        p.u8(u8::from(window.current));
                        p.str(&window.command);
                    }
                }
            }
            Reply::Spawned { session, terminal } => {
                p.u8(4);
                p.str(session);
                p.str(terminal);
            }
            Reply::Screen(bytes) => {
                p.u8(5);
                p.bytes(bytes);
            }
            Reply::Output(bytes) => {
                p.u8(6);
                p.bytes(bytes);
            }
            Reply::Exited => p.u8(7),
        }
        write_frame(out, &p.0)
    }

    /// The next reply; `None` once the daemon has hung up.
    pub fn read(input: &mut impl Read) -> io::Result<Option<Reply>> {
        let Some(frame) = read_frame(input)? else {
            return Ok(None);
        };
        let mut r = Reader(&frame);
        let reply = match r.u8()? {
            1 => Reply::Ok,
            2 => Reply::Error(r.str()?),
            3 => {
                let mut sessions = Vec::new();
                for _ in 0..r.u32()? {
                    let id = r.str()?;
                    let name = r.str()?;
                    let path = PathBuf::from(r.str()?);
                    let agent = r.option()?;
                    let activity = r.u64()?;
                    let mut windows = Vec::new();
                    for _ in 0..r.u32()? {
                        windows.push(Window {
                            id: r.str()?,
                            index: r.u32()?,
                            current: r.u8()? == 1,
                            command: r.str()?,
                        });
                    }
                    sessions.push(Session {
                        id,
                        name,
                        path,
                        agent,
                        activity,
                        windows,
                    });
                }
                Reply::Sessions(sessions)
            }
            4 => Reply::Spawned {
                session: r.str()?,
                terminal: r.str()?,
            },
            5 => Reply::Screen(r.bytes()?),
            6 => Reply::Output(r.bytes()?),
            7 => Reply::Exited,
            _ => return Err(invalid("unknown reply")),
        };
        Ok(Some(reply))
    }
}

/// A payload being written.
#[derive(Default)]
struct Payload(Vec<u8>);

impl Payload {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.0.extend_from_slice(value);
    }

    fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// A flag, then the string when there is one.
    fn option(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.str(value);
            }
            None => self.u8(0),
        }
    }
}

/// A payload being read, from the front.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(invalid("frame too short"));
        }
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(taken)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let mut value = [0; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(value))
    }

    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn str(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| invalid("a string is not UTF-8"))
    }

    fn option(&mut self) -> io::Result<Option<String>> {
        match self.u8()? {
            0 => Ok(None),
            _ => Ok(Some(self.str()?)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(request: Request) {
        let mut buffer = Vec::new();
        request.write(&mut buffer).unwrap();
        let read = Request::read(&mut buffer.as_slice()).unwrap();
        assert_eq!(read, Some(request));
    }

    fn round_trip_reply(reply: Reply) {
        let mut buffer = Vec::new();
        reply.write(&mut buffer).unwrap();
        let read = Reply::read(&mut buffer.as_slice()).unwrap();
        assert_eq!(read, Some(reply));
    }

    #[test]
    fn requests_round_trip() {
        round_trip_request(Request::List);
        round_trip_request(Request::Spawn {
            session: Some("$1".into()),
            name: "app-fix".into(),
            path: "/code/app wt/fix".into(),
            command: Some("claude --flag 'é'".into()),
            agent: true,
        });
        round_trip_request(Request::Spawn {
            session: None,
            name: "app-main".into(),
            path: "/code/app".into(),
            command: None,
            agent: false,
        });
        round_trip_request(Request::Kill {
            terminal: "%3".into(),
        });
        round_trip_request(Request::KillSession {
            session: "$2".into(),
        });
        round_trip_request(Request::Attach {
            terminal: "%3".into(),
            rows: 40,
            cols: 120,
        });
        round_trip_request(Request::Input(vec![0x1b, b'[', b'A', 0, 255]));
        round_trip_request(Request::Resize { rows: 1, cols: 2 });
        round_trip_request(Request::Stop);
    }

    #[test]
    fn replies_round_trip() {
        round_trip_reply(Reply::Ok);
        round_trip_reply(Reply::Error("no such terminal".into()));
        round_trip_reply(Reply::Sessions(vec![]));
        round_trip_reply(Reply::Sessions(vec![Session {
            id: "$1".into(),
            name: "app-fix".into(),
            path: "/code/app.worktrees/fix".into(),
            agent: Some("%1".into()),
            activity: 1_792_788_783,
            windows: vec![
                Window {
                    id: "%1".into(),
                    index: 0,
                    current: false,
                    command: "claude".into(),
                },
                Window {
                    id: "%2".into(),
                    index: 1,
                    current: true,
                    command: "zsh".into(),
                },
            ],
        }]));
        round_trip_reply(Reply::Spawned {
            session: "$1".into(),
            terminal: "%2".into(),
        });
        round_trip_reply(Reply::Screen(b"\x1b[H\x1b[2Jhello".to_vec()));
        round_trip_reply(Reply::Output(vec![]));
        round_trip_reply(Reply::Exited);
    }

    #[test]
    fn the_end_of_the_stream_is_none_and_garbage_an_error() {
        assert_eq!(Request::read(&mut &b""[..]).unwrap(), None);
        assert!(
            Request::read(&mut &[0, 0, 0, 1, 99][..]).is_err(),
            "unknown"
        );
        assert!(
            Request::read(&mut &[0, 0, 0, 3, 2, 1][..]).is_err(),
            "short"
        );
        assert!(
            Reply::read(&mut &[0xff, 0xff, 0xff, 0xff][..]).is_err(),
            "huge"
        );
    }
}
