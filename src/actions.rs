//! The commands behind the dashboard's actions, each run on a thread of its own.
//! They report what happens as events over a channel, so the UI never waits.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use crate::error::Error;
use crate::exec;
use crate::git;
use crate::tmux::Server;

/// An action the UI asks for.
#[derive(Debug, PartialEq)]
pub enum Task {
    /// Shows the session with this id in the dashboard's client.
    Switch(String),
    NewWorktree(NewWorktree),
    /// An agent in a new session, for a worktree that has none.
    StartAgent(StartAgent),
    /// The agent of a session, started again.
    RestartAgent(RestartAgent),
}

#[derive(Debug, PartialEq)]
pub struct StartAgent {
    /// The worktree's label, for progress.
    pub name: String,
    pub path: PathBuf,
    pub session: String,
    pub command: String,
}

#[derive(Debug, PartialEq)]
pub struct RestartAgent {
    pub name: String,
    pub path: PathBuf,
    /// The session's id.
    pub session: String,
    /// `@tatou_agent_pane`, unless the session was started by hand.
    pub pane: Option<String>,
    pub command: String,
}

/// A worktree to create, with its project's `on_create` and an agent to start in it.
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
    /// The agent's command, run by tmux's default shell.
    pub command: String,
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
    /// `on_create` failed; the worktree stays and the agent still starts.
    OnCreateFailed(String),
    /// The agent's session runs.
    Started,
}

/// Runs `task` on a thread of its own, sending what happens to `events`.
pub fn start(task: Task, server: &Server, events: &Sender<Event>) {
    let server = server.clone();
    let sender = events.clone();
    let started = thread::Builder::new()
        .name("task".to_string())
        .spawn(move || run(task, &server, &sender));
    if let Err(err) = started {
        tracing::error!(%err, "cannot start a task thread");
        let _ = events.send(Event::Failed(format!("cannot start a thread: {err}")));
    }
}

fn run(task: Task, server: &Server, events: &Sender<Event>) {
    // A send fails only when the dashboard has ended: nobody to tell
    let send = |event| {
        let _ = events.send(event);
    };
    match task {
        Task::Switch(session) => {
            if let Err(err) = server.switch_client(&session) {
                tracing::warn!(session, %err, "switch failed");
                send(Event::Failed(err.to_string()));
            }
        }
        Task::NewWorktree(new) => new_worktree(&new, server, &send),
        Task::StartAgent(start) => {
            send(Event::Progress(format!(
                "{}: starting the agent",
                start.name
            )));
            let started = server.new_session(&start.session, &start.path, &start.command);
            send(match started {
                Ok(_) => Event::Started,
                Err(err) => Event::Failed(format!("{}: {err}", start.name)),
            });
        }
        Task::RestartAgent(restart) => {
            send(Event::Progress(format!(
                "{}: restarting the agent",
                restart.name
            )));
            send(match restart_agent(&restart, server) {
                Ok(()) => Event::Started,
                Err(err) => Event::Failed(format!("{}: {err}", restart.name)),
            });
        }
    }
}

/// Respawns the agent's pane; without one, or when it is gone, starts the agent
/// in a new window of the session and makes that the agent's pane, so that
/// nothing the user started is killed.
fn restart_agent(restart: &RestartAgent, server: &Server) -> crate::error::Result<()> {
    if let Some(pane) = &restart.pane {
        match server.respawn_agent(pane, &restart.command) {
            Ok(()) => return Ok(()),
            Err(Error::Command { stderr, .. }) if stderr.starts_with("can't find pane") => {
                tracing::info!(pane, "agent pane gone: starting the agent in a new window");
            }
            Err(err) => return Err(err),
        }
    }
    let pane = server.new_window(&restart.session, &restart.path, Some(&restart.command))?;
    server.set_agent_pane(&restart.session, &pane)
}

/// `git worktree add`, then `on_create` in the new worktree, then the agent's
/// session. A failure of `on_create` is reported and the session still starts.
fn new_worktree(new: &NewWorktree, server: &Server, send: &dyn Fn(Event)) {
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

    send(Event::Progress(format!("{name}: starting the agent")));
    match server.new_session(&new.session, &path, &new.command) {
        Ok(session) => {
            tracing::info!(name, session = session.name, "agent started");
            send(Event::Started);
        }
        Err(err) => {
            tracing::warn!(name, %err, "agent not started");
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

    use super::*;
    use crate::tmux::testing::TestServer;

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

    /// A test server whose default shell is `/bin/sh`.
    fn sh_server(dir: &Path) -> Option<TestServer> {
        let conf = dir.join("tmux.conf");
        fs::write(&conf, "set-option -g default-shell /bin/sh\n").unwrap();
        TestServer::with_conf(&conf)
    }

    fn request(repo: &Path, name: &str, on_create: Option<&str>) -> NewWorktree {
        NewWorktree {
            project: repo.to_path_buf(),
            name: name.to_string(),
            base: None,
            path: git::worktree_path(repo, name, None),
            on_create: on_create.map(String::from),
            session: format!("app-{name}"),
            command: "sleep 1000".to_string(),
        }
    }

    /// Runs `task` to its end and returns what it reported.
    fn events(task: Task, server: &Server) -> Vec<Event> {
        let (sender, received) = mpsc::channel();
        run(task, server, &sender);
        drop(sender);
        received.iter().collect()
    }

    #[test]
    fn creates_the_worktree_runs_on_create_and_starts_the_agent() {
        let (tmp, repo) = repository();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let new = request(&repo, "fix", Some("touch created"));
        let path = new.path.clone();
        let events = events(Task::NewWorktree(new), &server);
        assert_eq!(
            events,
            [
                Event::Progress("fix: creating the worktree".into()),
                Event::Added {
                    project: repo.clone(),
                    path: path.clone(),
                },
                Event::Progress("fix: running on_create".into()),
                Event::Progress("fix: starting the agent".into()),
                Event::Started,
            ]
        );
        assert!(
            path.join("created").exists(),
            "on_create ran in the worktree"
        );
        let sessions = server.sessions().unwrap();
        assert_eq!(sessions[0].name, "app-fix");
        assert_eq!(sessions[0].tatou_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn a_refused_worktree_starts_nothing() {
        let (tmp, repo) = repository();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        // The branch exists already
        let events = events(Task::NewWorktree(request(&repo, "main", None)), &server);
        let [Event::Progress(_), Event::Refused(error)] = &events[..] else {
            panic!("{events:?}");
        };
        assert!(error.contains("already exists"), "{error}");
        assert!(!error.contains("git -C"), "git's message only: {error}");
        assert_eq!(server.sessions().unwrap(), []);
    }

    #[test]
    fn a_failed_on_create_keeps_the_worktree_and_starts_the_agent() {
        let (tmp, repo) = repository();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let new = request(&repo, "fix", Some("echo oops >&2; exit 3"));
        let path = new.path.clone();
        let events = events(Task::NewWorktree(new), &server);
        let failed = events.iter().find_map(|event| match event {
            Event::OnCreateFailed(output) => Some(output.clone()),
            _ => None,
        });
        assert!(
            failed.is_some_and(|output| output.contains("oops")),
            "{events:?}"
        );
        assert_eq!(events.last(), Some(&Event::Started));
        assert!(path.exists());
        assert_eq!(server.sessions().unwrap().len(), 1);
    }

    /// `format` for `target`, from tmux.
    fn show(server: &Server, target: &str, format: &str) -> String {
        let output = server.run(&["display-message", "-p", "-t", target, format]);
        output.unwrap().stdout.trim_end().to_string()
    }

    fn restart(path: &Path, session: &str, pane: Option<&str>) -> Task {
        Task::RestartAgent(RestartAgent {
            name: "fix".into(),
            path: path.to_path_buf(),
            session: session.into(),
            pane: pane.map(String::from),
            command: "sleep 2000".into(),
        })
    }

    #[test]
    fn starts_an_agent_in_a_new_session() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let start = Task::StartAgent(StartAgent {
            name: "fix".into(),
            path: tmp.path().to_path_buf(),
            session: "app-fix".into(),
            command: "sleep 1000".into(),
        });
        let events = events(start, &server);
        assert_eq!(events.last(), Some(&Event::Started), "{events:?}");
        let sessions = server.sessions().unwrap();
        assert_eq!(sessions[0].name, "app-fix");
        assert!(sessions[0].agent_pane.is_some());
    }

    #[test]
    fn restarts_the_agent_in_its_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let session = server
            .new_session("app-fix", tmp.path(), "sleep 1000")
            .unwrap();
        let events = events(
            restart(tmp.path(), &session.id, Some(&session.pane)),
            &server,
        );
        assert_eq!(events.last(), Some(&Event::Started), "{events:?}");
        assert_eq!(
            show(&server, &session.pane, "#{pane_start_command}"),
            "\"sleep 2000\""
        );
        assert_eq!(show(&server, &session.id, "#{session_windows}"), "1");
    }

    #[test]
    fn a_gone_agent_pane_gets_a_new_window() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let session = server
            .new_session("app-fix", tmp.path(), "sleep 1000")
            .unwrap();
        // A shell keeps the session alive once the agent's pane is closed
        server.new_window(&session.id, tmp.path(), None).unwrap();
        server.run(&["kill-pane", "-t", &session.pane]).unwrap();

        let events = events(
            restart(tmp.path(), &session.id, Some(&session.pane)),
            &server,
        );
        assert_eq!(events.last(), Some(&Event::Started), "{events:?}");
        let agent = server.sessions().unwrap()[0].agent_pane.clone().unwrap();
        assert_ne!(agent, session.pane, "the new window's pane");
        assert_eq!(
            show(&server, &agent, "#{pane_start_command}"),
            "\"sleep 2000\""
        );
        assert_eq!(show(&server, &session.id, "#{session_windows}"), "2");
    }

    #[test]
    fn a_session_started_by_hand_keeps_its_panes() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(server) = sh_server(tmp.path()) else {
            return;
        };
        let dir = tmp.path().to_str().unwrap();
        let args = [
            "new-session",
            "-d",
            "-c",
            dir,
            "-P",
            "-F",
            "#{session_id} #{pane_id}",
        ];
        let output = server.run(&args).unwrap().stdout;
        let (id, shell) = output.trim_end().split_once(' ').unwrap();

        let events = events(restart(tmp.path(), id, None), &server);
        assert_eq!(events.last(), Some(&Event::Started), "{events:?}");
        assert_eq!(
            show(&server, id, "#{session_windows}"),
            "2",
            "the shell is still there"
        );
        let agent = server.sessions().unwrap()[0].agent_pane.clone().unwrap();
        assert_ne!(agent, shell);
    }
}
