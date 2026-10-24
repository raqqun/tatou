//! Background refresh: worker threads list tmux's sessions and each project's
//! worktrees, and send the results to the UI loop over one channel.
//!
//! One thread for tmux and one per project, so a slow or broken repository only
//! delays itself. Each refreshes on its interval, or at once when woken (after
//! tatou itself changed something). A worker ends quietly when the UI drops the
//! receiving end, or drops `Refresh`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::git::{self, Worktree};
use crate::tmux::{Server, Session};

const SESSIONS_EVERY: Duration = Duration::from_secs(1);
const WORKTREES_EVERY: Duration = Duration::from_secs(5);

/// A result from one source; it says what it is about, so sources never wait on
/// each other. Errors are messages for the UI.
#[derive(Debug)]
pub enum Update {
    Sessions(Result<Vec<Session>, String>),
    Worktrees {
        project: PathBuf,
        result: Result<Vec<Worktree>, String>,
    },
}

/// The running workers; dropping it stops them once their current command returns.
// Not created yet: the dashboard starts it (#14)
#[allow(dead_code)]
pub struct Refresh {
    sessions: Sender<()>,
    projects: HashMap<PathBuf, Sender<()>>,
}

// Not called yet: the dashboard starts it, and actions wake it (#14, #21)
#[allow(dead_code)]
impl Refresh {
    /// Starts listing `server`'s sessions and each project's worktrees, sending
    /// every result to `updates`; the first ones come right away.
    pub fn start(server: Server, projects: &[PathBuf], updates: &Sender<Update>) -> Refresh {
        let mut last_error = None;
        let sessions = spawn("refresh-tmux", SESSIONS_EVERY, updates.clone(), move || {
            let result = server.sessions().map_err(|err| err.to_string());
            log_change(&mut last_error, &result, "tmux sessions");
            Update::Sessions(result)
        });
        let mut wakes = HashMap::new();
        for project in projects {
            let path = project.clone();
            let what = path.display().to_string();
            let mut last_error = None;
            let wake = spawn("refresh-git", WORKTREES_EVERY, updates.clone(), move || {
                let result = git::worktrees(&path).map_err(|err| err.to_string());
                log_change(&mut last_error, &result, &what);
                Update::Worktrees {
                    project: path.clone(),
                    result,
                }
            });
            wakes.insert(project.clone(), wake);
        }
        Refresh {
            sessions,
            projects: wakes,
        }
    }

    /// Lists the sessions again now, e.g. after tatou started one.
    pub fn sessions_now(&self) {
        // A worker that ended has nothing left to refresh
        let _ = self.sessions.send(());
    }

    /// Lists `project`'s worktrees again now, e.g. after tatou added one.
    pub fn worktrees_now(&self, project: &Path) {
        if let Some(wake) = self.projects.get(project) {
            let _ = wake.send(());
        }
    }
}

/// Starts a thread running `poll`, and returns the sender that wakes it.
fn spawn(
    name: &str,
    interval: Duration,
    updates: Sender<Update>,
    fetch: impl FnMut() -> Update + Send + 'static,
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
/// wake-ups waiting count as one. Returns when either channel is closed.
fn poll(
    interval: Duration,
    woken: &Receiver<()>,
    updates: &Sender<Update>,
    mut fetch: impl FnMut() -> Update,
) {
    loop {
        if updates.send(fetch()).is_err() {
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
    use std::time::Instant;

    use super::*;
    use crate::tmux::testing::TestServer;

    /// A fake source: each fetch sends the number of fetches so far as a session list.
    fn counter() -> impl FnMut() -> Update {
        let mut n = 0;
        move || {
            n += 1;
            Update::Sessions(Err(n.to_string()))
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
        log_change(&mut last, &failed, "tmux");
        assert_eq!(last.as_deref(), Some("boom"));
        log_change(&mut last, &failed, "tmux");
        assert_eq!(last.as_deref(), Some("boom"));
        log_change(&mut last, &Ok::<(), String>(()), "tmux");
        assert_eq!(last, None);
    }

    #[test]
    fn starts_with_tmux_and_each_project() {
        let Some(server) = TestServer::new() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
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
            Server::clone(&server),
            &[good.clone(), broken.clone()],
            &updates,
        );

        let mut seen = HashMap::new();
        while seen.len() < 3 {
            match received.recv_timeout(Duration::from_secs(10)).unwrap() {
                Update::Sessions(result) => {
                    assert_eq!(result.unwrap(), [], "no server yet: no sessions");
                    seen.insert("tmux".to_string(), true);
                }
                Update::Worktrees { project, result } if project == good => {
                    assert_eq!(result.unwrap().len(), 1, "the main worktree");
                    seen.insert("good".to_string(), true);
                }
                Update::Worktrees { project, result } => {
                    assert_eq!(project, broken);
                    let err = result.unwrap_err();
                    assert!(err.contains("cannot change to"), "{err}");
                    seen.insert("missing".to_string(), true);
                }
            }
        }
        drop(refresh);
    }
}
