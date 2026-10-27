//! Background refresh: worker threads list the daemon's sessions, each project's
//! worktrees and pull requests, watch `config.toml`, and send the results to the UI
//! loop over one channel.
//!
//! One thread for the daemon, two per project (git and `gh`), so a slow or broken
//! repository, or a slow network, only delays itself, and one for the config. Each refreshes on its interval, or at
//! once when woken (after tatou itself changed something). A worker ends quietly
//! when the UI drops the receiving end, or drops `Refresh`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::client::Client;
use crate::config::{self, Config, Stamp};
use crate::gh::{self, Pr};
use crate::git::{self, Worktree};
use crate::protocol::Session;

const SESSIONS_EVERY: Duration = Duration::from_secs(1);
const WORKTREES_EVERY: Duration = Duration::from_secs(5);
const CONFIG_EVERY: Duration = Duration::from_secs(2);
/// `gh` goes over the network, and CI moves in minutes.
const PRS_EVERY: Duration = Duration::from_secs(60);

/// A result from one source; it says what it is about, so sources never wait on
/// each other. Errors are messages for the UI.
#[derive(Debug)]
pub enum Update {
    Sessions(Result<Vec<Session>, String>),
    Worktrees {
        project: PathBuf,
        result: Result<Vec<Worktree>, String>,
        /// The remote's default branch, `origin/main`, when it has one.
        remote_default: Option<String>,
    },
    /// A project's pull requests by branch; empty when `gh` cannot list them
    /// (not installed, not logged in, no GitHub remote), which is only logged.
    Prs {
        project: PathBuf,
        prs: HashMap<String, Pr>,
    },
    /// `config.toml` changed and was loaded again.
    Config(Result<Config, String>),
}

/// The running workers; dropping it stops them once their current command returns.
pub struct Refresh {
    /// For the workers of projects added later.
    updates: Sender<Update>,
    sessions: Sender<()>,
    projects: HashMap<PathBuf, Workers>,
    /// Never woken; dropping it stops the config's worker.
    _config: Sender<()>,
}

impl Refresh {
    /// Starts listing `server`'s sessions and each project's worktrees, and
    /// watching the config, sending every result to `updates`; the first sessions
    /// and worktrees come right away.
    pub fn start(
        client: Client,
        mut watch: ConfigWatch,
        projects: &[PathBuf],
        updates: &Sender<Update>,
    ) -> Refresh {
        let mut last_error = None;
        let sessions = spawn(
            "refresh-daemon",
            SESSIONS_EVERY,
            updates.clone(),
            move || {
                let result = client.sessions().map_err(|err| err.to_string());
                log_change(&mut last_error, &result, "daemon sessions");
                Some(Update::Sessions(result))
            },
        );
        let config = spawn("refresh-config", CONFIG_EVERY, updates.clone(), move || {
            let result = watch.check()?;
            match &result {
                Ok(_) => tracing::info!("config reloaded"),
                Err(err) => tracing::warn!(err, "config reload failed"),
            }
            Some(Update::Config(result))
        });
        let mut refresh = Refresh {
            updates: updates.clone(),
            sessions,
            projects: HashMap::new(),
            _config: config,
        };
        refresh.set_projects(projects);
        refresh
    }

    /// Lists `projects` from now on: starts the new ones and stops the ones gone.
    pub fn set_projects(&mut self, projects: &[PathBuf]) {
        // Dropping a project's wake-up senders ends its workers
        self.projects.retain(|path, _| projects.contains(path));
        for project in projects {
            if !self.projects.contains_key(project) {
                let workers = Workers {
                    worktrees: spawn_project(project.clone(), &self.updates),
                    _prs: spawn_prs(project.clone(), &self.updates),
                };
                self.projects.insert(project.clone(), workers);
            }
        }
    }

    /// Lists the sessions again now, e.g. after tatou started one.
    pub fn sessions_now(&self) {
        // A worker that ended has nothing left to refresh
        let _ = self.sessions.send(());
    }

    /// Lists `project`'s worktrees again now, e.g. after tatou added one.
    pub fn worktrees_now(&self, project: &Path) {
        if let Some(workers) = self.projects.get(project) {
            let _ = workers.worktrees.send(());
        }
    }
}

/// A project's workers, ended by dropping their senders.
struct Workers {
    worktrees: Sender<()>,
    /// Never woken: pull requests change on GitHub, not through tatou.
    _prs: Sender<()>,
}

/// Notices that `config.toml` or a project file changed, by their modification
/// times, or that a project file was added or removed.
pub struct ConfigWatch {
    path: PathBuf,
    seen: Stamp,
}

impl ConfigWatch {
    /// Watches the config at `path`, as it was when `seen` was taken.
    pub fn new(path: PathBuf, seen: Stamp) -> ConfigWatch {
        ConfigWatch { path, seen }
    }

    /// The config loaded again, when it changed since the last check.
    fn check(&mut self) -> Option<Result<Config, String>> {
        let stamp = config::stamp(&self.path);
        if stamp == self.seen {
            return None;
        }
        self.seen = stamp;
        Some(config::reload(&self.path).map_err(|err| err.to_string()))
    }
}

/// Starts the worker listing `project`'s worktrees; returns the sender that wakes it.
fn spawn_project(project: PathBuf, updates: &Sender<Update>) -> Sender<()> {
    let what = project.display().to_string();
    let mut last_error = None;
    spawn("refresh-git", WORKTREES_EVERY, updates.clone(), move || {
        let result = git::worktrees(&project).map_err(|err| err.to_string());
        log_change(&mut last_error, &result, &what);
        // Only a default for new worktrees: without one, the main worktree's branch
        let remote_default = match &result {
            Ok(_) => git::remote_default(&project).ok().flatten(),
            Err(_) => None,
        };
        Some(Update::Worktrees {
            project: project.clone(),
            result,
            remote_default,
        })
    })
}

/// Starts the worker listing `project`'s pull requests; returns its sender.
fn spawn_prs(project: PathBuf, updates: &Sender<Update>) -> Sender<()> {
    let what = format!("{} pull requests", project.display());
    let mut last_error = None;
    spawn("refresh-gh", PRS_EVERY, updates.clone(), move || {
        let result = gh::prs(&project).map_err(|err| err.to_string());
        log_change(&mut last_error, &result, &what);
        Some(prs_update(project.clone(), result))
    })
}

/// No badges for a project whose pull requests `gh` cannot list; why is logged.
fn prs_update(project: PathBuf, result: Result<HashMap<String, Pr>, String>) -> Update {
    Update::Prs {
        project,
        prs: result.unwrap_or_default(),
    }
}

/// Starts a thread running `poll`, and returns the sender that wakes it.
fn spawn(
    name: &str,
    interval: Duration,
    updates: Sender<Update>,
    fetch: impl FnMut() -> Option<Update> + Send + 'static,
) -> Sender<()> {
    let (wake, woken) = mpsc::channel();
    let started = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || poll(interval, &woken, &updates, fetch));
    if let Err(err) = started {
        tracing::error!(name, %err, "cannot start a refresh thread");
    }
    wake
}

/// Sends `fetch()` now, then again every `interval` or as soon as woken; several
/// wake-ups waiting count as one. `None` is nothing new to send. Returns when
/// either channel is closed.
fn poll(
    interval: Duration,
    woken: &Receiver<()>,
    updates: &Sender<Update>,
    mut fetch: impl FnMut() -> Option<Update>,
) {
    loop {
        if let Some(update) = fetch()
            && updates.send(update).is_err()
        {
            return;
        }
        match woken.recv_timeout(interval) {
            Ok(()) => while woken.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Logs an error when it first appears, changes or goes away, not on every refresh.
fn log_change<T>(last: &mut Option<String>, result: &Result<T, String>, what: &str) {
    let error = result.as_ref().err();
    if error == last.as_ref() {
        return;
    }
    match error {
        Some(err) => tracing::warn!(what, err, "refresh failed"),
        None => tracing::info!(what, "refresh works again"),
    }
    *last = error.cloned();
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Instant, SystemTime};

    use super::*;

    /// A fake source: each fetch sends the number of fetches so far as a session list.
    fn counter() -> impl FnMut() -> Option<Update> {
        let mut n = 0;
        move || {
            n += 1;
            Some(Update::Sessions(Err(n.to_string())))
        }
    }

    fn count(update: Update) -> String {
        match update {
            Update::Sessions(Err(n)) => n,
            other => panic!("unexpected update: {other:?}"),
        }
    }

    #[test]
    fn sends_at_once_then_on_every_interval() {
        let (updates, received) = mpsc::channel();
        let (_wake, woken) = mpsc::channel();
        thread::spawn(move || poll(Duration::from_millis(10), &woken, &updates, counter()));
        let counts: Vec<_> = received.iter().take(3).map(count).collect();
        assert_eq!(counts, ["1", "2", "3"]);
    }

    #[test]
    fn a_wake_up_refreshes_before_the_interval() {
        let (updates, received) = mpsc::channel();
        let (wake, woken) = mpsc::channel();
        thread::spawn(move || poll(Duration::from_secs(3600), &woken, &updates, counter()));
        assert_eq!(count(received.recv().unwrap()), "1");
        let start = Instant::now();
        wake.send(()).unwrap();
        assert_eq!(count(received.recv().unwrap()), "2");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn wake_ups_waiting_together_make_one_refresh() {
        let (updates, received) = mpsc::channel();
        let (wake, woken) = mpsc::channel();
        // Each fetch waits for the test, so the wake-ups queue up while one runs
        let (gate, gated) = mpsc::channel::<()>();
        let mut counter = counter();
        let fetch = move || {
            let _ = gated.recv();
            counter()
        };
        thread::spawn(move || poll(Duration::from_secs(3600), &woken, &updates, fetch));
        for _ in 0..3 {
            wake.send(()).unwrap();
        }
        gate.send(()).unwrap();
        assert_eq!(count(received.recv().unwrap()), "1");
        gate.send(()).unwrap();
        assert_eq!(count(received.recv().unwrap()), "2");
        // Nothing left waiting: no third refresh, even with the gate open
        gate.send(()).unwrap();
        let more = received.recv_timeout(Duration::from_millis(200));
        assert!(more.is_err(), "{more:?}");
    }

    #[test]
    fn ends_when_the_ui_stops_listening() {
        let (updates, received) = mpsc::channel();
        let (_wake, woken) = mpsc::channel();
        drop(received);
        let worker =
            thread::spawn(move || poll(Duration::from_millis(1), &woken, &updates, counter()));
        worker.join().unwrap();
    }

    #[test]
    fn ends_when_refresh_is_dropped() {
        let (updates, received) = mpsc::channel();
        let (wake, woken) = mpsc::channel::<()>();
        let worker =
            thread::spawn(move || poll(Duration::from_secs(3600), &woken, &updates, counter()));
        assert_eq!(count(received.recv().unwrap()), "1");
        drop(wake);
        worker.join().unwrap();
    }

    #[test]
    fn errors_are_logged_only_when_they_change() {
        let mut last = None;
        let failed: Result<(), String> = Err("boom".into());
        log_change(&mut last, &failed, "daemon");
        assert_eq!(last.as_deref(), Some("boom"));
        log_change(&mut last, &failed, "daemon");
        assert_eq!(last.as_deref(), Some("boom"));
        log_change(&mut last, &Ok::<(), String>(()), "daemon");
        assert_eq!(last, None);
    }

    #[test]
    fn starts_with_the_daemon_and_each_project() {
        let dir = tempfile::tempdir().unwrap();
        let nobody = Client::new(&dir.path().join("no.sock"));
        let (good, broken) = (dir.path().join("good"), dir.path().join("missing"));
        fs::create_dir(&good).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&good)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(init.success());
        let (updates, received) = mpsc::channel();
        let refresh = Refresh::start(
            nobody,
            unchanged(&dir.path().join("config.toml")),
            &[good.clone(), broken.clone()],
            &updates,
        );

        let mut seen = HashMap::new();
        while seen.len() < 3 {
            match received.recv_timeout(Duration::from_secs(10)).unwrap() {
                Update::Sessions(result) => {
                    let err = result.unwrap_err();
                    assert!(err.contains("cannot reach the daemon"), "{err}");
                    seen.insert("daemon".to_string(), true);
                }
                Update::Worktrees {
                    project, result, ..
                } if project == good => {
                    assert_eq!(result.unwrap().len(), 1, "the main worktree");
                    seen.insert("good".to_string(), true);
                }
                Update::Worktrees {
                    project, result, ..
                } => {
                    assert_eq!(project, broken);
                    let err = result.unwrap_err();
                    assert!(err.contains("cannot change to"), "{err}");
                    seen.insert("missing".to_string(), true);
                }
                // Neither has a GitHub remote: gh fails before any network
                Update::Prs { prs, .. } => assert_eq!(prs, HashMap::new()),
                Update::Config(result) => panic!("no config, no change: {result:?}"),
            }
        }
        drop(refresh);
    }

    #[test]
    fn a_gh_error_is_no_pull_requests_for_that_project() {
        let update = prs_update("/code/app".into(), Err("gh: not logged in".into()));
        let Update::Prs { project, prs } = update else {
            panic!("{update:?}");
        };
        assert_eq!(project, Path::new("/code/app"));
        assert!(prs.is_empty());
    }

    #[test]
    fn new_projects_start_and_removed_ones_stop() {
        let dir = tempfile::tempdir().unwrap();
        let nobody = Client::new(&dir.path().join("no.sock"));
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        let watch = unchanged(&dir.path().join("config.toml"));
        let (updates, received) = mpsc::channel();
        let mut refresh = Refresh::start(nobody, watch, &[first], &updates);
        refresh.set_projects(std::slice::from_ref(&second));
        // The second project's worktrees come right away, though they fail
        let listed = received.iter().find_map(|update| match update {
            Update::Worktrees { project, .. } if project == second => Some(project),
            _ => None,
        });
        assert_eq!(listed, Some(second.clone()));
        // The first one's wake-up sender is gone, which ends its worker
        let watched: Vec<_> = refresh.projects.keys().collect();
        assert_eq!(watched, [&second]);
    }

    /// Watches the config at `path` from as it is now.
    fn unchanged(path: &Path) -> ConfigWatch {
        ConfigWatch::new(path.to_path_buf(), config::stamp(path))
    }

    /// Writes `text` to `path`, dated `secs` after the epoch, so that two writes in
    /// a row never share a modification time.
    fn write_at(path: &Path, text: &str, secs: u64) {
        fs::write(path, text).unwrap();
        let file = fs::File::options().write(true).open(path).unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        file.set_modified(time).unwrap();
    }

    #[test]
    fn a_changed_config_is_loaded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_at(&path, "", 1_000);
        let mut watch = unchanged(&path);
        assert!(
            watch.check().is_none(),
            "unchanged since the dashboard loaded it"
        );

        write_at(&path, "theme = \"monokai-pro\"\n", 2_000);
        let config = watch.check().unwrap().unwrap();
        assert_eq!(config.theme.name, "monokai-pro");
        assert!(watch.check().is_none(), "loaded once");

        // A project file added, then edited
        let projects = dir.path().join("projects");
        fs::create_dir(&projects).unwrap();
        write_at(&projects.join("root.toml"), "path = \"/\"\n", 2_500);
        let config = watch.check().unwrap().unwrap();
        assert_eq!(config.projects[0].path, Path::new("/"));
        assert!(watch.check().is_none(), "loaded once");
        write_at(&projects.join("root.toml"), "path = \"/tmp\"\n", 2_600);
        let config = watch.check().unwrap().unwrap();
        assert_eq!(
            config.projects[0].path,
            Path::new("/tmp").canonicalize().unwrap()
        );

        write_at(&path, "popop = 1\n", 3_000);
        let err = watch.check().unwrap().unwrap_err();
        assert!(err.contains("unknown field `popop`"), "{err}");
        assert!(watch.check().is_none(), "reported once");

        fs::remove_file(&path).unwrap();
        let err = watch.check().unwrap().unwrap_err();
        assert!(err.contains("No such file"), "{err}");
    }
}
