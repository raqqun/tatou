//! `tatou daemon`: owns the terminals the shells, editors and agents run in, so
//! that they outlive the dashboard. Each terminal is a pseudo-terminal with a
//! program in it and a `vt100` screen kept from its output, so that a dashboard
//! attaching gets the screen as it is. Dashboards talk to it over a Unix socket
//! (`protocol`). It ends once its last terminal has ended and no dashboard is
//! connected, or when asked to stop.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tui_term::vt100;

use crate::config;
use crate::error::Result;
use crate::protocol::{Reply, Request, Session, Window};
use crate::reflow;

/// Lines of scrollback kept per terminal, by the daemon and by the dashboard for
/// the one it shows, which gets them all on attach.
pub const SCROLLBACK: usize = 10_000;

/// The smallest a terminal gets, (rows, columns), as a terminal window's: a pane
/// smaller than this shows the top left of a screen this size. Smaller, `vt100`
/// panics wrapping text on a single row, and shells misbehave: bash clears its
/// screen at 2 rows and moves over the lines above its prompt after.
pub const MIN_SIZE: (u16, u16) = (4, 20);

/// How long a daemon with nothing to do waits before ending, in case a dashboard
/// connects meanwhile: one just starting, between its first requests.
const ALONE_FOR: Duration = Duration::from_secs(10);

/// The daemon's socket: `$XDG_STATE_HOME/tatou/tatou.sock`.
pub fn socket_path() -> Result<PathBuf> {
    Ok(config::state_dir()?.join("tatou.sock"))
}

/// `tatou daemon`.
pub fn run() -> ExitCode {
    // Without a terminal, a panic's message would go nowhere
    std::panic::set_hook(Box::new(|info| {
        let thread = thread::current().name().map(String::from);
        tracing::error!(thread = ?thread, %info, "the daemon panicked");
    }));
    let served = socket_path()
        .map_err(|err| err.to_string())
        .and_then(|path| serve(&path, true).map_err(|err| format!("{}: {err}", path.display())));
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(err, "daemon failed");
            eprintln!("tatou: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Listens on `path` until the daemon ends; another daemon already listening
/// there is an error. With `ends`, the process ends when the daemon has nothing
/// left to do, or is asked to stop; tests serve without.
pub fn serve(path: &Path, ends: bool) -> io::Result<()> {
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "a daemon is already running",
        ));
    }
    // A socket left by a daemon that did not end cleanly
    let _ = fs::remove_file(path);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    tracing::info!(?path, "daemon listening");
    let daemon = Daemon::new(path, ends);
    daemon.end_if_idle_later();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let daemon = daemon.clone();
                thread::spawn(move || daemon.serve_client(stream));
            }
            Err(err) => tracing::warn!(%err, "cannot accept a connection"),
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Daemon {
    state: Arc<Mutex<State>>,
    socket: PathBuf,
    /// Whether the process ends with the daemon.
    ends: bool,
}

#[derive(Default)]
struct State {
    sessions: Vec<SessionState>,
    terminals: HashMap<u32, Terminal>,
    /// The last id given to a session, a terminal or a connection.
    last_id: u32,
    clients: usize,
}

struct SessionState {
    id: u32,
    name: String,
    path: PathBuf,
    /// In the order they were opened.
    terminals: Vec<u32>,
    /// The one spawned or attached to last.
    current: Option<u32>,
}

struct Terminal {
    session: u32,
    /// What it runs, by name.
    command: String,
    screen: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    /// Shared so that input is written without holding the state's lock, which a
    /// program not reading its input would otherwise block.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    /// When it last printed, in seconds since the Unix epoch.
    activity: u64,
    /// Whether its shell waits at its prompt, which it draws again on a resize.
    at_prompt: bool,
    /// The connections attached to it, by id.
    watchers: Vec<(u32, Sender<Reply>)>,
}

impl State {
    fn next_id(&mut self) -> u32 {
        self.last_id += 1;
        self.last_id
    }

    fn list(&self) -> Vec<Session> {
        let mut sessions: Vec<Session> = self
            .sessions
            .iter()
            .map(|session| {
                let windows: Vec<Window> = session
                    .terminals
                    .iter()
                    .enumerate()
                    .filter_map(|(index, id)| {
                        let terminal = self.terminals.get(id)?;
                        Some(Window {
                            id: format!("%{id}"),
                            index: index as u32,
                            current: session.current == Some(*id),
                            command: terminal.command.clone(),
                        })
                    })
                    .collect();
                let activity = (session.terminals.iter())
                    .filter_map(|id| self.terminals.get(id))
                    .map(|terminal| terminal.activity)
                    .max()
                    .unwrap_or(0);
                Session {
                    id: format!("${}", session.id),
                    name: session.name.clone(),
                    path: session.path.clone(),
                    activity,
                    windows,
                }
            })
            .collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        sessions
    }

    /// `name`, or `name-2`, `name-3`, … when a session has it.
    fn free_name(&self, name: &str) -> String {
        let taken = |candidate: &str| self.sessions.iter().any(|s| s.name == candidate);
        let mut candidate = name.to_string();
        let mut n = 1;
        while taken(&candidate) {
            n += 1;
            candidate = format!("{name}-{n}");
        }
        candidate
    }
}

impl Daemon {
    fn new(socket: &Path, ends: bool) -> Daemon {
        Daemon {
            state: Arc::new(Mutex::new(State::default())),
            socket: socket.to_path_buf(),
            ends,
        }
    }

    // A poisoned lock is a thread that panicked holding it; the state is still
    // usable, and the daemon would rather keep its terminals than end
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One connection: its requests, its replies, and while attached, a terminal's
    /// output, written by a thread of its own so that a slow dashboard never
    /// holds up a terminal.
    fn serve_client(&self, stream: UnixStream) {
        let (replies, outgoing) = mpsc::channel::<Reply>();
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        thread::spawn(move || {
            for reply in outgoing {
                if reply.write(&mut out).is_err() {
                    break;
                }
            }
        });
        let client = {
            let mut state = self.lock();
            state.clients += 1;
            state.next_id()
        };
        let mut attached: Option<u32> = None;
        let mut input = stream;
        loop {
            let request = match Request::read(&mut input) {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(%err, "a dashboard sent something unreadable");
                    break;
                }
            };
            if let Some(reply) = self.handle(request, client, &mut attached, &replies) {
                let _ = replies.send(reply);
            }
        }
        let mut state = self.lock();
        state.clients -= 1;
        if let Some(terminal) = attached.and_then(|id| state.terminals.get_mut(&id)) {
            terminal.watchers.retain(|(id, _)| *id != client);
        }
        drop(state);
        self.end_if_idle_later();
    }

    /// The reply to `request`; none for input and resizes, which only an attached
    /// connection sends, and whose output answers.
    fn handle(
        &self,
        request: Request,
        client: u32,
        attached: &mut Option<u32>,
        replies: &Sender<Reply>,
    ) -> Option<Reply> {
        let reply = match request {
            Request::List => Reply::Sessions(self.lock().list()),
            Request::Spawn {
                session,
                name,
                path,
                command,
            } => match self.spawn(session.as_deref(), &name, &path, command.as_deref()) {
                Ok((session, terminal)) => Reply::Spawned {
                    session: format!("${session}"),
                    terminal: format!("%{terminal}"),
                },
                Err(err) => Reply::Error(err),
            },
            Request::Kill { terminal } => match parse_id(&terminal, '%') {
                Some(id) => self.kill(&[id]),
                None => Reply::Error(format!("no terminal {terminal}")),
            },
            Request::KillSession { session } => {
                let state = self.lock();
                let terminals = parse_id(&session, '$')
                    .and_then(|id| state.sessions.iter().find(|s| s.id == id))
                    .map(|s| s.terminals.clone());
                drop(state);
                match terminals {
                    Some(terminals) => self.kill(&terminals),
                    None => Reply::Error(format!("no session {session}")),
                }
            }
            Request::Attach {
                terminal,
                rows,
                cols,
            } => match parse_id(&terminal, '%') {
                Some(id) => self.attach(id, rows, cols, client, attached, replies),
                None => Reply::Error(format!("no terminal {terminal}")),
            },
            Request::Input(bytes) => {
                let writer = attached.and_then(|id| {
                    let state = self.lock();
                    state.terminals.get(&id).map(|t| t.writer.clone())
                });
                if let Some(writer) = writer {
                    let mut writer = writer.lock().unwrap_or_else(|p| p.into_inner());
                    let _ = writer.write_all(&bytes).and_then(|()| writer.flush());
                }
                return None;
            }
            Request::Resize { rows, cols } => {
                let mut state = self.lock();
                if let Some(terminal) = attached.and_then(|id| state.terminals.get_mut(&id))
                    && resize(terminal, rows, cols)
                {
                    redraw_watchers(terminal);
                }
                return None;
            }
            Request::Stop => {
                tracing::info!("asked to stop");
                let all: Vec<u32> = self.lock().terminals.keys().copied().collect();
                self.kill(&all);
                let socket = self.socket.clone();
                // Once the reply is on its way
                let ends = self.ends;
                thread::spawn(move || {
                    if !ends {
                        return;
                    }
                    thread::sleep(Duration::from_millis(200));
                    let _ = fs::remove_file(&socket);
                    std::process::exit(0);
                });
                Reply::Ok
            }
        };
        Some(reply)
    }

    /// Starts `command` in a new terminal of `session`, or of a new session.
    /// Returns the session's id and the terminal's.
    fn spawn(
        &self,
        session: Option<&str>,
        name: &str,
        path: &Path,
        command: Option<&str>,
    ) -> std::result::Result<(u32, u32), String> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|err| format!("cannot open a terminal: {err}"))?;
        let mut builder = match command {
            Some(command) => {
                let mut builder = CommandBuilder::new("/bin/sh");
                builder.args(["-c", command]);
                builder
            }
            None => {
                let mut builder = CommandBuilder::new(user_shell());
                builder.arg("-l");
                builder
            }
        };
        builder.cwd(path);
        builder.env("TATOU", "1");
        builder.env("TERM", "xterm-256color");
        // Started from inside tmux, the daemon must not hand that on
        builder.env_remove("TMUX");
        builder.env_remove("TMUX_PANE");
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|err| format!("cannot start {}: {err}", command.unwrap_or("a shell")))?;
        drop(pair.slave);
        let reader = (pair.master.try_clone_reader())
            .map_err(|err| format!("cannot read the terminal: {err}"))?;
        let writer = (pair.master.take_writer())
            .map_err(|err| format!("cannot write to the terminal: {err}"))?;

        let mut state = self.lock();
        let session_id = match session.and_then(|s| parse_id(s, '$')) {
            Some(id) if state.sessions.iter().any(|s| s.id == id) => id,
            _ => {
                let id = state.next_id();
                let name = state.free_name(name);
                state.sessions.push(SessionState {
                    id,
                    name,
                    path: path.to_path_buf(),
                    terminals: Vec::new(),
                    current: None,
                });
                id
            }
        };
        let id = state.next_id();
        state.terminals.insert(
            id,
            Terminal {
                session: session_id,
                command: program_name(command),
                screen: vt100::Parser::new(size.rows, size.cols, SCROLLBACK),
                master: pair.master,
                writer: Arc::new(Mutex::new(writer)),
                child,
                activity: unix_now(),
                at_prompt: false,
                watchers: Vec::new(),
            },
        );
        if let Some(session) = state.sessions.iter_mut().find(|s| s.id == session_id) {
            session.terminals.push(id);
            session.current = Some(id);
        }
        drop(state);
        tracing::info!(id, ?path, command, "terminal started");
        let daemon = self.clone();
        thread::spawn(move || daemon.pump(id, reader));
        Ok((session_id, id))
    }

    /// Feeds terminal `id`'s output to its screen and its watchers until its
    /// program ends; then the terminal goes, and its session with its last one.
    fn pump(&self, id: u32, mut reader: Box<dyn Read + Send>) {
        let mut buffer = [0; 16 * 1024];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut state = self.lock();
            let Some(terminal) = state.terminals.get_mut(&id) else {
                break;
            };
            feed(&mut terminal.screen, &buffer[..n], id);
            terminal.at_prompt = at_prompt(terminal.at_prompt, &buffer[..n]);
            terminal.activity = unix_now();
            let output = &buffer[..n];
            terminal
                .watchers
                .retain(|(_, watcher)| watcher.send(Reply::Output(output.to_vec())).is_ok());
        }
        let mut state = self.lock();
        let Some(mut terminal) = state.terminals.remove(&id) else {
            return;
        };
        let status = terminal.child.wait();
        tracing::info!(id, ?status, "terminal ended");
        for (_, watcher) in &terminal.watchers {
            let _ = watcher.send(Reply::Exited);
        }
        if let Some(session) = state.sessions.iter_mut().find(|s| s.id == terminal.session) {
            session.terminals.retain(|t| *t != id);
            if session.current == Some(id) {
                session.current = session.terminals.last().copied();
            }
        }
        state.sessions.retain(|s| !s.terminals.is_empty());
        drop(state);
        self.end_if_idle_later();
    }

    /// Attaches the connection to terminal `id` at this size: the screen now, its
    /// output from then on.
    fn attach(
        &self,
        id: u32,
        rows: u16,
        cols: u16,
        client: u32,
        attached: &mut Option<u32>,
        replies: &Sender<Reply>,
    ) -> Reply {
        let mut state = self.lock();
        if !state.terminals.contains_key(&id) {
            return Reply::Error(format!("no terminal %{id}"));
        }
        if let Some(previous) = attached.and_then(|p| state.terminals.get_mut(&p)) {
            previous.watchers.retain(|(c, _)| *c != client);
        }
        let Some(terminal) = state.terminals.get_mut(&id) else {
            return Reply::Error(format!("no terminal %{id}"));
        };
        if resize(terminal, rows, cols) {
            redraw_watchers(terminal);
        }
        let screen = screen_now(terminal);
        terminal.watchers.push((client, replies.clone()));
        let session = terminal.session;
        if let Some(session) = state.sessions.iter_mut().find(|s| s.id == session) {
            session.current = Some(id);
        }
        *attached = Some(id);
        Reply::Screen(screen)
    }

    /// Ends the programs of `terminals`; each terminal goes once its output ends.
    fn kill(&self, terminals: &[u32]) -> Reply {
        let mut state = self.lock();
        for id in terminals {
            if let Some(terminal) = state.terminals.get_mut(id)
                && let Err(err) = terminal.child.kill()
            {
                tracing::warn!(id, %err, "cannot end a terminal's program");
            }
        }
        Reply::Ok
    }

    /// Ends the daemon in `ALONE_FOR` if it still has nothing to do then.
    fn end_if_idle_later(&self) {
        let daemon = self.clone();
        thread::spawn(move || {
            thread::sleep(ALONE_FOR);
            daemon.end_if_idle();
        });
    }

    /// Ends the daemon when it has nothing left to do: no terminal, nobody connected.
    fn end_if_idle(&self) {
        let state = self.lock();
        if self.ends && state.terminals.is_empty() && state.clients == 0 {
            tracing::info!("no terminal and no dashboard left: the daemon ends");
            let _ = fs::remove_file(&self.socket);
            std::process::exit(0);
        }
    }
}

/// Gives `terminal` a new size, its screen and its program's; false when it has
/// it already. Its screen is reflowed, as a terminal's, lines wrapping again at
/// the new width with nothing lost, and its cursor's line kept in sight.
fn resize(terminal: &mut Terminal, rows: u16, cols: u16) -> bool {
    let size = terminal.screen.screen().size();
    if (rows, cols) == size || rows < MIN_SIZE.0 || cols < MIN_SIZE.1 {
        return false;
    }
    let screen = &mut terminal.screen;
    let at_prompt = terminal.at_prompt;
    let reflowed = panic::catch_unwind(AssertUnwindSafe(|| {
        reflow::reflow(screen, rows, cols, SCROLLBACK, at_prompt)
    }));
    terminal.screen = reflowed.unwrap_or_else(|_| {
        tracing::error!(rows, cols, "cannot reflow a screen; starting it afresh");
        vt100::Parser::new(rows, cols, SCROLLBACK)
    });
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    if let Err(err) = terminal.master.resize(size) {
        tracing::warn!(%err, "cannot resize a terminal");
    }
    true
}

/// Whether a shell is at its prompt after printing `output`, from `before`: a
/// shell with shell integration marks where its input starts with OSC 133;B, and
/// leaves the prompt with a new line, the Enter that runs a command. A mark cut in
/// two between reads is missed, until the next prompt's.
fn at_prompt(before: bool, output: &[u8]) -> bool {
    const INPUT: &[u8] = b"\x1b]133;B";
    let marked = output.windows(INPUT.len()).rposition(|w| w == INPUT);
    let after = marked.map_or(0, |at| at + INPUT.len());
    let left = output[after..].contains(&b'\n');
    (before || marked.is_some()) && !left
}

/// Feeds `bytes`, terminal `id`'s output, to its screen. Should `vt100` panic on
/// them, the screen starts afresh at its size, which the program draws again: its
/// output still has to be read, or the program would block on it for good.
fn feed(screen: &mut vt100::Parser, bytes: &[u8], id: u32) {
    if panic::catch_unwind(AssertUnwindSafe(|| screen.process(bytes))).is_err() {
        tracing::error!(
            id,
            "a terminal's screen failed on its output; starting it afresh"
        );
        let (rows, cols) = screen.screen().size();
        *screen = vt100::Parser::new(rows, cols, SCROLLBACK);
    }
}

/// What a dashboard draws `terminal` from: its main screen's history and
/// contents, then, when a full-screen program runs, the alternate screen's,
/// switched to as the program did, which `state_formatted` alone does not say;
/// then the input modes.
fn screen_now(terminal: &mut Terminal) -> Vec<u8> {
    let alternate = terminal.screen.screen().alternate_screen();
    let mut bytes = Vec::new();
    on_main_screen(&mut terminal.screen, |screen| {
        bytes = history(screen.screen_mut());
        bytes.extend(screen.screen().contents_formatted());
    });
    if alternate {
        bytes.extend(b"\x1b[?1049h");
        bytes.extend(terminal.screen.screen().contents_formatted());
    }
    bytes.extend(terminal.screen.screen().input_mode_formatted());
    bytes
}

/// Runs `with` on `screen`'s main screen, and back to the alternate one after,
/// when a full-screen program runs; mode 47 switches without clearing either.
fn on_main_screen(screen: &mut vt100::Parser, with: impl FnOnce(&mut vt100::Parser)) {
    let alternate = screen.screen().alternate_screen();
    if alternate {
        screen.process(b"\x1b[?47l");
    }
    with(screen);
    if alternate {
        screen.process(b"\x1b[?47h");
    }
}

/// Sends `terminal`'s screen anew to the dashboards showing it, after a resize: a
/// screen they resized themselves would differ from the daemon's, which output
/// printed for the old size reaches after. In order with its output, sent under
/// the same lock.
fn redraw_watchers(terminal: &mut Terminal) {
    if terminal.watchers.is_empty() {
        return;
    }
    let screen = screen_now(terminal);
    terminal
        .watchers
        .retain(|(_, watcher)| watcher.send(Reply::Screen(screen.clone())).is_ok());
}

/// `3` from `%3` (with `prefix` `%`).
fn parse_id(id: &str, prefix: char) -> Option<u32> {
    id.strip_prefix(prefix)?.parse().ok()
}

/// The user's login shell, from `$SHELL`.
fn user_shell() -> String {
    env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// What a terminal runs, by name: `command`'s first word without its folder, the
/// editor for `${EDITOR:-vi} .`, the user's shell without a command.
fn program_name(command: Option<&str>) -> String {
    let first = match command {
        Some(command) if command.starts_with("${EDITOR") => {
            env::var("EDITOR").unwrap_or_else(|_| "vi".to_string())
        }
        Some(command) => command.to_string(),
        None => user_shell(),
    };
    let word = first.split_whitespace().next().unwrap_or_default();
    word.rsplit('/').next().unwrap_or(word).to_string()
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    since_epoch.map_or(0, |elapsed| elapsed.as_secs())
}

/// The lines that scrolled off the top of `screen`, oldest first, each with its
/// colours and a line end, then as many line ends as it is tall less one: fed to a
/// new screen of its size before `state_formatted`, they all become its scrollback.
/// None on the alternate screen, which keeps no history. Lines that wrapped come
/// back as separate lines.
fn history(screen: &mut vt100::Screen) -> Vec<u8> {
    if screen.alternate_screen() {
        return Vec::new();
    }
    let (rows, cols) = screen.size();
    // Viewing the history a screen at a time, from its start; offsets are clamped
    screen.set_scrollback(usize::MAX);
    let mut left = screen.scrollback();
    let mut bytes = Vec::new();
    while left > 0 {
        screen.set_scrollback(left);
        let take = left.min(usize::from(rows));
        // Each row moves the cursor right only: a stream of them draws line after line
        for row in screen.rows_formatted(0, cols).take(take) {
            bytes.extend(row);
            bytes.extend(b"\x1b[m\r\n");
        }
        left -= take;
    }
    screen.set_scrollback(0);
    if !bytes.is_empty() {
        for _ in 1..rows {
            bytes.extend(b"\r\n");
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_go_by_their_first_word() {
        assert_eq!(program_name(Some("claude --resume")), "claude");
        assert_eq!(program_name(Some("/usr/local/bin/codex")), "codex");
        assert_eq!(program_name(Some("sleep 1000")), "sleep");
    }

    #[test]
    fn a_screen_that_panics_on_output_starts_afresh() {
        // A single row: vt100 cannot wrap text there
        let mut screen = vt100::Parser::new(1, 1, SCROLLBACK);
        feed(&mut screen, b"abc", 1);
        assert_eq!(screen.screen().size(), (1, 1));
        feed(&mut screen, b"x", 1);
        assert_eq!(screen.screen().contents(), "x", "and goes on");
    }

    #[test]
    fn the_smallest_screen_takes_any_output_and_reflows() {
        let mut screen = vt100::Parser::new(24, 80, SCROLLBACK);
        screen.process("日本語 the quick brown fox\r\n".repeat(30).as_bytes());
        let (rows, cols) = MIN_SIZE;
        let mut small = reflow::reflow(&mut screen, rows, cols, SCROLLBACK, false);
        small.process(
            "日本 more output that wraps\r\n\x1b[2J\x1b[H"
                .repeat(5)
                .as_bytes(),
        );
        let back = reflow::reflow(&mut small, 24, 80, SCROLLBACK, false);
        assert_eq!(back.screen().size(), (24, 80));
    }

    #[test]
    fn a_shell_is_at_its_prompt_from_its_input_mark_to_a_new_line() {
        let prompt = b"\x1b]133;A\x07\r\nstatus\r\n> \x1b]133;B\x07";
        assert!(at_prompt(false, prompt));
        assert!(at_prompt(true, b"ls -l"), "typing");
        assert!(!at_prompt(true, b"\r\n"), "Enter");
        assert!(!at_prompt(false, b"output\r\nmore"), "a command's output");
        let redrawn = b"\r\x1b[K> \x1b]133;B\x07";
        assert!(at_prompt(false, redrawn));
        let after = b"\r\nfile\r\n> \x1b]133;B\x07";
        assert!(at_prompt(false, after), "the prompt again after a listing");
        assert!(!at_prompt(false, b"no marks, no prompt"));
    }

    #[test]
    fn ids_have_their_prefix() {
        assert_eq!(parse_id("%3", '%'), Some(3));
        assert_eq!(parse_id("$12", '$'), Some(12));
        assert_eq!(parse_id("3", '%'), None);
        assert_eq!(parse_id("%x", '%'), None);
    }
}
