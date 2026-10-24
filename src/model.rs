//! Merges config, git and tmux into what the UI shows.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::git::Worktree;
use crate::refresh::Update;
use crate::tmux::{self, AgentStatus, Session};

/// How long after its last output an agent without hooks still counts as active, in seconds.
const ACTIVE_FOR: u64 = 10;

/// A project from the config, with its worktrees.
#[derive(Debug, PartialEq)]
pub struct ProjectView {
    pub path: PathBuf,
    /// The directory's name, the project's title.
    pub name: String,
    /// Why the worktrees cannot be listed: a config problem, or git's message.
    pub error: Option<String>,
    /// The main worktree first, as git lists it.
    pub worktrees: Vec<WorktreeView>,
    /// The remote's default branch, `origin/main`, when it has one.
    pub remote_default: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct WorktreeView {
    pub path: PathBuf,
    /// The directory's name.
    pub name: String,
    /// `None` when detached.
    pub branch: Option<String>,
    /// The repository's main worktree.
    pub main: bool,
    pub session: Option<SessionView>,
    pub status: Status,
}

#[derive(Debug, PartialEq)]
pub struct SessionView {
    /// The target for tmux commands.
    pub id: String,
    pub name: String,
    pub agent_pane: Option<String>,
}

impl WorktreeView {
    /// What the UI calls it: its branch, or its directory's name when detached.
    pub fn label(&self) -> &str {
        self.branch.as_deref().unwrap_or(&self.name)
    }
}

/// A session on tatou's server, for the Sessions pane.
#[derive(Debug, PartialEq)]
pub struct SessionRow {
    /// The target for tmux commands.
    pub id: String,
    pub name: String,
    pub status: Status,
    /// What runs in its current window: `claude`, `zsh`.
    pub command: Option<String>,
    /// What it is for: `project/worktree` when it belongs to one, else the
    /// folder it started in, with `~/`.
    pub place: String,
}

/// A worktree's agent: reported by its hooks, else guessed from its window's activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    Input,
    Idle,
    /// No hooks, and output within the last `ACTIVE_FOR` seconds.
    Active,
    /// No hooks, and no recent output.
    Quiet,
    NoSession,
}

/// The latest result from each source, kept until the next one arrives.
#[derive(Debug, Default)]
pub struct Sources {
    sessions: Vec<Session>,
    /// Why tmux could not be read; the last sessions read are kept meanwhile.
    sessions_error: Option<String>,
    worktrees: HashMap<PathBuf, Result<Vec<Worktree>, String>>,
    remote_defaults: HashMap<PathBuf, Option<String>>,
}

impl Sources {
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Sessions(Ok(sessions)) => {
                self.sessions = sessions;
                self.sessions_error = None;
            }
            Update::Sessions(Err(err)) => self.sessions_error = Some(err),
            Update::Worktrees {
                project,
                result,
                remote_default,
            } => {
                self.remote_defaults.insert(project.clone(), remote_default);
                self.worktrees.insert(project, result);
            }
            // The UI applies it: it owns the config
            Update::Config(_) => {}
        }
    }

    pub fn view(&self, config: &Config, now: u64) -> Vec<ProjectView> {
        let mut view = merge(config, &self.worktrees, &self.sessions, now);
        for project in &mut view {
            project.remote_default = self.remote_defaults.get(&project.path).cloned().flatten();
        }
        view
    }

    pub fn sessions_error(&self) -> Option<&str> {
        self.sessions_error.as_deref()
    }

    /// Every session but the dashboard's, in tmux's order (by name), with the
    /// worktree each belongs to in `projects`, the view built from these sources.
    pub fn session_rows(
        &self,
        projects: &[ProjectView],
        now: u64,
        home: Option<&Path>,
    ) -> Vec<SessionRow> {
        let mut places = HashMap::new();
        for project in projects {
            for worktree in &project.worktrees {
                if let Some(session) = &worktree.session {
                    let place = format!("{}/{}", project.name, worktree.label());
                    places.insert(session.id.as_str(), place);
                }
            }
        }
        let mine = self.sessions.iter().filter(|s| s.name != tmux::DASHBOARD);
        mine.map(|session| SessionRow {
            id: session.id.clone(),
            name: session.name.clone(),
            status: status(session, now),
            command: session.command.clone(),
            place: match places.get(session.id.as_str()) {
                Some(place) => place.clone(),
                None => config::tilde(&session.path, home),
            },
        })
        .collect()
    }
}

/// Builds the view. `worktrees` holds each project's `git worktree list`, or git's
/// error, by project path; a project not in it yet shows no worktrees. `now` is in
/// seconds since the Unix epoch, like `Session::activity`.
pub fn merge(
    config: &Config,
    worktrees: &HashMap<PathBuf, Result<Vec<Worktree>, String>>,
    sessions: &[Session],
    now: u64,
) -> Vec<ProjectView> {
    let by_path = sessions_by_path(sessions);
    let mut projects = Vec::new();
    for project in &config.projects {
        let mut view = ProjectView {
            path: project.path.clone(),
            name: file_name(&project.path),
            error: project.error.clone(),
            worktrees: Vec::new(),
            remote_default: None,
        };
        match worktrees.get(&project.path) {
            _ if project.error.is_some() => {}
            Some(Ok(list)) => {
                // A bare repository's entry has no files to work on
                let checked_out = list.iter().filter(|worktree| !worktree.bare);
                for (i, worktree) in checked_out.enumerate() {
                    let session = by_path.get(&canonical(&worktree.path)).copied();
                    view.worktrees.push(WorktreeView {
                        path: worktree.path.clone(),
                        name: file_name(&worktree.path),
                        branch: worktree.branch.clone(),
                        main: i == 0 && !list[0].bare,
                        status: session.map_or(Status::NoSession, |s| status(s, now)),
                        session: session.map(|s| SessionView {
                            id: s.id.clone(),
                            name: s.name.clone(),
                            agent_pane: s.agent_pane.clone(),
                        }),
                    });
                }
            }
            Some(Err(message)) => view.error = Some(message.clone()),
            None => {}
        }
        projects.push(view);
    }
    projects
}

/// Sessions by the worktree they belong to: `@tatou_path` when set, else the
/// session's start directory, never its name. When two sessions claim one worktree,
/// one matched by `@tatou_path` wins, then the first in tmux's order (by name).
/// The dashboard's own session belongs to no worktree.
fn sessions_by_path(sessions: &[Session]) -> HashMap<PathBuf, &Session> {
    let mut by_path = HashMap::new();
    let mine = sessions.iter().filter(|s| s.name != tmux::DASHBOARD);
    let (tagged, untagged): (Vec<_>, Vec<_>) = mine.partition(|s| s.tatou_path.is_some());
    for session in tagged.into_iter().chain(untagged) {
        let path = session.tatou_path.as_ref().unwrap_or(&session.path);
        by_path.entry(canonical(path)).or_insert(session);
    }
    by_path
}

fn status(session: &Session, now: u64) -> Status {
    match session.status {
        Some(AgentStatus::Busy) => Status::Busy,
        Some(AgentStatus::Input) => Status::Input,
        Some(AgentStatus::Idle) => Status::Idle,
        None if now.saturating_sub(session.activity) <= ACTIVE_FOR => Status::Active,
        None => Status::Quiet,
    }
}

/// `path` with symlinks resolved, so that `/var/…` and `/private/var/…` on macOS
/// compare equal; as is when it does not exist.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn file_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::{Keys, Project, Tmux};

    const NOW: u64 = 1_000_000;

    fn config(projects: &[&str]) -> Config {
        Config {
            path: "/c/config.toml".into(),
            default_agent: "claude".into(),
            show_ignored: false,
            tmux: Tmux::default(),
            agents: BTreeMap::new(),
            keys: Keys::default(),
            projects: projects
                .iter()
                .map(|path| Project {
                    path: path.into(),
                    base: None,
                    worktree_dir: None,
                    on_create: None,
                    error: None,
                    file: "/c/projects/p.toml".into(),
                })
                .collect(),
        }
    }

    fn worktree(path: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("abc".into()),
            branch: branch.map(String::from),
            detached: branch.is_none(),
            bare: false,
            locked: None,
            prunable: None,
        }
    }

    /// A session tatou did not create: no options, matched by its start directory.
    fn session(id: &str, name: &str, path: &str) -> Session {
        Session {
            id: id.into(),
            name: name.into(),
            tatou_path: None,
            status: None,
            agent_pane: None,
            path: path.into(),
            activity: NOW,
            command: None,
        }
    }

    /// A session tatou created for `path`, started elsewhere.
    fn tatou_session(id: &str, name: &str, path: &str) -> Session {
        Session {
            tatou_path: Some(path.into()),
            agent_pane: Some("%1".into()),
            ..session(id, name, "/somewhere/else")
        }
    }

    fn app_worktrees() -> HashMap<PathBuf, Result<Vec<Worktree>, String>> {
        let list = vec![
            worktree("/code/app", Some("main")),
            worktree("/code/app.worktrees/fix", Some("fix")),
        ];
        HashMap::from([(PathBuf::from("/code/app"), Ok(list))])
    }

    /// The session id matched to each worktree of the first project.
    fn matched(sessions: &[Session]) -> Vec<Option<String>> {
        let view = merge(&config(&["/code/app"]), &app_worktrees(), sessions, NOW);
        view[0]
            .worktrees
            .iter()
            .map(|w| w.session.as_ref().map(|s| s.id.clone()))
            .collect()
    }

    #[test]
    fn lists_projects_and_their_worktrees() {
        let sessions = [tatou_session("$1", "app-fix", "/code/app.worktrees/fix")];
        let view = merge(&config(&["/code/app"]), &app_worktrees(), &sessions, NOW);
        let expected = ProjectView {
            path: "/code/app".into(),
            name: "app".into(),
            error: None,
            remote_default: None,
            worktrees: vec![
                WorktreeView {
                    path: "/code/app".into(),
                    name: "app".into(),
                    branch: Some("main".into()),
                    main: true,
                    session: None,
                    status: Status::NoSession,
                },
                WorktreeView {
                    path: "/code/app.worktrees/fix".into(),
                    name: "fix".into(),
                    branch: Some("fix".into()),
                    main: false,
                    session: Some(SessionView {
                        id: "$1".into(),
                        name: "app-fix".into(),
                        agent_pane: Some("%1".into()),
                    }),
                    status: Status::Active,
                },
            ],
        };
        assert_eq!(view, [expected]);
    }

    #[test]
    fn matches_by_option_then_by_session_path() {
        // The option wins over the start directory, wherever the session started
        let by_option = tatou_session("$1", "anything", "/code/app.worktrees/fix");
        assert_eq!(matched(&[by_option]), [None, Some("$1".into())]);
        let by_path = session("$2", "anything", "/code/app");
        assert_eq!(matched(&[by_path]), [Some("$2".into()), None]);
    }

    #[test]
    fn never_matches_by_name() {
        let named_like_it = session("$1", "app-fix", "/elsewhere");
        assert_eq!(matched(&[named_like_it]), [None, None]);
    }

    #[test]
    fn two_sessions_on_one_worktree_pick_the_option_then_the_first() {
        let first = || session("$1", "a", "/code/app.worktrees/fix");
        let second = || session("$2", "b", "/code/app.worktrees/fix");
        let tagged = tatou_session("$3", "c", "/code/app.worktrees/fix");
        let fix = |sessions: &[Session]| matched(sessions)[1].clone();
        assert_eq!(fix(&[first(), second()]), Some("$1".into()));
        assert_eq!(fix(&[second(), first()]), Some("$2".into()));
        assert_eq!(fix(&[second(), first(), tagged]), Some("$3".into()));
    }

    #[test]
    fn the_dashboard_session_is_no_worktrees() {
        let dashboard = session("$0", tmux::DASHBOARD, "/code/app");
        assert_eq!(matched(&[dashboard]), [None, None]);
    }

    #[test]
    fn sessions_without_a_worktree_are_ignored() {
        let elsewhere = session("$1", "notes", "/home/me/notes");
        let view = merge(&config(&["/code/app"]), &app_worktrees(), &[elsewhere], NOW);
        assert_eq!(view.len(), 1);
        assert!(view[0].worktrees.iter().all(|w| w.session.is_none()));
    }

    #[test]
    fn paths_are_compared_with_symlinks_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        // Through a symlink, like /var and /private/var on macOS
        let link = real.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let project = real.to_str().unwrap();
        let worktrees = HashMap::from([(real.clone(), Ok(vec![worktree(project, Some("main"))]))]);
        let through_link = session("$1", "s", link.to_str().unwrap());
        let view = merge(&config(&[project]), &worktrees, &[through_link], NOW);
        assert_eq!(view[0].worktrees[0].status, Status::Active);
    }

    #[test]
    fn status_comes_from_hooks_then_activity() {
        let with = |status, activity| Session {
            status,
            activity,
            ..session("$1", "s", "/code/app")
        };
        let cases = [
            (with(Some(AgentStatus::Busy), 0), Status::Busy),
            (with(Some(AgentStatus::Input), 0), Status::Input),
            (with(Some(AgentStatus::Idle), NOW), Status::Idle),
            (with(None, NOW - ACTIVE_FOR), Status::Active),
            (with(None, NOW - ACTIVE_FOR - 1), Status::Quiet),
            // A clock that went backwards is still recent
            (with(None, NOW + 5), Status::Active),
        ];
        for (session, expected) in cases {
            assert_eq!(status(&session, NOW), expected, "{session:?}");
        }
    }

    #[test]
    fn project_errors_and_projects_not_loaded_yet() {
        let mut config = config(&["/code/app", "/code/broken", "/code/gone", "/code/new"]);
        config.projects[2].error = Some("/code/gone: no such directory".into());
        let mut worktrees = app_worktrees();
        let git_error = "fatal: not a git repository".to_string();
        worktrees.insert("/code/broken".into(), Err(git_error.clone()));
        let view = merge(&config, &worktrees, &[], NOW);
        let summary: Vec<_> = view
            .iter()
            .map(|p| (p.name.as_str(), p.error.as_deref(), p.worktrees.len()))
            .collect();
        let expected = [
            ("app", None, 2),
            ("broken", Some(git_error.as_str()), 0),
            ("gone", Some("/code/gone: no such directory"), 0),
            ("new", None, 0),
        ];
        assert_eq!(summary, expected);
    }

    #[test]
    fn a_bare_main_repository_is_not_listed() {
        let bare = Worktree {
            bare: true,
            head: None,
            ..worktree("/code/api.git", None)
        };
        let list = vec![bare, worktree("/code/api.worktrees/main", Some("main"))];
        let worktrees = HashMap::from([(PathBuf::from("/code/api.git"), Ok(list))]);
        let view = merge(&config(&["/code/api.git"]), &worktrees, &[], NOW);
        let names: Vec<_> = view[0]
            .worktrees
            .iter()
            .map(|w| (w.name.as_str(), w.main))
            .collect();
        assert_eq!(names, [("main", false)]);
    }

    #[test]
    fn sources_keep_the_latest_result_of_each() {
        let mut sources = Sources::default();
        let config = config(&["/code/app"]);
        assert!(
            sources.view(&config, NOW)[0].worktrees.is_empty(),
            "nothing yet"
        );

        sources.apply(Update::Worktrees {
            project: "/code/app".into(),
            result: Ok(vec![worktree("/code/app", Some("main"))]),
            remote_default: None,
        });
        sources.apply(Update::Sessions(Ok(vec![session("$1", "s", "/code/app")])));
        assert_eq!(sources.view(&config, NOW)[0].remote_default, None);
        sources.apply(Update::Worktrees {
            project: "/code/app".into(),
            result: Ok(vec![worktree("/code/app", Some("main"))]),
            remote_default: Some("origin/main".into()),
        });
        let view = sources.view(&config, NOW);
        assert_eq!(view[0].remote_default.as_deref(), Some("origin/main"));
        let status = |sources: &Sources| sources.view(&config, NOW)[0].worktrees[0].status;
        assert_eq!(status(&sources), Status::Active);

        // A tmux error is reported, and the sessions read before are kept
        sources.apply(Update::Sessions(Err("tmux: boom".into())));
        assert_eq!(sources.sessions_error(), Some("tmux: boom"));
        assert_eq!(status(&sources), Status::Active);
        sources.apply(Update::Sessions(Ok(vec![])));
        assert_eq!(sources.sessions_error(), None);
        assert_eq!(status(&sources), Status::NoSession);

        sources.apply(Update::Worktrees {
            project: "/code/app".into(),
            result: Err("fatal: gone".into()),
            remote_default: None,
        });
        let view = sources.view(&config, NOW);
        assert_eq!(view[0].error.as_deref(), Some("fatal: gone"));
    }
}
