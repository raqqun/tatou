//! tatou's own tmux server: its generated `tmux.conf`, sessions, options,
//! switch/popup/window/send-keys, and output parsing.

use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::exec::{self, Output};

/// Lines of scrollback kept per pane; agents print a lot.
const HISTORY_LIMIT: u32 = 50_000;

/// The `-L` socket name of tatou's server.
const SOCKET: &str = "tatou";

/// The session the dashboard runs in.
pub const DASHBOARD: &str = "tatou";

/// POSIX `sh` run after each command tatou starts in a window, while its pane
/// still exists. Shown in the dashboard's slot, the pane first goes back to its
/// window, the placeholder taking the slot again and the dashboard the focus, so
/// that it ends where it came from. Then the clients whose current window is this
/// pane's go back to the dashboard. Only those, so a program ending elsewhere moves
/// nobody; without a dashboard session, the switch fails quietly.
const BACK_TO_DASHBOARD: &str = r##"if [ "$(tmux display-message -p -t "$TMUX_PANE" '#{@tatou_shown}')" = 1 ]; then p=$(tmux list-panes -a -f '#{@tatou_placeholder}' -F '#{pane_id}'); tmux swap-pane -d -s "$TMUX_PANE" -t "$p" \; set-option -pu -t "$TMUX_PANE" @tatou_shown \; select-pane -t "$p" -L; fi; w=$(tmux display-message -p -t "$TMUX_PANE" '#{window_id}'); for c in $(tmux list-clients -f "#{==:#{window_id},$w}" -F '#{client_name}'); do tmux switch-client -c "$c" -t '=tatou' 2>/dev/null; done"##;

/// POSIX `sh` run once the dashboard has ended, however it ended: the pane shown in
/// its slot goes back to its window, and the placeholder is closed, which closes the
/// dashboard's window and session.
const CLOSE_SLOT: &str = r##"p=$(tmux list-panes -a -f '#{@tatou_placeholder}' -F '#{pane_id}'); s=$(tmux list-panes -a -f '#{@tatou_shown}' -F '#{pane_id}'); if [ -n "$s" ] && [ -n "$p" ]; then tmux swap-pane -d -s "$s" -t "$p" \; set-option -pu -t "$s" @tatou_shown; fi; [ -n "$p" ] && tmux kill-pane -t "$p"; true"##;

/// What the dashboard's slot runs while no window is shown there, and in a shown
/// window's place meanwhile: a line saying which, from the session it is in.
const PLACEHOLDER: &str = r##"printf '\033[?25l'; while :; do if [ "$(tmux display-message -p -t "$TMUX_PANE" '#{session_name}')" = tatou ]; then m='Enter on a worktree or a window shows it here'; else m="Shown in tatou's dashboard"; fi; if [ "$m" != "$o" ]; then printf '\033[H\033[2J\n  %s\n' "$m"; o=$m; fi; sleep 1; done"##;

/// The dashboard's slot, right of it, as a share of its window.
const SLOT_WIDTH: &str = "60%";

/// The shell code a window tatou opens runs: `command`, or without one the user's
/// shell as a login shell (as tmux starts it), then `BACK_TO_DASHBOARD`. A newline,
/// not `;`, ends the command, so a `# comment` at its end comments out nothing more.
/// Ctrl-C reaches the whole foreground group: trapped, it leaves this shell running
/// to `BACK_TO_DASHBOARD`, and `command`, which a trap does not follow, gets it as usual.
fn returning(command: Option<&str>) -> String {
    let command = command.unwrap_or(r#""${SHELL:-/bin/sh}" -l"#);
    format!("trap : INT\n{command}\n{BACK_TO_DASHBOARD}")
}

/// The oldest tmux tatou supports, as (major, minor).
const MIN_VERSION: (u32, u32) = (3, 2);

/// The fields `sessions` reads for each pane, tab-separated; empty when an option is
/// not set. Panes rather than sessions, because only a window's activity follows the
/// output of its panes: `session_activity` changes on keyboard input alone.
const PANE_FORMAT: &str = "#{session_id}\t#{session_name}\t#{@tatou_path}\t#{@tatou_status}\t#{@tatou_agent_pane}\t#{session_path}\t#{pane_id}\t#{window_activity}\t#{window_active}\t#{pane_active}\t#{pane_current_command}\t#{pane_tty}\t#{window_id}\t#{window_index}\t#{@tatou_placeholder}\t#{@tatou_shown}";

/// The marks `show` looks for, with each pane's window: the dashboard, the slot's
/// placeholder, and the pane shown in it.
const MARKS_FORMAT: &str =
    "#{pane_id}\t#{window_id}\t#{@tatou_dashboard}\t#{@tatou_placeholder}\t#{@tatou_shown}";

/// What `ps` lists of each process, to tell what runs in the foreground of each
/// pane's terminal; the same columns on Linux and macOS.
const PS_ARGS: [&str; 3] = ["-A", "-o", "pid=,ppid=,pgid=,tpgid=,tty=,args="];

/// Shells: in the foreground of a pane with a command of theirs in the same
/// group (`sh -c 'cd x && claude'`), that command is what runs.
const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "dash", "fish", "ksh", "tcsh", "csh"];

/// A tmux server: every command sent to it is `tmux -u -L <socket> -f <conf> …`.
/// `-u` makes tmux print UTF-8 whatever the locale; without it, tabs and non-ASCII
/// characters in `-F` output come back as `_`.
#[derive(Debug, Clone)]
pub struct Server {
    socket: String,
    conf: String,
}

impl Server {
    /// tatou's own server, on the `tatou` socket.
    pub fn tatou(conf: &Path) -> Server {
        Server::new(SOCKET, conf)
    }

    fn new(socket: &str, conf: &Path) -> Server {
        Server {
            socket: socket.to_string(),
            conf: conf.to_string_lossy().into_owned(),
        }
    }

    /// Runs a tmux command on this server; several can be chained with `";"`.
    pub fn run(&self, args: &[&str]) -> Result<Output> {
        exec::run("tmux", &self.args(args), None)
    }

    /// The full argument list for `args`, for commands that take over the
    /// terminal through `exec::exec_replace` instead of `run`.
    pub fn args<'a>(&'a self, args: &[&'a str]) -> Vec<&'a str> {
        let mut all = vec!["-u", "-L", self.socket.as_str(), "-f", self.conf.as_str()];
        all.extend_from_slice(args);
        all
    }

    /// Applies the regenerated config to the server, if it is running. `previous` is
    /// the config it replaced: sourcing the new one can add and change key bindings
    /// but not remove them, so the ones `previous` made are undone first.
    pub fn reload(&self, previous: Option<&str>) -> Result<()> {
        let undo = previous.map(undo_commands).unwrap_or_default();
        let mut args = Vec::new();
        for command in &undo {
            args.extend(command.iter().map(String::as_str));
            args.push(";");
        }
        args.extend(["source-file", self.conf.as_str()]);
        match self.run(&args) {
            Ok(_) => Ok(()),
            // The next command starts the server with `-f`, which loads the config
            Err(Error::Command { stderr, .. }) if no_server(&stderr) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Starts `command`, an agent, in a new detached session in `path`, named
    /// `name`, or `name-2`, `name-3`, … when taken; without a command, the default
    /// shell. `TATOU=1` in its environment lets the agent's hooks report to tatou;
    /// `@tatou_path` tags it, and `@tatou_agent_pane` the agent's pane, if any.
    /// tmux runs `command` with its default shell, like any config command.
    pub fn new_session(
        &self,
        name: &str,
        path: &Path,
        command: Option<&str>,
    ) -> Result<NewSession> {
        self.start_session(name, path, command, command.is_some())
    }

    /// Starts `command`, which is not an agent (the editor), in a new detached
    /// session as `new_session` does, without an agent pane.
    pub fn new_plain_session(&self, name: &str, path: &Path, command: &str) -> Result<NewSession> {
        self.start_session(name, path, Some(command), false)
    }

    fn start_session(
        &self,
        name: &str,
        path: &Path,
        command: Option<&str>,
        agent: bool,
    ) -> Result<NewSession> {
        let dir = path.to_string_lossy();
        let run = returning(command);
        let mut n = 1;
        let (name, output) = loop {
            let name = match n {
                1 => name.to_string(),
                n => format!("{name}-{n}"),
            };
            let args = [
                "new-session",
                "-d",
                "-s",
                &name,
                "-c",
                &dir,
                "-e",
                "TATOU=1",
                "-P",
                "-F",
                "#{session_id} #{pane_id}",
                &run,
            ];
            match self.run(&args) {
                Ok(output) => break (name, output.stdout),
                Err(Error::Command { stderr, .. }) if stderr.starts_with("duplicate session") => {
                    n += 1;
                }
                Err(err) => return Err(err),
            }
        };
        let Some((id, pane)) = output.trim_end().split_once(' ') else {
            return Err(Error::TmuxOutput { output });
        };
        // By id: `-t =<name>` is read as a pane here. Should this fail, the session
        // still matches its worktree by its start directory
        let mut tags = vec!["set-option", "-t", id, "@tatou_path", &dir];
        if agent {
            tags.extend([";", "set-option", "-t", id, "@tatou_agent_pane", pane]);
        }
        self.run(&tags)?;
        Ok(NewSession {
            id: id.to_string(),
            name,
            pane: pane.to_string(),
        })
    }

    /// Tags `pane` as `session`'s agent pane, for the agent's hooks and restarts.
    pub fn set_agent_pane(&self, session: &str, pane: &str) -> Result<()> {
        self.run(&["set-option", "-t", session, "@tatou_agent_pane", pane])?;
        Ok(())
    }

    /// Restarts `pane` (`%4`) with `command`, killing what runs in it; the pane
    /// keeps its id and folder.
    pub fn respawn_agent(&self, pane: &str, command: &str) -> Result<()> {
        let run = returning(Some(command));
        self.run(&["respawn-pane", "-k", "-t", pane, &run])?;
        Ok(())
    }

    /// Opens a window in `session` (an id) in `path`, running `command`, or the
    /// default shell; it becomes the session's current window. Returns its pane.
    pub fn new_window(&self, session: &str, path: &Path, command: Option<&str>) -> Result<String> {
        let dir = path.to_string_lossy();
        let run = returning(command);
        let args = [
            "new-window",
            "-t",
            session,
            "-c",
            &dir,
            "-P",
            "-F",
            "#{pane_id}",
            &run,
        ];
        let output = self.run(&args)?;
        Ok(output.stdout.trim_end().to_string())
    }

    /// Opens the dashboard's slot right of `dashboard`, its pane: the placeholder,
    /// which a shown pane swaps places with. Returns its id.
    pub fn open_slot(&self, dashboard: &str) -> Result<String> {
        let args = [
            "split-window",
            "-h",
            "-d",
            "-l",
            SLOT_WIDTH,
            "-t",
            dashboard,
            "-P",
            "-F",
            "#{pane_id}",
            PLACEHOLDER,
        ];
        let output = self.run(&args)?;
        let pane = output.stdout.trim_end().to_string();
        let mut marks = vec![
            "set-option",
            "-p",
            "-t",
            &pane,
            "@tatou_placeholder",
            "1",
            ";",
        ];
        marks.extend(["set-option", "-p", "-t", dashboard, "@tatou_dashboard", "1"]);
        self.run(&marks)?;
        Ok(pane)
    }

    /// Shows `pane` in the dashboard's slot, the placeholder taking its place in
    /// its window, and gives it the focus. The pane shown before goes back to its
    /// own window first, so that the placeholder is in the slot again.
    pub fn show(&self, pane: &str) -> Result<()> {
        let (placeholder, shown) = self.mended_slot()?;
        let mut args = Vec::new();
        if let Some(shown) = shown.as_deref() {
            if shown == pane {
                return self.run(&["select-pane", "-t", pane]).map(|_| ());
            }
            args.extend(["swap-pane", "-d", "-s", shown, "-t", &placeholder, ";"]);
            args.extend(["set-option", "-pu", "-t", shown, "@tatou_shown", ";"]);
        }
        args.extend(["swap-pane", "-d", "-s", pane, "-t", &placeholder, ";"]);
        args.extend(["set-option", "-p", "-t", pane, "@tatou_shown", "1", ";"]);
        args.extend(["select-pane", "-t", pane]);
        self.run(&args).map(|_| ())
    }

    /// Puts the slot right again when a shown pane went without sending the
    /// placeholder back (killed, or its program ended without tatou's way home):
    /// the placeholder comes back to the slot, which closes the window it stood in.
    pub fn mend_slot(&self) -> Result<()> {
        self.mended_slot().map(|_| ())
    }

    /// The placeholder and the pane shown in the slot, if any, once mended: the
    /// placeholder back in the slot when nothing is shown there, and the shown mark
    /// off the panes outside it.
    fn mended_slot(&self) -> Result<(String, Option<String>)> {
        let output = self.run(&["list-panes", "-a", "-F", MARKS_FORMAT])?;
        let marks = parse_marks(&output.stdout);
        let (Some((dashboard, window)), Some((placeholder, placeholder_window))) =
            (&marks.dashboard, &marks.placeholder)
        else {
            return Err(Error::NoSlot);
        };
        let mut args = Vec::new();
        let mut in_slot = None;
        for (pane, pane_window) in &marks.shown {
            if pane_window == window && in_slot.is_none() {
                in_slot = Some(pane.clone());
            } else {
                args.extend(["set-option", "-pu", "-t", pane, "@tatou_shown", ";"]);
            }
        }
        if in_slot.is_none() && placeholder_window != window {
            tracing::info!(placeholder, "the placeholder comes back to the slot");
            args.extend(["join-pane", "-h", "-d", "-l", SLOT_WIDTH, "-s", placeholder]);
            args.extend(["-t", dashboard, ";"]);
        }
        if args.pop().is_some() {
            self.run(&args)?;
        }
        Ok((placeholder.clone(), in_slot))
    }

    /// Kills `session` (an id) and everything running in it.
    pub fn kill_session(&self, session: &str) -> Result<()> {
        self.run(&["kill-session", "-t", session])?;
        Ok(())
    }

    /// Every session on the server, sorted by name; none when the server is not running.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        match self.run(&["list-panes", "-a", "-F", PANE_FORMAT]) {
            Ok(output) => Ok(parse_sessions(&output.stdout, &foreground_programs())),
            Err(Error::Command { stderr, .. }) if no_server(&stderr) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }
}

/// A session on tatou's server, from the list of its panes.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `$1`: the target for commands, unlike the name, which can be ambiguous.
    pub id: String,
    pub name: String,
    /// `@tatou_path`, set when tatou creates the session; matched against worktrees first.
    pub tatou_path: Option<PathBuf>,
    /// `@tatou_status`, set by the agent's hooks (DESIGN §8.4).
    pub status: Option<AgentStatus>,
    /// `@tatou_agent_pane`, the pane running the agent: `%1`.
    pub agent_pane: Option<String>,
    /// The session's start directory, matched when `tatou_path` is not set.
    pub path: PathBuf,
    /// When the agent's window last printed, in seconds since the Unix epoch; for a
    /// session without an agent pane, its most recent window.
    pub activity: u64,
    /// What runs in the foreground of its current window's active pane, by the
    /// name it was started under (`claude`, `zsh`, `vim`), as a program's own name
    /// may differ: Claude Code's executable is named after its version.
    pub command: Option<String>,
    /// Its windows, by index.
    pub windows: Vec<Window>,
}

/// A window of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// `@3`: the target for commands.
    pub id: String,
    /// Its active pane, the one to show: `%4`.
    pub pane: String,
    pub index: u32,
    /// The session's current window.
    pub current: bool,
    /// What runs in its active pane, as `Session::command`.
    pub command: Option<String>,
}

/// A session `Server::new_session` started.
#[derive(Debug, PartialEq)]
pub struct NewSession {
    /// `$3`: the target for commands.
    pub id: String,
    pub name: String,
    /// Its pane, running the agent unless it was started without one: `%4`.
    pub pane: String,
}

/// The session name for worktree `name` of the repository named `repo`:
/// `<repo>-<name>`. `.` and `:` become `-`: tmux accepts them in names, but reads
/// them in targets as window and pane separators, so `tmux attach -t` would fail.
pub fn session_name(repo: &str, name: &str) -> String {
    format!("{repo}-{name}").replace(['.', ':'], "-")
}

/// What an agent reports through `@tatou_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Busy,
    Input,
    Idle,
}

/// The marked panes, each as (pane, window), from `MARKS_FORMAT` lines.
#[derive(Debug, Default, PartialEq)]
struct Marks {
    dashboard: Option<(String, String)>,
    placeholder: Option<(String, String)>,
    /// One, or none; more only when something went wrong.
    shown: Vec<(String, String)>,
}

fn parse_marks(text: &str) -> Marks {
    let mut marks = Marks::default();
    for line in text.lines() {
        let [pane, window, dashboard, placeholder, shown] =
            line.split('\t').collect::<Vec<_>>()[..]
        else {
            continue;
        };
        let at = (pane.to_string(), window.to_string());
        if dashboard == "1" {
            marks.dashboard = Some(at);
        } else if placeholder == "1" {
            marks.placeholder = Some(at);
        } else if shown == "1" {
            marks.shown.push(at);
        }
    }
    marks
}

/// tmux's messages when nothing listens on the socket: the server exited, or never started.
fn no_server(stderr: &str) -> bool {
    stderr.starts_with("no server running on ")
        || (stderr.starts_with("error connecting to ")
            && (stderr.ends_with("(No such file or directory)")
                || stderr.ends_with("(Connection refused)")))
}

/// Parses `list-panes -a -F PANE_FORMAT` output into sessions, in the order they
/// first appear. A line in another shape, such as one with a tab or a newline inside
/// a path, is skipped so the other sessions still show.
fn parse_sessions(text: &str, programs: &HashMap<String, String>) -> Vec<Session> {
    let mut lines = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        match parse_pane(line) {
            Some(parsed) => lines.push(parsed),
            None => tracing::warn!(line, "skipping a tmux pane line in an unexpected shape"),
        }
    }
    // Each session with its panes
    let mut sessions: Vec<(Session, Vec<Pane>)> = Vec::new();
    for (session, pane) in in_their_windows(lines) {
        match sessions
            .iter_mut()
            .find(|(known, _)| known.id == session.id)
        {
            Some((_, panes)) => panes.push(pane),
            None => sessions.push((session, vec![pane])),
        }
    }
    let mut result = Vec::new();
    for (mut session, panes) in sessions {
        let agent = panes
            .iter()
            .find(|pane| session.agent_pane.as_ref() == Some(&pane.id));
        session.activity = match agent {
            Some(pane) => pane.activity,
            // Without an agent pane, any window counts
            None => panes.iter().map(|pane| pane.activity).max().unwrap_or(0),
        };
        // Every window has one active pane, which names it
        let mut windows: Vec<Window> = panes
            .iter()
            .filter(|pane| pane.pane_active)
            .map(|pane| {
                let tty = pane.tty.trim_start_matches("/dev/");
                Window {
                    id: pane.window_id.clone(),
                    pane: pane.id.clone(),
                    index: pane.window_index,
                    current: pane.window_active,
                    // tmux's own name for it, when ps had nothing to say
                    command: programs.get(tty).cloned().or_else(|| pane.command.clone()),
                }
            })
            .collect();
        windows.sort_by_key(|window| window.index);
        session.command = windows
            .iter()
            .find(|w| w.current)
            .and_then(|w| w.command.clone());
        session.windows = windows;
        result.push(session);
    }
    result
}

/// The panes, each where it belongs: the one shown in the dashboard's slot in its
/// own window, where its placeholder stands, keeping the activity of the window it
/// prints in; the placeholder nowhere.
fn in_their_windows(lines: Vec<(Session, Pane)>) -> Vec<(Session, Pane)> {
    let home = lines
        .iter()
        .find(|(_, pane)| pane.placeholder)
        .map(|(session, pane)| {
            (
                session.clone(),
                pane.window_id.clone(),
                pane.window_index,
                pane.window_active,
                pane.pane_active,
            )
        });
    let mut result = Vec::new();
    for (session, pane) in lines {
        if pane.placeholder {
            continue;
        }
        match &home {
            Some((home, window_id, window_index, window_active, pane_active)) if pane.shown => {
                let pane = Pane {
                    window_id: window_id.clone(),
                    window_index: *window_index,
                    window_active: *window_active,
                    pane_active: *pane_active,
                    ..pane
                };
                result.push((home.clone(), pane));
            }
            _ => result.push((session, pane)),
        }
    }
    result
}

/// A pane of a session, from one line.
struct Pane {
    id: String,
    /// Its window's activity.
    activity: u64,
    window_id: String,
    window_index: u32,
    /// Its window is the session's current one.
    window_active: bool,
    /// The active pane of its window.
    pane_active: bool,
    command: Option<String>,
    /// Its terminal: `/dev/ttys012`, `/dev/pts/3`.
    tty: String,
    /// The dashboard's placeholder, standing in for the shown pane when there is one.
    placeholder: bool,
    /// Shown in the dashboard's slot.
    shown: bool,
}

/// One line: the pane's session (its activity and command not set yet) and the pane.
fn parse_pane(line: &str) -> Option<(Session, Pane)> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [
        id,
        name,
        tatou_path,
        status,
        agent_pane,
        path,
        pane,
        activity,
        window_active,
        pane_active,
        command,
        tty,
        window_id,
        window_index,
        placeholder,
        shown,
    ] = fields[..]
    else {
        return None;
    };
    let set = |value: &str| (!value.is_empty()).then(|| value.to_string());
    let session = Session {
        id: id.to_string(),
        name: name.to_string(),
        tatou_path: set(tatou_path).map(PathBuf::from),
        status: parse_status(status),
        agent_pane: set(agent_pane),
        path: PathBuf::from(path),
        activity: 0,
        command: None,
        windows: Vec::new(),
    };
    let pane = Pane {
        id: pane.to_string(),
        activity: activity.parse().ok()?,
        window_id: window_id.to_string(),
        window_index: window_index.parse().ok()?,
        window_active: window_active == "1",
        pane_active: pane_active == "1",
        command: set(command),
        tty: tty.to_string(),
        placeholder: placeholder == "1",
        shown: shown == "1",
    };
    Some((session, pane))
}

/// What runs in the foreground of each terminal, by terminal (`ttys012`): see
/// `parse_processes`. None when `ps` cannot be run; tmux's names stand in then.
fn foreground_programs() -> HashMap<String, String> {
    match exec::run("ps", &PS_ARGS, None) {
        Ok(output) => parse_processes(&output.stdout),
        Err(err) => {
            tracing::debug!(%err, "ps failed: tmux names the programs");
            HashMap::new()
        }
    }
}

/// A process from `ps`.
struct Process {
    pid: u32,
    ppid: u32,
    pgid: u32,
    /// The foreground process group of its terminal.
    tpgid: u32,
    tty: String,
    /// The name it was started under, its first argument, without its folder, or
    /// a login shell's leading `-`.
    program: String,
}

/// Parses `ps PS_ARGS` into each terminal's foreground program: the leader of its
/// foreground process group, or when that is a shell running a command of its own
/// in the group, that command. By the name each was started under, which a
/// program's file may not have: Claude Code's executable is named after its
/// version, its link `claude`.
fn parse_processes(text: &str) -> HashMap<String, String> {
    let processes: Vec<Process> = text.lines().filter_map(parse_process).collect();
    let mut programs = HashMap::new();
    for leader in processes.iter().filter(|p| p.pid == p.tpgid) {
        let child = processes
            .iter()
            .find(|p| p.ppid == leader.pid && p.pgid == leader.pgid);
        let program = match child {
            Some(child) if SHELLS.contains(&leader.program.as_str()) => &child.program,
            _ => &leader.program,
        };
        programs.insert(leader.tty.clone(), program.clone());
    }
    programs
}

/// One line of `ps`: five numbers and names, then the command line; none for a
/// process without a terminal (`??` on macOS, `?` on Linux).
fn parse_process(line: &str) -> Option<Process> {
    let mut rest = line;
    let mut field = || {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        rest = &trimmed[end..];
        Some(&trimmed[..end]).filter(|f| !f.is_empty())
    };
    let (pid, ppid, pgid, tpgid) = (field()?, field()?, field()?, field()?);
    let tty = field()?;
    let first = field()?;
    if tty.starts_with('?') || tty == "-" {
        return None;
    }
    let name = Path::new(first.trim_start_matches('-')).file_name()?;
    Some(Process {
        pid: pid.parse().ok()?,
        ppid: ppid.parse().ok()?,
        pgid: pgid.parse().ok()?,
        tpgid: tpgid.parse().ok()?,
        tty: tty.to_string(),
        program: name.to_string_lossy().into_owned(),
    })
}

/// Any value other than the three statuses counts as not set.
fn parse_status(value: &str) -> Option<AgentStatus> {
    match value {
        "busy" => Some(AgentStatus::Busy),
        "input" => Some(AgentStatus::Input),
        "idle" => Some(AgentStatus::Idle),
        "" => None,
        _ => {
            tracing::debug!(value, "ignoring an unknown @tatou_status");
            None
        }
    }
}

/// Where a tatou process runs, according to `$TMUX`.
#[derive(Debug, PartialEq)]
pub enum Nesting {
    /// Not inside tmux.
    Outside,
    /// Inside a session on tatou's server.
    Tatou,
    /// Inside another tmux server, such as the user's own.
    Other,
}

/// Where this process runs, from `$TMUX`.
pub fn nesting() -> Nesting {
    nesting_from(env::var_os("TMUX").as_deref())
}

/// `$TMUX` is `<socket path>,<server pid>,<session index>`; the socket path ends with
/// the `-L` name.
fn nesting_from(var: Option<&OsStr>) -> Nesting {
    let var = match var {
        Some(var) if !var.is_empty() => var.to_string_lossy(),
        _ => return Nesting::Outside,
    };
    // The socket path may contain commas; the pid and index never do
    let socket = var.rsplitn(3, ',').last().unwrap_or(&var);
    if Path::new(socket).file_name() == Some(OsStr::new(SOCKET)) {
        Nesting::Tatou
    } else {
        Nesting::Other
    }
}

/// Checks that the installed tmux is recent enough and returns its version line.
/// `tmux -V` talks to no server, so it is the one command run without `Server`.
pub fn check_version() -> Result<String> {
    let output = exec::run("tmux", &["-V"], None)?;
    let line = output.stdout.trim();
    check(line)?;
    Ok(line.to_string())
}

/// Accepts `line` from `tmux -V` unless it names a version older than `MIN_VERSION`.
fn check(line: &str) -> Result<()> {
    match parse_version(line) {
        Some(version) if version < MIN_VERSION => Err(Error::TmuxVersion {
            found: line.to_string(),
        }),
        Some(_) => Ok(()),
        // Builds from git print no number (`tmux master`) and are newer than any release
        None => {
            tracing::warn!(
                version = line,
                "unknown tmux version, assuming a recent build"
            );
            Ok(())
        }
    }
}

/// `tmux 3.3a` and `tmux next-3.5` give (3, 3) and (3, 5); `tmux master` gives `None`.
fn parse_version(line: &str) -> Option<(u32, u32)> {
    let version = line.strip_prefix("tmux ")?;
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    // The minor version may carry a letter or a suffix: `3a`, `0-rc5`
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    Some((major.parse().ok()?, rest[..digits].parse().ok()?))
}

/// `tmux.conf`, generated next to `config.toml`.
pub fn conf_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("tmux.conf"))
}

/// Writes `tmux.conf` from `config`, replacing the previous one. Called at every
/// launch, so the file always follows the config.
pub fn write_conf(config: &Config) -> Result<()> {
    let dir = config::config_dir()?;
    // The files tmux itself would load, in its own order
    let user_confs = [
        config::home_dir()?.join(".tmux.conf"),
        config::config_home()?.join("tmux/tmux.conf"),
    ];
    let local_conf = dir.join("tmux.local.conf");
    let text = conf(&config.tmux, &user_confs, &local_conf, &dashboard_command());
    // A running tmux server never sources a half-written file
    config::write_atomic(&conf_path()?, &text)
}

/// The shell command the dashboard session runs: this binary, by absolute path so
/// the session works whatever its `$PATH`, then `CLOSE_SLOT`, however it ended.
pub fn dashboard_command() -> String {
    let exe = match env::current_exe() {
        Ok(exe) => exe.to_string_lossy().into_owned(),
        Err(_) => "tatou".to_string(),
    };
    format!("{} --dashboard; {CLOSE_SLOT}", exec::shell_quote(&exe))
}

/// The contents of tatou's `tmux.conf`: the user's own tmux config when inherited,
/// the settings agents need, tatou's bindings, and last the user's `tmux.local.conf`.
/// `dashboard` is what the back key starts when the dashboard's session is gone.
fn conf(tmux: &config::Tmux, user_confs: &[PathBuf], local_conf: &Path, dashboard: &str) -> String {
    let mut lines = vec![
        "# Generated by tatou from config.toml at every launch: edits here are lost.".to_string(),
        "# Put your own settings in tmux.local.conf, next to this file.".to_string(),
    ];
    if tmux.inherit_user_conf {
        lines.push(String::new());
        for path in user_confs {
            lines.push(format!("source-file -q {}", quote_path(path)));
        }
    }

    lines.push(String::new());
    lines.push("# Settings agents need".to_string());
    lines.extend(
        [
            "set-option -s escape-time 10",
            "set-option -s extended-keys on",
            "set-option -s focus-events on",
            "set-option -s set-clipboard on",
            "set-option -as terminal-features ',*:RGB'",
            "set-option -g mouse on",
            // allow-passthrough exists from tmux 3.3; -q skips it on 3.2
            "set-option -gq allow-passthrough on",
        ]
        .map(String::from),
    );
    lines.push(format!("set-option -g history-limit {HISTORY_LIMIT}"));
    // A session that ends moves its clients to another one, never out of tmux
    lines.push("set-option -g detach-on-destroy off".to_string());

    lines.push(String::new());
    lines.push("# The status bar: every window as <session>-<index>, `kdbg-master-0`".to_string());
    lines.push("set-option -g status-left ''".to_string());
    // tmux's own window format, with the session's name for the window's: tmux
    // names windows after their program's file, and Claude's is its version
    let window = "#S-#I#{?window_flags,#{window_flags}, }";
    for option in ["window-status-format", "window-status-current-format"] {
        lines.push(format!("set-option -g {option} {}", quote(window)));
    }

    lines.push(String::new());
    lines.push("# tatou's bindings".to_string());
    if let Some(prefix) = &tmux.prefix {
        lines.push(format!("set-option -g prefix {}", quote(prefix)));
        lines.push(format!("bind-key {} send-prefix", quote(prefix)));
    }
    // The dashboard, with the focus on it rather than on its slot (the leftmost
    // pane), started again when its session is gone (`q` ends it). The format finds
    // a session named exactly `tatou`; `{ }` keep the command's quoting as is. Not
    // `new-session -A`: it attaches only a terminal's client
    let exists = format!("#{{S:#{{?#{{==:#{{session_name}},{DASHBOARD}}},1,}}}}");
    let switch =
        format!("switch-client -t '={DASHBOARD}' ; select-pane -t '={DASHBOARD}:.{{left}}'");
    let start = format!(
        "new-session -d -s {DASHBOARD} -n {DASHBOARD} {}",
        quote(dashboard)
    );
    lines.push(format!(
        "bind-key -n {} if-shell -F '{exists}' {{ {switch} }} {{ {start} ; {switch} }}",
        quote(&tmux.back)
    ));

    lines.push(String::new());
    lines.push(format!("source-file -q {}", quote_path(local_conf)));
    lines.join("\n") + "\n"
}

/// Quotes `value` for tmux's config parser, which reads single quotes like `sh`:
/// nothing inside them is special, and `'\''` puts a quote in the middle.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn quote_path(path: &Path) -> String {
    quote(&path.to_string_lossy())
}

/// Reads back the word `quote` wrote at the start of `text`.
fn unquote(text: &str) -> Option<String> {
    let mut rest = text.strip_prefix('\'')?;
    let mut word = String::new();
    loop {
        let end = rest.find('\'')?;
        word.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        match rest.strip_prefix(r"\''") {
            Some(after) => {
                word.push('\'');
                rest = after;
            }
            None => return Some(word),
        }
    }
}

/// The tmux commands that undo the key bindings and the prefix set by `conf`, a
/// `tmux.conf` tatou generated. Its other settings are the same in every version.
fn undo_commands(conf: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    for line in conf.lines() {
        let command = if let Some(rest) = line.strip_prefix("bind-key -n ") {
            unquote(rest).map(|key| vec!["unbind-key".into(), "-n".into(), key])
        } else if let Some(rest) = line.strip_prefix("bind-key ") {
            unquote(rest).map(|key| vec!["unbind-key".into(), key])
        } else if line.starts_with("set-option -g prefix ") {
            Some(vec!["set-option".into(), "-gu".into(), "prefix".into()])
        } else {
            None
        };
        commands.extend(command);
    }
    commands
}

/// A tmux server for tests, on a socket of its own and killed when dropped.
#[cfg(test)]
pub mod testing {
    use std::ops::Deref;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Server;
    use crate::exec;

    pub struct TestServer(Server);

    impl TestServer {
        /// A server that loads no config, or `None` when tmux is not installed, so
        /// that the test can return early. The server starts with the first command.
        pub fn new() -> Option<TestServer> {
            TestServer::with_conf(Path::new("/dev/null"))
        }

        /// A server that loads `conf` when it starts.
        pub fn with_conf(conf: &Path) -> Option<TestServer> {
            if exec::run("tmux", &["-V"], None).is_err() {
                eprintln!("tmux is not installed: skipping");
                return None;
            }
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let socket = format!("tatou-test-{}-{n}", std::process::id());
            Some(TestServer(Server::new(&socket, conf)))
        }
    }

    impl Deref for TestServer {
        type Target = Server;

        fn deref(&self) -> &Server {
            &self.0
        }
    }

    /// A control-mode client of `server` on `session`, as a terminal would be;
    /// it detaches when dropped.
    pub struct Client(std::process::Child);

    impl Client {
        pub fn attach(server: &Server, session: &str) -> Client {
            let args = server.args(&["-C", "attach", "-t", session]);
            let child = std::process::Command::new("tmux")
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            Client(child)
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The session the server's one client shows, once it has one, within 5 s
    /// once `until` holds.
    pub fn client_on(server: &Server, until: impl Fn(&str) -> bool) -> String {
        let mut session = String::new();
        for _ in 0..50 {
            let output = server.run(&["list-clients", "-F", "#{client_session}"]);
            session = output
                .map(|o| o.stdout.trim().to_string())
                .unwrap_or_default();
            if until(&session) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        session
    }

    impl Drop for TestServer {
        /// Kills the server and removes its socket, which tmux leaves behind, also
        /// when the server exited by itself with its last session.
        fn drop(&mut self) {
            // Fails when the server already exited, which is fine
            let _ = self.0.run(&["kill-server"]);
            if let Some(socket) = socket_file(&self.0.socket) {
                let _ = std::fs::remove_file(socket);
            }
        }
    }

    /// Where tmux puts the socket named `name`: `$TMUX_TMPDIR`, or `/tmp`, then
    /// `tmux-<uid>/<name>`.
    fn socket_file(name: &str) -> Option<PathBuf> {
        let uid = exec::run("id", &["-u"], None).ok()?.stdout;
        let dir = std::env::var_os("TMUX_TMPDIR").filter(|dir| !dir.is_empty());
        let dir = dir.map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        Some(dir.join(format!("tmux-{}", uid.trim())).join(name))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::testing::{Client, TestServer, client_on};
    use super::*;

    fn user_confs() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/home/me/.tmux.conf"),
            PathBuf::from("/home/me/.config/tmux/tmux.conf"),
        ]
    }

    /// What the back key starts in these tests.
    const DASHBOARD_COMMAND: &str = "'/usr/bin/tatou' --dashboard";

    fn generate(tmux: &config::Tmux) -> String {
        conf(
            tmux,
            &user_confs(),
            Path::new("/home/me/.config/tatou/tmux.local.conf"),
            DASHBOARD_COMMAND,
        )
    }

    #[test]
    fn the_defaults() {
        let expected = "\
# Generated by tatou from config.toml at every launch: edits here are lost.
# Put your own settings in tmux.local.conf, next to this file.

# Settings agents need
set-option -s escape-time 10
set-option -s extended-keys on
set-option -s focus-events on
set-option -s set-clipboard on
set-option -as terminal-features ',*:RGB'
set-option -g mouse on
set-option -gq allow-passthrough on
set-option -g history-limit 50000
set-option -g detach-on-destroy off

# The status bar: every window as <session>-<index>, `kdbg-master-0`
set-option -g status-left ''
set-option -g window-status-format '#S-#I#{?window_flags,#{window_flags}, }'
set-option -g window-status-current-format '#S-#I#{?window_flags,#{window_flags}, }'

# tatou's bindings
bind-key -n 'M-d' if-shell -F '#{S:#{?#{==:#{session_name},tatou},1,}}' { switch-client -t '=tatou' ; select-pane -t '=tatou:.{left}' } { new-session -d -s tatou -n tatou ''\\''/usr/bin/tatou'\\'' --dashboard' ; switch-client -t '=tatou' ; select-pane -t '=tatou:.{left}' }

source-file -q '/home/me/.config/tatou/tmux.local.conf'
";
        assert_eq!(generate(&config::Tmux::default()), expected);
    }

    #[test]
    fn a_custom_back_key() {
        let tmux = config::Tmux {
            back: "C-q".into(),
            ..Default::default()
        };
        let text = generate(&tmux);
        let binding = r"bind-key -n 'C-q' if-shell -F '#{S:#{?#{==:#{session_name},tatou},1,}}' { switch-client -t '=tatou' ; select-pane -t '=tatou:.{left}' } { new-session -d -s tatou -n tatou ''\''/usr/bin/tatou'\'' --dashboard' ; switch-client -t '=tatou' ; select-pane -t '=tatou:.{left}' }";
        assert!(text.contains(&format!("\n{binding}\n")), "{text}");
        assert!(!text.contains("'M-d'"));
    }

    #[test]
    fn a_prefix_is_set_and_sent_on_double_press() {
        let tmux = config::Tmux {
            prefix: Some("C-a".into()),
            ..Default::default()
        };
        let text = generate(&tmux);
        assert!(text.contains("\nset-option -g prefix 'C-a'\nbind-key 'C-a' send-prefix\n"));
    }

    #[test]
    fn the_user_conf_comes_before_tatous_settings() {
        let tmux = config::Tmux {
            inherit_user_conf: true,
            ..Default::default()
        };
        let text = generate(&tmux);
        let position = |line: &str| text.find(line).expect(line);
        let home = position("source-file -q '/home/me/.tmux.conf'\n");
        let xdg = position("source-file -q '/home/me/.config/tmux/tmux.conf'\n");
        let settings = position("set-option -s escape-time 10");
        assert!(home < xdg && xdg < settings);
        assert!(!generate(&config::Tmux::default()).contains(".tmux.conf"));
    }

    #[test]
    fn the_local_conf_is_the_last_line() {
        let tmux = config::Tmux {
            prefix: Some("C-a".into()),
            inherit_user_conf: true,
            ..Default::default()
        };
        let text = generate(&tmux);
        assert_eq!(
            text.lines().last(),
            Some("source-file -q '/home/me/.config/tatou/tmux.local.conf'")
        );
    }

    #[test]
    fn values_are_quoted() {
        assert_eq!(quote("M-d"), "'M-d'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        let tmux = config::Tmux {
            back: "M-d ; kill-server".into(),
            ..Default::default()
        };
        let text = conf(
            &tmux,
            &[],
            Path::new("/it's here/tmux.local.conf"),
            DASHBOARD_COMMAND,
        );
        assert!(text.contains("bind-key -n 'M-d ; kill-server' if-shell"));
        assert!(text.contains(r"source-file -q '/it'\''s here/tmux.local.conf'"));
    }

    /// tmux ignores errors in the file given with `-f`, so the generated conf is
    /// checked with `source-file`, which fails on the first invalid line.
    #[test]
    fn tmux_accepts_the_generated_conf() {
        let Some(server) = TestServer::new() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // Spaces and a quote in the path exercise the quoting
        let dir = dir.path().join("it's a dir");
        let user_conf = dir.join("user.conf");
        let local_conf = dir.join("tmux.local.conf");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&user_conf, "set-option -g @tatou_user on\n").unwrap();
        fs::write(&local_conf, "set-option -g @tatou_local on\n").unwrap();
        let tmux = config::Tmux {
            prefix: Some("C-a".into()),
            inherit_user_conf: true,
            ..Default::default()
        };
        let path = dir.join("tmux.conf");
        config::write_atomic(
            &path,
            // The real dashboard command, with its quotes and formats
            &conf(&tmux, &[user_conf], &local_conf, &dashboard_command()),
        )
        .unwrap();

        let mut args = vec!["start-server", ";", "source-file", path.to_str().unwrap()];
        for option in ["@tatou_user", "@tatou_local", "prefix"] {
            args.extend([";", "show-options", "-gv", option]);
        }
        args.extend([";", "show-options", "-gwv", "window-status-format"]);
        let result = server.run(&args);
        assert_eq!(
            result.unwrap().stdout,
            "on\non\nC-a\n#S-#I#{?window_flags,#{window_flags}, }\n"
        );
    }

    #[test]
    fn parses_tmux_versions() {
        let cases = [
            ("tmux 3.2", Some((3, 2))),
            ("tmux 3.3a", Some((3, 3))),
            ("tmux 3.4", Some((3, 4))),
            ("tmux next-3.5", Some((3, 5))),
            ("tmux 3.10", Some((3, 10))),
            ("tmux 3.0-rc5", Some((3, 0))),
            ("tmux 2.9a", Some((2, 9))),
            ("tmux master", None),
            ("tmux", None),
            ("", None),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_version(line), expected, "{line:?}");
        }
    }

    #[test]
    fn requires_tmux_3_2() {
        for line in [
            "tmux 3.2",
            "tmux 3.3a",
            "tmux next-3.6",
            "tmux 4.0",
            "tmux master",
        ] {
            assert!(check(line).is_ok(), "{line:?}");
        }
        for line in ["tmux 3.1c", "tmux 2.9a", "tmux 3.0-rc5"] {
            let err = check(line).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("tatou needs tmux 3.2 or later, found `{line}`")
            );
        }
    }

    #[test]
    fn the_server_arguments_come_first() {
        let server = Server::tatou(Path::new("/c/tatou/tmux.conf"));
        assert_eq!(
            server.args(&["attach", "-t", "=tatou"]).join(" "),
            "-u -L tatou -f /c/tatou/tmux.conf attach -t =tatou"
        );
    }

    #[test]
    fn a_test_server_runs_on_its_own_socket_and_is_killed_on_drop() {
        let Some(server) = TestServer::new() else {
            return;
        };
        let other = TestServer::new().unwrap();
        assert_ne!(server.args(&[]), other.args(&[]));

        server.run(&["new-session", "-d", "-s", "probe"]).unwrap();
        let sessions = server.run(&["list-sessions", "-F", "#{session_name}"]);
        assert_eq!(sessions.unwrap().stdout, "probe\n");
        // Nothing runs on the other socket
        assert!(other.run(&["list-sessions"]).is_err());

        let socket = Server::clone(&server);
        drop(server);
        assert!(socket.run(&["list-sessions"]).is_err());
    }

    #[test]
    fn check_version_accepts_the_installed_tmux() {
        if TestServer::new().is_none() {
            return;
        }
        let line = check_version().unwrap();
        assert!(line.starts_with("tmux "), "{line:?}");
    }

    #[test]
    fn parses_sessions_from_their_panes() {
        let text = include_str!("../tests/fixtures/tmux/panes.txt");
        let programs = parse_processes(include_str!("../tests/fixtures/ps/processes.txt"));
        let plain = |id: &str, name: &str, path: &str, activity, command: &str| Session {
            id: id.into(),
            name: name.into(),
            tatou_path: None,
            status: None,
            agent_pane: None,
            path: path.into(),
            activity,
            command: Some(command.into()),
            windows: Vec::new(),
        };
        let created = |id, name, path: &str, pane: &str, activity, command| Session {
            tatou_path: Some(path.into()),
            agent_pane: Some(pane.into()),
            ..plain(id, name, path, activity, command)
        };
        let docs = "/home/me/code/api.worktrees/docs";
        let login = "/home/me/code/my-app.worktrees/fix login";
        let expected = [
            Session {
                status: Some(AgentStatus::Input),
                ..created("$2", "api-docs", docs, "%2", 1792793302, "claude")
            },
            // Its agent's window, not the shell window opened later, and what runs
            // in the active pane of its current window: vim in a split of the shell's.
            // api-docs' agent, tmux's `2.1.310`, runs as claude
            created("$1", "my-app-fix login", login, "%1", 1792793302, "vim"),
            // No agent pane: its most recent window
            plain(
                "$3",
                "scratch",
                "/home/me/notes and ideas",
                1792793307,
                "less",
            ),
            plain("$0", "tatou", "/home/me", 1792793302, "tatou"),
        ];
        let parsed = parse_sessions(text, &programs);
        let without_windows: Vec<_> = parsed
            .iter()
            .map(|s| Session {
                windows: Vec::new(),
                ..s.clone()
            })
            .collect();
        assert_eq!(without_windows, expected);

        // Each session's windows, by index, each named by its active pane
        let window = |id: &str, pane: &str, index, current, command: &str| Window {
            id: id.into(),
            pane: pane.into(),
            index,
            current,
            command: Some(command.into()),
        };
        let windows: Vec<_> = parsed.into_iter().map(|s| s.windows).collect();
        assert_eq!(
            windows,
            [
                vec![window("@2", "%2", 0, true, "claude")],
                // The agent's window, then the shell's, split, with vim active
                vec![
                    window("@1", "%1", 0, false, "claude"),
                    window("@4", "%5", 1, true, "vim")
                ],
                vec![
                    window("@3", "%3", 0, false, "claude"),
                    window("@6", "%6", 1, true, "less")
                ],
                vec![window("@0", "%0", 0, true, "tatou")],
            ]
        );
        // Without ps, tmux's names
        let names: Vec<_> = parse_sessions(text, &HashMap::new())
            .into_iter()
            .map(|s| s.command.unwrap())
            .collect();
        assert_eq!(names, ["2.1.310", "vim", "less", "tatou"]);
    }

    #[test]
    fn a_closed_agent_pane_falls_back_to_the_most_recent_window() {
        let text = "\
$1\tapp\t/a\t\t%9\t/a\t%1\t100\t0\t1\tzsh\t\t@1\t0\t\t
$1\tapp\t/a\t\t%9\t/a\t%2\t200\t1\t1\tzsh\t\t@2\t1\t\t
";
        assert_eq!(parse_sessions(text, &HashMap::new())[0].activity, 200);
    }

    #[test]
    fn each_terminal_runs_its_foreground_program_by_the_name_it_was_started_under() {
        let programs = parse_processes(include_str!("../tests/fixtures/ps/processes.txt"));
        let program = |tty: &str| programs.get(tty).map(String::as_str);
        // A link named claude, to a file named after its version
        assert_eq!(
            program("ttys002"),
            Some("claude"),
            "not its helper in the group"
        );
        assert_eq!(program("ttys001"), Some("claude"));
        // A login shell's job in the foreground, and the login shell at its prompt
        assert_eq!(program("ttys005"), Some("vim"));
        assert_eq!(program("ttys004"), Some("zsh"));
        // A shell running a command of its own in the same group
        assert_eq!(program("ttys003"), Some("claude"));
        assert_eq!(program("ttys006"), Some("less"));
        assert_eq!(program("ttys000"), Some("tatou"));
        // Seven terminals: the processes without one, as launchd, name none
        assert_eq!(programs.len(), 7);
    }

    /// `read`, once `until` holds of it, or after 5 s. A program tatou starts runs
    /// in its pane only once the wrapping `sh` has started it: before, the pane
    /// runs `tmux` (not yet exec'd), then `sh`.
    fn eventually<T>(read: impl Fn() -> T, until: impl Fn(&T) -> bool) -> T {
        let mut value = read();
        for _ in 0..50 {
            if until(&value) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            value = read();
        }
        value
    }

    #[test]
    fn a_program_started_through_a_link_goes_by_the_links_name() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        // As Claude Code: tmux names the file, ps the link it was started through
        let link = dir.path().join("claude");
        // To `sh`, not `sleep`: Ubuntu's coreutils are one program that runs the
        // tool its link is named after, and knows no `claude`. `; exit` keeps `sh`
        // from exec'ing `sleep` in its place
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
        let command = format!("{} -c 'sleep 1000; exit'", link.display());
        server
            .new_session("app", dir.path(), Some(&command))
            .unwrap();
        let program = eventually(
            || server.sessions().unwrap()[0].command.clone(),
            |program| program.as_deref() == Some("claude"),
        );
        assert_eq!(program.as_deref(), Some("claude"));
    }

    #[test]
    fn no_output_is_no_sessions() {
        assert_eq!(parse_sessions("", &HashMap::new()), []);
    }

    #[test]
    fn lines_in_another_shape_are_skipped() {
        let text = "\
$0\ttatou\t\t\t\t/home/me\t%0\t1792788783\t1\t1\ttatou\t\t@0\t0\t\t
$1\ttab\t\t\t\t/home/me/a\tb\t%1\t1792788783\t1\t1\tzsh\t\t@2\t1\t\t
$2\tactivity\t\t\t\t/home/me\t%2\tnot a number\t1\t1\tzsh\t\t@2\t1\t\t
$3\tshort\t/home/me
";
        let names: Vec<_> = parse_sessions(text, &HashMap::new())
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["tatou"]);
    }

    #[test]
    fn unknown_statuses_count_as_not_set() {
        assert_eq!(parse_status("idle"), Some(AgentStatus::Idle));
        assert_eq!(parse_status(""), None);
        assert_eq!(parse_status("Busy"), None);
        assert_eq!(parse_status("thinking"), None);
    }

    #[test]
    fn recognises_no_server_running() {
        let messages = [
            "no server running on /tmp/tmux-0/tatou",
            "error connecting to /tmp/tmux-0/tatou (No such file or directory)",
            "error connecting to /private/tmp/tmux-501/tatou (Connection refused)",
        ];
        for stderr in messages {
            assert!(no_server(stderr), "{stderr:?}");
        }
        assert!(!no_server(
            "error connecting to /tmp/tmux-0/tatou (Permission denied)"
        ));
        assert!(!no_server("unknown option -- F"));
    }

    /// A test server whose default shell is `/bin/sh`, so that commands never run
    /// the user's shell and its startup files.
    fn sh_server() -> Option<(TestServer, tempfile::TempDir)> {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("tmux.conf");
        fs::write(&conf, "set-option -g default-shell /bin/sh\n").unwrap();
        Some((TestServer::with_conf(&conf)?, dir))
    }

    /// `format` for `target`, from `display-message`.
    fn show(server: &Server, target: &str, format: &str) -> String {
        let output = server.run(&["display-message", "-p", "-t", target, format]);
        output.unwrap().stdout.trim_end().to_string()
    }

    #[test]
    fn session_names_have_no_target_separators() {
        assert_eq!(session_name("my-app", "fix-login"), "my-app-fix-login");
        assert_eq!(session_name("my.app", "v1.2:x"), "my-app-v1-2-x");
        assert_eq!(session_name("app", "feature/login"), "app-feature/login");
    }

    #[test]
    fn new_sessions_are_tagged_for_tatou() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let path = dir.path().canonicalize().unwrap();
        let session = server
            .new_session("app-fix", &path, Some("sleep 1000"))
            .unwrap();
        assert_eq!(session.name, "app-fix");
        assert!(session.id.starts_with('$') && session.pane.starts_with('%'));

        let sessions = server.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id);
        assert_eq!(sessions[0].tatou_path.as_deref(), Some(path.as_path()));
        assert_eq!(
            sessions[0].agent_pane.as_deref(),
            Some(session.pane.as_str())
        );
        assert_eq!(sessions[0].path, path);
        let env = server.run(&["show-environment", "-t", &session.id, "TATOU"]);
        assert_eq!(env.unwrap().stdout, "TATOU=1\n");
        // The command, then the way back to the dashboard
        let command = show(&server, &session.pane, "#{pane_start_command}");
        assert!(
            command.contains("sleep 1000") && command.contains("switch-client"),
            "{command}"
        );
        assert!(command.contains("switch-client"), "{command}");
    }

    #[test]
    fn a_plain_session_has_no_agent_pane() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let path = dir.path().canonicalize().unwrap();
        let session = server
            .new_plain_session("app-fix", &path, "sleep 1000")
            .unwrap();
        let sessions = server.sessions().unwrap();
        assert_eq!(sessions[0].id, session.id);
        assert_eq!(sessions[0].tatou_path.as_deref(), Some(path.as_path()));
        assert_eq!(sessions[0].agent_pane, None);
        let command = show(&server, &session.pane, "#{pane_start_command}");
        assert!(
            command.contains("sleep 1000") && command.contains("switch-client"),
            "{command}"
        );
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let names: Vec<_> = (0..3)
            .map(|_| {
                server
                    .new_session("app-fix", dir.path(), Some("sleep 1000"))
                    .unwrap()
                    .name
            })
            .collect();
        assert_eq!(names, ["app-fix", "app-fix-2", "app-fix-3"]);
    }

    #[test]
    fn respawns_the_agent_in_its_pane() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let session = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        let pid = show(&server, &session.pane, "#{pane_pid}");
        server.respawn_agent(&session.pane, "sleep 2000").unwrap();
        let after = show(&server, &session.pane, "#{pane_start_command}");
        assert!(
            after.contains("sleep 2000") && after.contains("switch-client"),
            "{after}"
        );
        assert_ne!(show(&server, &session.pane, "#{pane_pid}"), pid);
        let path = show(&server, &session.pane, "#{pane_current_path}");
        assert_eq!(Path::new(&path), dir.path().canonicalize().unwrap());
    }

    #[test]
    fn started_commands_go_back_to_the_dashboard_after() {
        let agent = returning(Some("claude # with a comment"));
        assert_eq!(
            agent,
            format!("trap : INT\nclaude # with a comment\n{BACK_TO_DASHBOARD}")
        );
        let shell = returning(None);
        assert_eq!(
            shell,
            format!("trap : INT\n\"${{SHELL:-/bin/sh}}\" -l\n{BACK_TO_DASHBOARD}")
        );
    }

    #[test]
    fn the_back_key_starts_the_dashboard_again_when_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tmux.conf");
        let tmux = config::Tmux::default();
        let text = conf(&tmux, &[], &dir.path().join("none"), "sleep 1000");
        fs::write(
            &path,
            format!("{text}set-option -g default-shell /bin/sh\n"),
        )
        .unwrap();
        let Some(server) = TestServer::with_conf(&path) else {
            return;
        };
        let work = server
            .new_session("work", dir.path(), Some("sleep 1000"))
            .unwrap();
        let _client = Client::attach(&server, &work.id);
        assert_eq!(client_on(&server, |s| s == "work"), "work");
        let client = server
            .run(&["list-clients", "-F", "#{client_name}"])
            .unwrap();
        let back = || {
            let client = client.stdout.trim();
            server
                .run(&["send-keys", "-K", "-c", client, "M-d"])
                .unwrap();
        };
        // No dashboard session: the back key starts it
        back();
        assert_eq!(client_on(&server, |s| s == DASHBOARD), DASHBOARD);
        // Back to work, and again: the same dashboard, no second one
        let to_work = ["switch-client", "-c", client.stdout.trim(), "-t", &work.id];
        server.run(&to_work).unwrap();
        assert_eq!(client_on(&server, |s| s == "work"), "work");
        back();
        assert_eq!(client_on(&server, |s| s == DASHBOARD), DASHBOARD);
        let names: Vec<_> = server
            .sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, [DASHBOARD, "work"]);
    }

    #[test]
    fn a_program_ending_in_the_window_one_looks_at_goes_back_to_the_dashboard() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        server
            .run(&["new-session", "-d", "-s", DASHBOARD, "sleep 1000"])
            .unwrap();
        let work = server
            .new_session("work", dir.path(), Some("sleep 1"))
            .unwrap();
        let _client = Client::attach(&server, &work.id);
        assert_eq!(client_on(&server, |s| s == "work"), "work");
        // sleep ends after a second, then the client goes back
        assert_eq!(client_on(&server, |s| s == DASHBOARD), DASHBOARD);
    }

    #[test]
    fn a_program_ending_in_another_window_moves_nobody() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        server
            .run(&["new-session", "-d", "-s", DASHBOARD, "sleep 1000"])
            .unwrap();
        let work = server
            .new_session("work", dir.path(), Some("sleep 1"))
            .unwrap();
        // The client looks at a shell in a second window
        server.new_window(&work.id, dir.path(), None).unwrap();
        let _client = Client::attach(&server, &work.id);
        assert_eq!(client_on(&server, |s| s == "work"), "work");
        // Long after the agent's window closed: still there
        std::thread::sleep(std::time::Duration::from_millis(2500));
        assert_eq!(client_on(&server, |_| true), "work");
        let windows = server.sessions().unwrap();
        let work = windows.iter().find(|s| s.name == "work").unwrap();
        assert_eq!(work.windows.len(), 1, "the agent's window closed");
    }

    #[test]
    fn lists_each_window_with_its_pane() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let session = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        server.new_window(&session.id, dir.path(), None).unwrap();
        let windows = eventually(
            || server.sessions().unwrap()[0].windows.clone(),
            |windows| windows[0].command.as_deref() == Some("sleep"),
        );
        let current: Vec<_> = windows.iter().map(|w| (w.index, w.current)).collect();
        assert_eq!(current, [(0, false), (1, true)], "the one opened last");
        assert_eq!(windows[0].command.as_deref(), Some("sleep"));
        assert_eq!(windows[0].pane, session.pane);
    }

    /// A dashboard stand-in running `command` in a session named `tatou`, and its
    /// slot: (dashboard pane, placeholder).
    fn dashboard(server: &Server, dir: &Path, command: &str) -> (String, String) {
        let args = [
            "new-session",
            "-d",
            "-s",
            DASHBOARD,
            "-x",
            "120",
            "-y",
            "30",
        ];
        let output = server.run(
            &[
                &args[..],
                &[
                    "-c",
                    &dir.to_string_lossy(),
                    "-P",
                    "-F",
                    "#{pane_id}",
                    command,
                ],
            ]
            .concat(),
        );
        let pane = output.unwrap().stdout.trim_end().to_string();
        let placeholder = server.open_slot(&pane).unwrap();
        (pane, placeholder)
    }

    /// The panes of `target`'s window, in order.
    fn panes_of(server: &Server, target: &str) -> Vec<String> {
        let output = server.run(&["list-panes", "-t", target, "-F", "#{pane_id}"]);
        output.unwrap().stdout.lines().map(String::from).collect()
    }

    #[test]
    fn a_shown_pane_swaps_places_with_the_placeholder() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let (board, placeholder) = dashboard(&server, dir.path(), "sleep 1000");
        assert_eq!(
            panes_of(&server, &board),
            [board.clone(), placeholder.clone()]
        );
        let app = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        let web = server
            .new_session("web", dir.path(), Some("sleep 1000"))
            .unwrap();

        server.show(&app.pane).unwrap();
        assert_eq!(panes_of(&server, &board), [board.clone(), app.pane.clone()]);
        assert_eq!(
            panes_of(&server, &app.id),
            std::slice::from_ref(&placeholder)
        );
        assert_eq!(show(&server, "tatou:", "#{pane_id}"), app.pane, "the focus");
        // Listed in its own session, the placeholder nowhere
        let sessions = server.sessions().unwrap();
        let listed = |name: &str| {
            let session = sessions.iter().find(|s| s.name == name).unwrap();
            session
                .windows
                .iter()
                .map(|w| w.pane.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(listed("app"), std::slice::from_ref(&app.pane));
        assert!(!listed(DASHBOARD).contains(&app.pane), "only in its own");

        // Another: the first goes home
        server.show(&web.pane).unwrap();
        assert_eq!(panes_of(&server, &board), [board.clone(), web.pane.clone()]);
        assert_eq!(panes_of(&server, &app.id), std::slice::from_ref(&app.pane));
        assert_eq!(panes_of(&server, &web.id), [placeholder]);
        assert_eq!(show(&server, &app.pane, "#{@tatou_shown}"), "");
    }

    #[test]
    fn a_shown_program_that_ends_goes_home_first() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let (board, placeholder) = dashboard(&server, dir.path(), "sleep 1000");
        let app = server
            .new_session("app", dir.path(), Some("sleep 1"))
            .unwrap();
        server.show(&app.pane).unwrap();
        // It ends in its own window, which closes with its session
        let names = eventually(
            || {
                let sessions = server.sessions().unwrap();
                sessions.into_iter().map(|s| s.name).collect::<Vec<_>>()
            },
            |names| names.len() == 1,
        );
        assert_eq!(names, [DASHBOARD]);
        assert_eq!(panes_of(&server, &board), [board.clone(), placeholder]);
        assert_eq!(show(&server, "tatou:", "#{pane_id}"), board, "the focus");
    }

    #[test]
    fn the_dashboard_ending_sends_the_shown_pane_home_and_closes_its_slot() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let (_, _) = dashboard(&server, dir.path(), &format!("sleep 1; {CLOSE_SLOT}"));
        let app = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        server.show(&app.pane).unwrap();
        let names = eventually(
            || {
                let sessions = server.sessions().unwrap();
                sessions.into_iter().map(|s| s.name).collect::<Vec<_>>()
            },
            |names| names.len() == 1,
        );
        assert_eq!(names, ["app"]);
        assert_eq!(panes_of(&server, &app.id), std::slice::from_ref(&app.pane));
        assert_eq!(show(&server, &app.pane, "#{@tatou_shown}"), "");
    }

    #[test]
    fn a_shown_program_ended_by_ctrl_c_still_goes_home() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let (board, placeholder) = dashboard(&server, dir.path(), "sleep 1000");
        let app = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        server.show(&app.pane).unwrap();
        server.run(&["send-keys", "-t", &app.pane, "C-c"]).unwrap();
        let panes = eventually(
            || panes_of(&server, &board),
            |panes| panes.len() == 2 && panes[1] == placeholder,
        );
        assert_eq!(panes, [board, placeholder]);
    }

    #[test]
    fn the_placeholder_left_behind_comes_back() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let (board, placeholder) = dashboard(&server, dir.path(), "sleep 1000");
        let app = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        server.show(&app.pane).unwrap();
        // Killed: no way home ran, the placeholder stays in app's window
        server.run(&["kill-pane", "-t", &app.pane]).unwrap();
        assert_eq!(
            panes_of(&server, &app.id),
            std::slice::from_ref(&placeholder)
        );

        // The next show mends the slot first, closing app's session
        let web = server
            .new_session("web", dir.path(), Some("sleep 1000"))
            .unwrap();
        server.show(&web.pane).unwrap();
        assert_eq!(panes_of(&server, &board), [board.clone(), web.pane.clone()]);
        assert_eq!(
            panes_of(&server, &web.id),
            std::slice::from_ref(&placeholder)
        );
        let names: Vec<_> = server
            .sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, [DASHBOARD, "web"]);

        // And so does the refresh
        server.run(&["kill-pane", "-t", &web.pane]).unwrap();
        server.mend_slot().unwrap();
        assert_eq!(panes_of(&server, &board), [board, placeholder]);
    }

    #[test]
    fn marks_by_pane() {
        let text = "%0\t@0\t1\t\t\n%1\t@0\t\t1\t\n%2\t@1\t\t\t1\n%3\t@2\t\t\t\nshort\n";
        let marks = parse_marks(text);
        let at = |pane: &str, window: &str| Some((pane.to_string(), window.to_string()));
        assert_eq!(marks.dashboard, at("%0", "@0"));
        assert_eq!(marks.placeholder, at("%1", "@0"));
        assert_eq!(marks.shown, [("%2".to_string(), "@1".to_string())]);
    }

    #[test]
    fn showing_without_a_slot_says_so() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let app = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        assert!(matches!(server.show(&app.pane), Err(Error::NoSlot)));
    }

    #[test]
    fn the_shown_pane_is_listed_where_its_placeholder_stands() {
        let text = "\
$0\ttatou\t\t\t\t/h\t%0\t500\t1\t0\ttatou\t\t@0\t0\t\t
$0\ttatou\t\t\t\t/h\t%1\t500\t1\t1\tclaude\t\t@0\t0\t\t1
$1\tapp\t/a\t\t%1\t/a\t%2\t100\t1\t1\tsh\t\t@1\t0\t1\t
";
        let sessions = parse_sessions(text, &HashMap::new());
        let names: Vec<_> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, [DASHBOARD, "app"]);
        assert!(
            sessions[0].windows.iter().all(|w| w.pane != "%1"),
            "only in its own"
        );
        let app = &sessions[1];
        assert_eq!(app.windows[0].pane, "%1");
        assert_eq!(app.windows[0].id, "@1");
        assert_eq!(app.command.as_deref(), Some("claude"));
        assert_eq!(app.activity, 500, "where it prints");
    }

    #[test]
    fn opens_windows_and_kills_sessions() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let session = server
            .new_session("app", dir.path(), Some("sleep 1000"))
            .unwrap();
        let other = tempfile::tempdir().unwrap();
        let shell = server.new_window(&session.id, other.path(), None).unwrap();
        let tool = server
            .new_window(&session.id, other.path(), Some("sleep 3000"))
            .unwrap();
        assert!(shell.starts_with('%') && tool.starts_with('%'));
        let windows = show(&server, &session.id, "#{session_windows}");
        assert_eq!(windows, "3");
        let path = show(&server, &shell, "#{pane_current_path}");
        assert_eq!(Path::new(&path), other.path().canonicalize().unwrap());
        // The last one opened is the session's current window
        assert_eq!(show(&server, &session.id, "#{pane_id}"), tool);

        server.kill_session(&session.id).unwrap();
        assert_eq!(server.sessions().unwrap(), []);
        assert!(server.kill_session(&session.id).is_err(), "gone");
    }

    #[test]
    fn reads_sessions_back_from_tmux() {
        let Some(server) = TestServer::new() else {
            return;
        };
        assert_eq!(server.sessions().unwrap(), [], "no server yet");

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let worktree = home.join("fix login");
        fs::create_dir(&worktree).unwrap();
        let (home, worktree) = (home.to_str().unwrap(), worktree.to_str().unwrap());
        // `-P -F` prints the new session's id and pane, as tatou will when it creates one
        let new_session = |name: &str, path: &str| {
            let args = ["new-session", "-d", "-s", name, "-c", path, "-P", "-F"];
            let output = server.run(&[&args[..], &["#{session_id} #{pane_id}"]].concat());
            let output = output.unwrap().stdout;
            let (id, pane) = output.trim_end().split_once(' ').unwrap();
            (id.to_string(), pane.to_string())
        };
        let (app, pane) = new_session("app-fix login", worktree);
        let (plain, _) = new_session("plain", home);
        for (option, value) in [
            ("@tatou_path", worktree),
            ("@tatou_status", "busy"),
            ("@tatou_agent_pane", &pane),
        ] {
            server
                .run(&["set-option", "-t", &app, option, value])
                .unwrap();
        }

        let sessions = server.sessions().unwrap();
        let expected = [
            Session {
                id: app,
                name: "app-fix login".into(),
                tatou_path: Some(worktree.into()),
                status: Some(AgentStatus::Busy),
                agent_pane: Some(pane),
                path: worktree.into(),
                activity: sessions[0].activity,
                command: sessions[0].command.clone(),
                windows: sessions[0].windows.clone(),
            },
            Session {
                id: plain,
                name: "plain".into(),
                tatou_path: None,
                status: None,
                agent_pane: None,
                path: home.into(),
                activity: sessions[1].activity,
                command: sessions[1].command.clone(),
                windows: sessions[1].windows.clone(),
            },
        ];
        assert_eq!(sessions, expected);
        assert!(sessions.iter().all(|session| session.activity > 0));
        // The test server's default shell, whichever it is
        assert!(sessions.iter().all(|session| session.command.is_some()));
    }

    #[test]
    fn tells_where_tmux_says_we_run() {
        let nesting = |var: Option<&str>| nesting_from(var.map(OsStr::new));
        assert_eq!(nesting(None), Nesting::Outside);
        assert_eq!(nesting(Some("")), Nesting::Outside);
        assert_eq!(
            nesting(Some("/private/tmp/tmux-501/tatou,4242,0")),
            Nesting::Tatou
        );
        assert_eq!(nesting(Some("/tmp/a,b/tmux-0/tatou,1,2")), Nesting::Tatou);
        assert_eq!(
            nesting(Some("/tmp/tmux-1000/default,4242,3")),
            Nesting::Other
        );
        assert_eq!(
            nesting(Some("/tmp/tmux-0/tatou-test-1-0,1,0")),
            Nesting::Other
        );
    }

    #[test]
    fn unquote_reads_back_what_quote_wrote() {
        for value in ["M-d", "it's", "''", "a ; b", "", r"back\slash"] {
            let line = format!("{} switch-client", quote(value));
            assert_eq!(unquote(&line).as_deref(), Some(value), "{value:?}");
        }
        assert_eq!(unquote("M-d switch-client"), None);
        assert_eq!(unquote("'unterminated"), None);
    }

    #[test]
    fn undoes_the_bindings_and_prefix_of_a_generated_conf() {
        let tmux = config::Tmux {
            back: "M-'".into(),
            prefix: Some("C-a".into()),
            ..Default::default()
        };
        let commands = undo_commands(&generate(&tmux));
        let expected = [
            vec!["set-option", "-gu", "prefix"],
            vec!["unbind-key", "C-a"],
            vec!["unbind-key", "-n", "M-'"],
        ];
        assert_eq!(commands, expected);
        assert!(undo_commands(&generate(&config::Tmux::default())).len() == 1);
    }

    #[test]
    fn reload_applies_a_new_conf_and_drops_the_old_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tmux.conf");
        let old = conf(
            &config::Tmux::default(),
            &[],
            &dir.path().join("none"),
            DASHBOARD_COMMAND,
        );
        fs::write(&path, &old).unwrap();
        let Some(server) = TestServer::with_conf(&path) else {
            return;
        };
        assert!(server.reload(None).is_ok(), "no server running is fine");
        server.run(&["new-session", "-d", "-s", "probe"]).unwrap();
        let root_keys = || server.run(&["list-keys", "-T", "root"]).unwrap().stdout;
        assert!(root_keys().contains("M-d"));

        let tmux = config::Tmux {
            back: "M-b".into(),
            prefix: Some("C-a".into()),
            ..Default::default()
        };
        fs::write(
            &path,
            conf(&tmux, &[], &dir.path().join("none"), DASHBOARD_COMMAND),
        )
        .unwrap();
        server.reload(Some(&old)).unwrap();
        let keys = root_keys();
        assert!(keys.contains("M-b") && !keys.contains("M-d"), "{keys}");
        let prefix = server.run(&["show-options", "-gv", "prefix"]).unwrap();
        assert_eq!(prefix.stdout, "C-a\n");

        // Back to the defaults: the prefix returns to tmux's own
        let current = fs::read_to_string(&path).unwrap();
        fs::write(&path, &old).unwrap();
        server.reload(Some(&current)).unwrap();
        let prefix = server.run(&["show-options", "-gv", "prefix"]).unwrap();
        assert_eq!(prefix.stdout, "C-b\n");
        let keys = root_keys();
        assert!(keys.contains("M-d") && !keys.contains("M-b"), "{keys}");
    }
}
