//! The commands behind the dashboard's actions, each run on a thread of its own.
//! They report what happens as events over a channel, so the UI never waits.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use crate::client::Client;
use crate::config;
use crate::error::Error;
use crate::exec;
use crate::gh;
use crate::git;
use crate::platform;

/// An action the UI asks for.
#[derive(Debug, PartialEq)]
pub enum Task {
    NewWorktree(NewWorktree),
    /// A shell, or the editor, in a worktree, shown in the dashboard.
    Program(Program),
    RemoveWorktree(RemoveWorktree),
    /// Ends a window, or a session with all its windows; the worktree stays.
    End(End),
    /// Sets the theme in the config at `config`.
    SaveTheme {
        config: PathBuf,
        name: String,
    },
    /// Puts text selected in the shown terminal in the clipboard.
    Copy(String),
    /// Opens a pull request of the project's repository in the browser.
    OpenPr {
        project: PathBuf,
        number: u32,
    },
}

/// What `d` ends in the sessions' pane.
#[derive(Debug, Clone, PartialEq)]
pub enum End {
    /// A terminal: `%3`.
    Window(String),
    /// A session: `$2`.
    Session(String),
}

/// A linked worktree to remove, and its session to end once it is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveWorktree {
    pub name: String,
    pub project: PathBuf,
    pub path: PathBuf,
    /// The session's id.
    pub session: Option<String>,
    /// Passes `--force`; only after git refused once and the user confirmed.
    pub force: bool,
}

/// What `e` runs in a worktree: the user's editor, on the worktree's folder.
/// `$EDITOR` is left unquoted so that its arguments (`code -w`) stay arguments.
pub const EDITOR: &str = "${EDITOR:-vi} .";

/// A program to open in a new terminal of a worktree's session: a shell, or the editor.
#[derive(Debug, PartialEq)]
pub struct Program {
    pub name: String,
    pub path: PathBuf,
    /// The worktree's session id; `None`: a new session with just the program.
    pub session: Option<String>,
    /// The name for a new session.
    pub session_name: String,
    /// Run by `sh -c`; `None`: the user's shell.
    pub command: Option<String>,
}

/// A worktree to create, with its project's `on_create` and a shell to open in it.
#[derive(Debug, PartialEq)]
pub struct NewWorktree {
    pub project: PathBuf,
    /// The new branch, also the worktree's name.
    pub name: String,
    /// `None`: from the branch checked out in the main worktree.
    pub base: Option<String>,
    pub path: PathBuf,
    pub on_create: Option<String>,
    pub session: String,
}

/// What happened, for the UI.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// A command failed; its message is for the hint bar.
    Failed(String),
    /// A step of a longer action started.
    Progress(String),
    /// git refused the new worktree; nothing was created.
    Refused(String),
    /// The worktree exists, at this path with symlinks resolved.
    Added { project: PathBuf, path: PathBuf },
    /// `on_create` failed; the worktree stays and the shell still opens.
    OnCreateFailed(String),
    /// A terminal was started, to show: a shell's, the editor's, a custom action's.
    Started(String),
    /// git refused to remove the worktree, saying why; nothing was changed. The
    /// removal to run, with `force`, if the user confirms.
    RemoveRefused { remove: RemoveWorktree, why: String },
    /// The worktree is gone, and its session.
    Removed { project: PathBuf, name: String },
    /// A window or a session is ending: its programs were told to.
    Ended,
    /// A command's whole output, to read: e.g. git refusing even a forced removal.
    Error { title: String, output: String },
    /// The theme is in `config.toml`.
    ThemeSaved(String),
    /// The theme could not be written there; why.
    ThemeNotSaved(String),
    /// This many characters are in the clipboard.
    Copied(usize),
    /// The browser was asked to show this pull request.
    OpenedPr(u32),
    /// No clipboard tool could copy the text: the dashboard's terminal is asked to.
    NoClipboardTool(String),
}

/// Runs `task` on a thread of its own, sending what happens to `events`.
pub fn start(task: Task, client: &Client, events: &Sender<Event>) {
    let client = client.clone();
    let sender = events.clone();
    let started = thread::Builder::new()
        .name("task".to_string())
        .spawn(move || run(task, &client, &sender));
    if let Err(err) = started {
        tracing::error!(%err, "cannot start a task thread");
        let _ = events.send(Event::Failed(format!("cannot start a thread: {err}")));
    }
}

fn run(task: Task, client: &Client, events: &Sender<Event>) {
    // A send fails only when the dashboard has ended: nobody to tell
    let send = |event| {
        let _ = events.send(event);
    };
    match task {
        Task::NewWorktree(new) => new_worktree(&new, client, &send),
        Task::Program(program) => {
            let started = client.spawn(
                program.session.as_deref(),
                &program.session_name,
                &program.path,
                program.command.as_deref(),
            );
            send(match started {
                Ok(terminal) => Event::Started(terminal),
                Err(err) => Event::Failed(format!("{}: {err}", program.name)),
            });
        }
        Task::RemoveWorktree(remove) => send(remove_worktree(remove, client)),
        Task::End(end) => {
            let ended = match &end {
                End::Window(terminal) => client.kill(terminal),
                End::Session(session) => client.kill_session(session),
            };
            send(match ended {
                Ok(()) => Event::Ended,
                Err(err) => Event::Failed(err.to_string()),
            });
        }
        Task::Copy(text) => send(match platform::copy(&text) {
            Some(_) => Event::Copied(text.chars().count()),
            None => Event::NoClipboardTool(text),
        }),
        Task::OpenPr { project, number } => send(match gh::open(&project, number) {
            Ok(()) => Event::OpenedPr(number),
            Err(err) => Event::Failed(format!("#{number}: {}", message(err))),
        }),
        Task::SaveTheme { config, name } => {
            send(match config::save_theme(&config, &name) {
                Ok(()) => Event::ThemeSaved(name),
                Err(err) => Event::ThemeNotSaved(err.to_string()),
            });
        }
    }
}

/// `git worktree remove`, then, only once it is gone, the session: a refusal
/// leaves both as they were.
fn remove_worktree(remove: RemoveWorktree, client: &Client) -> Event {
    let name = remove.name.clone();
    match git::remove(&remove.project, &remove.path, remove.force) {
        Ok(()) => {}
        Err(Error::Command { stderr, .. }) if !remove.force => {
            tracing::info!(name, why = stderr, "removal refused");
            return Event::RemoveRefused {
                remove,
                why: stderr,
            };
        }
        Err(err) => {
            tracing::warn!(name, %err, "removal failed");
            let output = message(err);
            let title = format!("{name} not removed");
            return Event::Error { title, output };
        }
    }
    tracing::info!(name, path = ?remove.path, force = remove.force, "worktree removed");
    if let Some(session) = &remove.session
        && let Err(err) = client.kill_session(session)
    {
        tracing::warn!(name, %err, "session not ended");
        return Event::Failed(format!("{name} removed, but not its session: {err}"));
    }
    Event::Removed {
        project: remove.project,
        name,
    }
}

/// `git worktree add`, then `on_create` in the new worktree, then a session with
/// a shell. A failure of `on_create` is reported and the session still starts.
fn new_worktree(new: &NewWorktree, client: &Client, send: &dyn Fn(Event)) {
    let name = &new.name;
    send(Event::Progress(format!("{name}: creating the worktree")));
    if let Err(err) = git::add(&new.project, name, new.base.as_deref(), &new.path) {
        tracing::info!(name, %err, "worktree refused");
        send(Event::Refused(message(err)));
        return;
    }
    tracing::info!(name, path = ?new.path, "worktree added");
    let path = new.path.canonicalize().unwrap_or_else(|_| new.path.clone());
    send(Event::Added {
        project: new.project.clone(),
        path: path.clone(),
    });

    if let Some(script) = &new.on_create {
        send(Event::Progress(format!("{name}: running on_create")));
        if let Err(err) = exec::run_sh(script, Some(&path)) {
            tracing::warn!(name, %err, "on_create failed");
            send(Event::OnCreateFailed(err.to_string()));
        }
    }

    send(Event::Progress(format!("{name}: opening a shell")));
    match client.spawn(None, &new.session, &path, None) {
        Ok(terminal) => {
            tracing::info!(name, terminal, "shell started");
            send(Event::Started(terminal));
        }
        Err(err) => {
            tracing::warn!(name, %err, "shell not started");
            send(Event::Failed(format!("{name}: {err}")));
        }
    }
}

/// What git said, without the command line around it; other errors as they are.
fn message(err: Error) -> String {
    match err {
        Error::Command { stderr, .. } => stderr,
        err => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::client::testing::{daemon, sessions_become};

    /// A repository at `<dir>/app` with one commit, `dir` canonical.
    fn repository() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("app");
        fs::create_dir(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "tatou")
                .env("GIT_AUTHOR_EMAIL", "tatou@example.com")
                .env("GIT_COMMITTER_NAME", "tatou")
                .env("GIT_COMMITTER_EMAIL", "tatou@example.com")
                .status()
                .unwrap();
            assert!(status.success());
        }
        (tmp, repo)
    }

    fn request(repo: &Path, name: &str, on_create: Option<&str>) -> NewWorktree {
        NewWorktree {
            project: repo.to_path_buf(),
            name: name.to_string(),
            base: None,
            path: git::worktree_path(repo, name, None),
            on_create: on_create.map(String::from),
            session: format!("app-{name}"),
        }
    }

    /// Runs `task` to its end and returns what it reported.
    fn events(task: Task, client: &Client) -> Vec<Event> {
        let (sender, received) = mpsc::channel();
        run(task, client, &sender);
        drop(sender);
        received.iter().collect()
    }

    /// The terminal the last event says was started.
    fn started(events: &[Event]) -> String {
        match events.last() {
            Some(Event::Started(terminal)) => terminal.clone(),
            other => panic!("not started: {other:?} in {events:?}"),
        }
    }

    #[test]
    fn creates_the_worktree_runs_on_create_and_opens_a_shell() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        let new = request(&repo, "fix", Some("touch created"));
        let path = new.path.clone();
        let events = events(Task::NewWorktree(new), &client);
        let terminal = started(&events);
        assert_eq!(
            events[..4],
            [
                Event::Progress("fix: creating the worktree".into()),
                Event::Added {
                    project: repo.clone(),
                    path: path.clone(),
                },
                Event::Progress("fix: running on_create".into()),
                Event::Progress("fix: opening a shell".into()),
            ]
        );
        assert!(
            path.join("created").exists(),
            "on_create ran in the worktree"
        );
        let sessions = client.sessions().unwrap();
        assert_eq!(sessions[0].name, "app-fix");
        assert_eq!(sessions[0].path, path);
        assert_eq!(sessions[0].windows[0].id, terminal);
    }

    #[test]
    fn a_refused_worktree_starts_nothing() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        // The branch exists already
        let events = events(Task::NewWorktree(request(&repo, "main", None)), &client);
        let [Event::Progress(_), Event::Refused(error)] = &events[..] else {
            panic!("{events:?}");
        };
        assert!(error.contains("already exists"), "{error}");
        assert!(!error.contains("git -C"), "git's message only: {error}");
        assert_eq!(client.sessions().unwrap(), []);
    }

    #[test]
    fn a_failed_on_create_keeps_the_worktree_and_opens_a_shell() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        let new = request(&repo, "fix", Some("echo oops >&2; exit 3"));
        let path = new.path.clone();
        let events = events(Task::NewWorktree(new), &client);
        let failed = events.iter().find_map(|event| match event {
            Event::OnCreateFailed(output) => Some(output.clone()),
            _ => None,
        });
        assert!(
            failed.is_some_and(|output| output.contains("oops")),
            "{events:?}"
        );
        started(&events);
        assert!(path.exists());
    }

    fn program(path: &Path, session: Option<&str>, command: Option<&str>) -> Task {
        Task::Program(Program {
            name: "fix".into(),
            path: path.to_path_buf(),
            session: session.map(String::from),
            session_name: "app-fix".into(),
            command: command.map(String::from),
        })
    }

    #[test]
    fn ends_a_window_or_a_whole_session_and_never_the_worktree() {
        let (dir, client) = daemon();
        let first = client
            .spawn(None, "app-fix", dir.path(), Some("sleep 1000"))
            .unwrap();
        let session = client.sessions().unwrap()[0].id.clone();
        let second = client
            .spawn(Some(&session), "app-fix", dir.path(), Some("sleep 1001"))
            .unwrap();
        client
            .spawn(None, "notes", dir.path(), Some("sleep 1002"))
            .unwrap();

        let ended = events(Task::End(End::Window(first)), &client);
        assert_eq!(ended, [Event::Ended]);
        let since = Instant::now();
        loop {
            let sessions = client.sessions().unwrap();
            let windows: Vec<_> = sessions[0].windows.iter().map(|w| w.id.clone()).collect();
            if windows == [second.clone()] {
                break;
            }
            assert!(since.elapsed() < Duration::from_secs(5), "{windows:?}");
            thread::sleep(Duration::from_millis(20));
        }

        let ended = events(Task::End(End::Session(session)), &client);
        assert_eq!(ended, [Event::Ended]);
        sessions_become(&client, 1);
        assert!(dir.path().exists());
        let gone = events_of_end(&client, "$99");
        assert!(
            matches!(&gone[..], [Event::Failed(m)] if m == "no session $99"),
            "{gone:?}"
        );
    }

    fn events_of_end(client: &Client, session: &str) -> Vec<Event> {
        events(Task::End(End::Session(session.into())), client)
    }

    #[test]
    fn a_shell_or_the_editor_opens_in_the_worktrees_session_or_a_new_one() {
        let (dir, client) = daemon();
        let shell = started(&events(program(dir.path(), None, None), &client));
        let sessions = client.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "app-fix");

        let session = sessions[0].id.clone();
        let task = program(dir.path(), Some(&session), Some(EDITOR));
        let editor = started(&events(task, &client));
        let sessions = client.sessions().unwrap();
        let windows: Vec<_> = sessions[0].windows.iter().map(|w| w.id.clone()).collect();
        assert_eq!(windows, [shell, editor]);
    }

    /// git in `repo`, without the user's config; its output.
    fn git_out(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// The linked worktree `fix` of `repo`, with a session in it.
    fn linked(repo: &Path, client: &Client) -> (PathBuf, String) {
        let path = git::worktree_path(repo, "fix", None);
        git::add(repo, "fix", None, &path).unwrap();
        client
            .spawn(None, "app-fix", &path, Some("sleep 1000"))
            .unwrap();
        let session = client.sessions().unwrap()[0].id.clone();
        (path, session)
    }

    fn remove(repo: &Path, path: &Path, session: &str, force: bool) -> RemoveWorktree {
        RemoveWorktree {
            name: "fix".into(),
            project: repo.to_path_buf(),
            path: path.to_path_buf(),
            session: Some(session.into()),
            force,
        }
    }

    #[test]
    fn removes_the_worktree_then_its_session_and_keeps_the_branch() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        let (path, session) = linked(&repo, &client);
        let events = events(
            Task::RemoveWorktree(remove(&repo, &path, &session, false)),
            &client,
        );
        let removed = Event::Removed {
            project: repo.clone(),
            name: "fix".into(),
        };
        assert_eq!(events, [removed]);
        assert!(!path.exists());
        sessions_become(&client, 0);
        assert_eq!(git_out(&repo, &["branch", "--list", "fix"]).trim(), "fix");
    }

    #[test]
    fn a_dirty_worktree_is_refused_until_forced() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        let (path, session) = linked(&repo, &client);
        fs::write(path.join("notes.md"), "draft").unwrap();

        let asked = remove(&repo, &path, &session, false);
        let refused = events(Task::RemoveWorktree(asked.clone()), &client);
        let [Event::RemoveRefused { remove: again, why }] = &refused[..] else {
            panic!("{refused:?}");
        };
        assert_eq!(again, &asked, "the same removal, to force");
        assert!(
            why.contains("contains modified or untracked files"),
            "{why}"
        );
        assert!(path.join("notes.md").exists(), "nothing changed");
        assert_eq!(
            client.sessions().unwrap().len(),
            1,
            "its session still runs"
        );

        let forced = RemoveWorktree {
            force: true,
            ..asked
        };
        let events = events(Task::RemoveWorktree(forced), &client);
        assert!(matches!(events[..], [Event::Removed { .. }]), "{events:?}");
        assert!(!path.exists());
        sessions_become(&client, 0);
        assert_eq!(git_out(&repo, &["branch", "--list", "fix"]).trim(), "fix");
    }

    #[test]
    fn a_forced_removal_git_still_refuses_shows_why() {
        let (_daemon, client) = daemon();
        let (_tmp, repo) = repository();
        let (path, session) = linked(&repo, &client);
        git_out(&repo, &["worktree", "lock", path.to_str().unwrap()]);
        let events = events(
            Task::RemoveWorktree(remove(&repo, &path, &session, true)),
            &client,
        );
        let [Event::Error { title, output }] = &events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(title, "fix not removed");
        assert!(output.contains("locked"), "{output}");
        assert!(path.exists());
        assert_eq!(client.sessions().unwrap().len(), 1);
    }
}
