//! tatou's own tmux server: its generated `tmux.conf`, sessions, options,
//! switch/popup/window/send-keys, and output parsing.

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

/// The oldest tmux tatou supports, as (major, minor).
const MIN_VERSION: (u32, u32) = (3, 2);

/// The fields `sessions` reads for each pane, tab-separated; empty when an option is
/// not set. Panes rather than sessions, because only a window's activity follows the
/// output of its panes: `session_activity` changes on keyboard input alone.
const PANE_FORMAT: &str = "#{session_id}\t#{session_name}\t#{@tatou_path}\t#{@tatou_status}\t#{@tatou_agent_pane}\t#{session_path}\t#{pane_id}\t#{window_activity}";

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

    /// Shows `session` (an id: `$3`) in the client this process runs in: the one
    /// showing the dashboard, found by tmux from `$TMUX`.
    pub fn switch_client(&self, session: &str) -> Result<()> {
        self.run(&["switch-client", "-t", session]).map(|_| ())
    }

    /// Starts `command` in a new detached session in `path`, named `name`, or
    /// `name-2`, `name-3`, … when taken. `TATOU=1` in its environment lets the
    /// agent's hooks report to tatou; `@tatou_path` and `@tatou_agent_pane` tag it.
    /// tmux runs `command` with its default shell, like any config command.
    // Not called yet: starting an agent uses it (#21, #22)
    #[allow(dead_code)]
    pub fn new_session(&self, name: &str, path: &Path, command: &str) -> Result<NewSession> {
        let dir = path.to_string_lossy();
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
                command,
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
        let tag_path = ["set-option", "-t", id, "@tatou_path", &dir];
        let tag_pane = ["set-option", "-t", id, "@tatou_agent_pane", pane];
        self.run(&[&tag_path[..], &[";"], &tag_pane[..]].concat())?;
        Ok(NewSession {
            id: id.to_string(),
            name,
            pane: pane.to_string(),
        })
    }

    /// Restarts `pane` (`%4`) with `command`, killing what runs in it; the pane
    /// keeps its id and folder.
    // Not called yet: restarting an agent uses it (#22)
    #[allow(dead_code)]
    pub fn respawn_agent(&self, pane: &str, command: &str) -> Result<()> {
        self.run(&["respawn-pane", "-k", "-t", pane, command])?;
        Ok(())
    }

    /// Opens a window in `session` (an id) in `path`, running `command`, or the
    /// default shell; it becomes the session's current window. Returns its pane.
    // Not called yet: opening a shell uses it (#23)
    #[allow(dead_code)]
    pub fn new_window(&self, session: &str, path: &Path, command: Option<&str>) -> Result<String> {
        let dir = path.to_string_lossy();
        let mut args = vec![
            "new-window",
            "-t",
            session,
            "-c",
            &dir,
            "-P",
            "-F",
            "#{pane_id}",
        ];
        args.extend(command);
        let output = self.run(&args)?;
        Ok(output.stdout.trim_end().to_string())
    }

    /// Kills `session` (an id) and everything running in it.
    // Not called yet: removing a worktree uses it (#24)
    #[allow(dead_code)]
    pub fn kill_session(&self, session: &str) -> Result<()> {
        self.run(&["kill-session", "-t", session])?;
        Ok(())
    }

    /// Every session on the server, sorted by name; none when the server is not running.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        match self.run(&["list-panes", "-a", "-F", PANE_FORMAT]) {
            Ok(output) => Ok(parse_sessions(&output.stdout)),
            Err(Error::Command { stderr, .. }) if no_server(&stderr) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }
}

/// A session on tatou's server, from the list of its panes.
#[derive(Debug, PartialEq)]
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
}

/// A session `Server::new_session` started.
// Not read yet: starting an agent uses it (#21, #22)
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub struct NewSession {
    /// `$3`: the target for commands.
    pub id: String,
    pub name: String,
    /// The pane running the agent: `%4`.
    pub pane: String,
}

/// The session name for worktree `name` of the repository named `repo`:
/// `<repo>-<name>`. `.` and `:` become `-`: tmux accepts them in names, but reads
/// them in targets as window and pane separators, so `tmux attach -t` would fail.
// Not called yet: starting an agent uses it (#21, #22)
#[allow(dead_code)]
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
fn parse_sessions(text: &str) -> Vec<Session> {
    // Each session with its panes' ids and window activity
    let mut sessions: Vec<(Session, Vec<(String, u64)>)> = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let Some((session, pane)) = parse_pane(line) else {
            tracing::warn!(line, "skipping a tmux pane line in an unexpected shape");
            continue;
        };
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
            .find(|(id, _)| session.agent_pane.as_ref() == Some(id));
        session.activity = match agent {
            Some((_, activity)) => *activity,
            // Without an agent pane, any window counts
            None => panes
                .iter()
                .map(|(_, activity)| *activity)
                .max()
                .unwrap_or(0),
        };
        result.push(session);
    }
    result
}

/// One line: the pane's session (its activity not set yet) and the pane's id and
/// window activity.
fn parse_pane(line: &str) -> Option<(Session, (String, u64))> {
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
    };
    Some((session, (pane.to_string(), activity.parse().ok()?)))
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
    let text = conf(&config.tmux, &user_confs, &dir.join("tmux.local.conf"));
    // A running tmux server never sources a half-written file
    config::write_atomic(&conf_path()?, &text)
}

/// The contents of tatou's `tmux.conf`: the user's own tmux config when inherited,
/// the settings agents need, tatou's bindings, and last the user's `tmux.local.conf`.
fn conf(tmux: &config::Tmux, user_confs: &[PathBuf], local_conf: &Path) -> String {
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

    lines.push(String::new());
    lines.push("# tatou's bindings".to_string());
    if let Some(prefix) = &tmux.prefix {
        lines.push(format!("set-option -g prefix {}", quote(prefix)));
        lines.push(format!("bind-key {} send-prefix", quote(prefix)));
    }
    // `=` makes the target an exact match, so a session named `tatou-…` is never picked
    lines.push(format!(
        "bind-key -n {} switch-client -t '=tatou'",
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

    use super::testing::TestServer;
    use super::*;

    fn user_confs() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/home/me/.tmux.conf"),
            PathBuf::from("/home/me/.config/tmux/tmux.conf"),
        ]
    }

    fn generate(tmux: &config::Tmux) -> String {
        conf(
            tmux,
            &user_confs(),
            Path::new("/home/me/.config/tatou/tmux.local.conf"),
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

# tatou's bindings
bind-key -n 'M-d' switch-client -t '=tatou'

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
        assert!(text.contains("\nbind-key -n 'C-q' switch-client -t '=tatou'\n"));
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
        let text = conf(&tmux, &[], Path::new("/it's here/tmux.local.conf"));
        assert!(text.contains("bind-key -n 'M-d ; kill-server' switch-client"));
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
        config::write_atomic(&path, &conf(&tmux, &[user_conf], &local_conf)).unwrap();

        let mut args = vec!["start-server", ";", "source-file", path.to_str().unwrap()];
        for option in ["@tatou_user", "@tatou_local", "prefix"] {
            args.extend([";", "show-options", "-gv", option]);
        }
        let result = server.run(&args);
        assert_eq!(result.unwrap().stdout, "on\non\nC-a\n");
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
        let plain = |id: &str, name: &str, path: &str, activity| Session {
            id: id.into(),
            name: name.into(),
            tatou_path: None,
            status: None,
            agent_pane: None,
            path: path.into(),
            activity,
        };
        let created = |id, name, path: &str, pane: &str, activity| Session {
            tatou_path: Some(path.into()),
            agent_pane: Some(pane.into()),
            ..plain(id, name, path, activity)
        };
        let docs = "/home/me/code/api.worktrees/docs";
        let login = "/home/me/code/my-app.worktrees/fix login";
        let expected = [
            Session {
                status: Some(AgentStatus::Input),
                ..created("$2", "api-docs", docs, "%2", 1792793302)
            },
            // Its agent's window, not the shell window opened later
            created("$1", "my-app-fix login", login, "%1", 1792793302),
            // No agent pane: its most recent window
            plain("$3", "scratch", "/home/me/notes and ideas", 1792793307),
            plain("$0", "tatou", "/home/me", 1792793302),
        ];
        assert_eq!(parse_sessions(text), expected);
    }

    #[test]
    fn a_closed_agent_pane_falls_back_to_the_most_recent_window() {
        let text = "\
$1\tapp\t/a\t\t%9\t/a\t%1\t100
$1\tapp\t/a\t\t%9\t/a\t%2\t200
";
        assert_eq!(parse_sessions(text)[0].activity, 200);
    }

    #[test]
    fn no_output_is_no_sessions() {
        assert_eq!(parse_sessions(""), []);
    }

    #[test]
    fn lines_in_another_shape_are_skipped() {
        let text = "\
$0\ttatou\t\t\t\t/home/me\t%0\t1792788783
$1\ttab\t\t\t\t/home/me/a\tb\t%1\t1792788783
$2\tactivity\t\t\t\t/home/me\t%2\tnot a number
$3\tshort\t/home/me
";
        let names: Vec<_> = parse_sessions(text).into_iter().map(|s| s.name).collect();
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

    #[test]
    fn switching_without_a_client_reports_tmux_error() {
        let Some(server) = TestServer::new() else {
            return;
        };
        let output = server.run(&["new-session", "-d", "-P", "-F", "#{session_id}"]);
        let id = output.unwrap().stdout.trim_end().to_string();
        // No client is attached in a test; jumping for real is the PR's manual check
        match server.switch_client(&id) {
            Err(Error::Command { stderr, .. }) => assert!(!stderr.is_empty()),
            other => panic!("{other:?}"),
        }
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
        let session = server.new_session("app-fix", &path, "sleep 1000").unwrap();
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
        let command = show(&server, &session.pane, "#{pane_start_command}");
        assert_eq!(command, "\"sleep 1000\"");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let names: Vec<_> = (0..3)
            .map(|_| {
                server
                    .new_session("app-fix", dir.path(), "sleep 1000")
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
        let session = server.new_session("app", dir.path(), "sleep 1000").unwrap();
        let pid = show(&server, &session.pane, "#{pane_pid}");
        server.respawn_agent(&session.pane, "sleep 2000").unwrap();
        let after = show(&server, &session.pane, "#{pane_start_command}");
        assert_eq!(after, "\"sleep 2000\"");
        assert_ne!(show(&server, &session.pane, "#{pane_pid}"), pid);
        let path = show(&server, &session.pane, "#{pane_current_path}");
        assert_eq!(Path::new(&path), dir.path().canonicalize().unwrap());
    }

    #[test]
    fn opens_windows_and_kills_sessions() {
        let Some((server, dir)) = sh_server() else {
            return;
        };
        let session = server.new_session("app", dir.path(), "sleep 1000").unwrap();
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
            },
            Session {
                id: plain,
                name: "plain".into(),
                tatou_path: None,
                status: None,
                agent_pane: None,
                path: home.into(),
                activity: sessions[1].activity,
            },
        ];
        assert_eq!(sessions, expected);
        assert!(sessions.iter().all(|session| session.activity > 0));
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
        let old = conf(&config::Tmux::default(), &[], &dir.path().join("none"));
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
        fs::write(&path, conf(&tmux, &[], &dir.path().join("none"))).unwrap();
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
