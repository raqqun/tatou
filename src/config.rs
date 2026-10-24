//! Loads and saves `config.toml`.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Written when `config.toml` does not exist: every setting, commented out.
const TEMPLATE: &str = r#"# tatou configuration: https://github.com/raqqun/tatou
# Every setting is optional; the values shown are the defaults.

# Agent started in new worktrees, one of the agents below
# default_agent = "claude"

# Show gitignored and hidden files in the file tree
# show_ignored = false

# [tmux]
# popup = "M-g"               # opens `tatou --popup`
# back = "M-d"                # returns to the dashboard
# prefix = "C-b"              # prefix key on tatou's tmux server
# inherit_user_conf = false   # also source your own tmux config before tatou's settings

# Commands that start an agent; added to the built-in claude and codex, or
# replacing one of them when the name is the same. Shells are `t`'s job
# [agents]
# aider = "aider"

# Override any default key by action name
# [keys]
# jump = "Enter"
# new_worktree = "n"

# Custom actions in the tatou UI: key = tmux command template;
# {path}, {file}, {session}, {pane} are substituted
# [keys.custom]
# "g" = "display-popup -E -d {path} lazygit"

# Extra bindings written into tatou's tmux config (active everywhere on the tatou server)
# [keys.tmux]
# "M-|" = "split-window -h -c '#{pane_current_path}'"

# Projects are not set here: each is a file in projects/, next to this one,
# written by `tatou add <path>` and yours to edit. projects/my-app.toml:
#   path = "~/code/my-app"
#   base = "main"                        # default: the branch checked out in the main worktree
#   worktree_dir = "~/worktrees/my-app"  # default: ~/code/my-app.worktrees
#   on_create = "cp ../my-app/.env . && npm install"
"#;

/// The configuration, with defaults applied and paths resolved.
#[derive(Debug, PartialEq)]
pub struct Config {
    /// Where it was loaded from, shown in the UI's empty state.
    pub path: PathBuf,
    pub default_agent: String,
    pub show_ignored: bool,
    pub tmux: Tmux,
    /// Built-in agents merged with the user's `[agents]`.
    pub agents: BTreeMap<String, String>,
    pub keys: Keys,
    /// From the files in `projects/`, by file name.
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tmux {
    pub popup: String,
    pub back: String,
    pub prefix: Option<String>,
    pub inherit_user_conf: bool,
}

impl Default for Tmux {
    fn default() -> Self {
        Tmux {
            popup: "M-g".into(),
            back: "M-d".into(),
            prefix: None,
            inherit_user_conf: false,
        }
    }
}

// Not read yet: the keymap and the tmux config generator use it (#41, #42, #43)
#[allow(dead_code)]
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Keys {
    /// `[keys.custom]`: key → tmux command template run from the tatou UI.
    pub custom: BTreeMap<String, String>,
    /// `[keys.tmux]`: key → tmux command bound on tatou's tmux server.
    pub tmux: BTreeMap<String, String>,
    /// Action name → key. Names are checked against the keymap's actions (#41);
    /// serde cannot reject unknown fields next to `flatten`.
    #[serde(flatten)]
    pub actions: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq)]
pub struct Project {
    /// Canonical when the directory exists, else as written with `~` expanded.
    pub path: PathBuf,
    /// Branch new worktrees start from; `None` means the main worktree's branch.
    pub base: Option<String>,
    pub worktree_dir: Option<PathBuf>,
    pub on_create: Option<String>,
    /// Why the project cannot be used, e.g. its directory is missing or its file
    /// does not parse; shown next to it.
    pub error: Option<String>,
    /// The file in `projects/` it comes from.
    pub file: PathBuf,
}

/// `config.toml` as written by the user.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    default_agent: Option<String>,
    #[serde(default)]
    show_ignored: bool,
    #[serde(default)]
    tmux: Tmux,
    #[serde(default)]
    agents: BTreeMap<String, String>,
    #[serde(default)]
    keys: Keys,
    /// Where projects were before they moved to `projects/`; an error with a hint.
    project: Option<toml::Value>,
}

/// A file in `projects/`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    path: String,
    base: Option<String>,
    worktree_dir: Option<String>,
    on_create: Option<String>,
}

/// `<config dir>/config.toml`.
pub fn path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Loads `<config dir>/config.toml`, first writing the commented template if it
/// does not exist.
pub fn load() -> Result<Config> {
    let path = path()?;
    write_template(&path)?;
    reload(&path)
}

/// Loads `path` again after it changed; unlike `load`, never writes it.
pub fn reload(path: &Path) -> Result<Config> {
    let home = env::var_os("HOME").map(PathBuf::from);
    load_from(path, home.as_deref())
}

/// The folder of project files, next to the config at `path`.
pub fn projects_dir(path: &Path) -> PathBuf {
    path.with_file_name("projects")
}

/// When the config at `path` and its project files were last modified, to tell
/// when it needs loading again.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    config: Option<SystemTime>,
    /// By file name; adding or removing a file changes it too.
    projects: Vec<(PathBuf, Option<SystemTime>)>,
}

pub fn stamp(path: &Path) -> Stamp {
    let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let files = project_files(&projects_dir(path)).unwrap_or_default();
    Stamp {
        config: modified(path),
        projects: files
            .into_iter()
            .map(|file| {
                let modified = modified(&file);
                (file, modified)
            })
            .collect(),
    }
}

pub fn load_from(path: &Path, home: Option<&Path>) -> Result<Config> {
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut config = parse(&text, path).map_err(|message| Error::Config {
        path: path.to_path_buf(),
        message,
    })?;
    config.projects = load_projects(&projects_dir(path), home)?;
    Ok(config)
}

/// The `.toml` files in `dir`, by name; none when `dir` does not exist. Hidden
/// files, editors' backups and `write_atomic`'s temporary files are left out.
fn project_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|file| {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            !name.starts_with('.') && name.ends_with(".toml")
        })
        .collect();
    files.sort();
    Ok(files)
}

/// The projects in `dir`, one per file; a file that cannot be read, or names a
/// project another file already has, is a project with an error.
fn load_projects(dir: &Path, home: Option<&Path>) -> Result<Vec<Project>> {
    let mut projects: Vec<Project> = Vec::new();
    for file in project_files(dir)? {
        let read = fs::read_to_string(&file).map_err(|err| err.to_string());
        let parsed = read.and_then(|text| toml::from_str(&text).map_err(|err| err.to_string()));
        let mut project = match parsed {
            Ok(parsed) => resolve_project(parsed, home, &file),
            Err(message) => Project {
                path: file.clone(),
                base: None,
                worktree_dir: None,
                on_create: None,
                error: Some(format!("{}: {message}", file.display())),
                file,
            },
        };
        if let Some(first) = projects.iter().find(|p| p.path == project.path) {
            let first = first.file.display();
            project
                .error
                .get_or_insert(format!("the same project as {first}"));
        }
        projects.push(project);
    }
    Ok(projects)
}

/// What `add_project` did, and the project's file.
#[derive(Debug, PartialEq)]
pub enum Added {
    Added(PathBuf),
    /// A project already has this path; nothing was written.
    AlreadyThere(PathBuf),
}

impl Added {
    /// What `tatou add` says about `repo`.
    pub fn message(&self, repo: &Path, home: Option<&Path>) -> String {
        let repo = tilde(repo, home);
        match self {
            Added::Added(file) => format!("Added {repo}: {}", tilde(file, home)),
            Added::AlreadyThere(file) => {
                format!("{repo} is already a project: {}", tilde(file, home))
            }
        }
    }
}

/// Adds `repo` to the projects next to `<config dir>/config.toml`, first writing
/// the config's template if it does not exist; see `add_project_to`.
pub fn add_project(repo: &Path) -> Result<Added> {
    let path = path()?;
    write_template(&path)?;
    let home = env::var_os("HOME").map(PathBuf::from);
    add_project_to(&projects_dir(&path), repo, home.as_deref())
}

/// Writes `<dir>/<repo's folder name>.toml` with `repo`, a main worktree, as its
/// `path`, unless a project already has its canonical path. A name already taken
/// gets `-2`, `-3`, …; the file must load before it is written.
pub fn add_project_to(dir: &Path, repo: &Path, home: Option<&Path>) -> Result<Added> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let projects = load_projects(dir, home)?;
    if let Some(project) = projects.into_iter().find(|project| project.path == repo) {
        return Ok(Added::AlreadyThere(project.file));
    }
    let name = match repo.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => "project".to_string(),
    };
    let mut file = dir.join(format!("{name}.toml"));
    let mut n = 1;
    while fs::symlink_metadata(&file).is_ok() {
        n += 1;
        file = dir.join(format!("{name}-{n}.toml"));
    }
    let quoted = toml::Value::String(tilde(&repo, home)).to_string();
    let text = format!("path = {quoted}\n");
    let parsed = toml::from_str(&text).map_err(|err: toml::de::Error| Error::Config {
        path: file.clone(),
        message: err.to_string(),
    })?;
    if let Some(message) = resolve_project(parsed, home, &file).error {
        return Err(Error::Config {
            path: file,
            message,
        });
    }
    write_atomic(&file, &text)?;
    Ok(Added::Added(file))
}

/// `path` as written in the config and shown to the user: under `$HOME`, with `~/`.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    // git prints paths with symlinks resolved, which `$HOME` may not be
    let homes = home
        .into_iter()
        .flat_map(|home| [Some(home.to_path_buf()), home.canonicalize().ok()])
        .flatten();
    for home in homes {
        match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => return "~".to_string(),
            Ok(rest) => return format!("~/{}", rest.display()),
            Err(_) => {}
        }
    }
    path.display().to_string()
}

/// Writes `text` to a temporary file beside `path` and renames it over `path`, so
/// that nothing ever reads a half-written file. Through a symlink (a config kept in
/// a dotfiles repository), the file it points to is replaced, with its permissions.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let io_error = |source| Error::Io {
        path: path.clone(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_error)?;
    }
    // The pid keeps two tatou processes at the same time from sharing a temporary file
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", process::id()));
    let tmp = PathBuf::from(tmp);
    let result = fs::write(&tmp, text)
        .and_then(|()| match fs::metadata(&path) {
            Ok(meta) => fs::set_permissions(&tmp, meta.permissions()),
            Err(_) => Ok(()),
        })
        .and_then(|()| fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_error)
}

/// Creates the template at `path`, unless a file is already there.
fn write_template(path: &Path) -> Result<()> {
    let io_error = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_error)?;
    }
    match fs::File::create_new(path) {
        Ok(mut file) => file.write_all(TEMPLATE.as_bytes()).map_err(io_error),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(io_error(err)),
    }
}

/// The settings in `config.toml`; projects are loaded from their own files.
fn parse(text: &str, path: &Path) -> std::result::Result<Config, String> {
    let file: File = toml::from_str(text).map_err(|err| err.to_string())?;
    if file.project.is_some() {
        return Err(format!(
            "[[project]] is no longer read from config.toml: each project is a file in {}, \
             written by `tatou add <path>`",
            projects_dir(path).display()
        ));
    }

    // No shell among them: `t` opens shells, `s` and `n` start agents
    let mut agents = BTreeMap::from([
        ("claude".to_string(), "claude".to_string()),
        ("codex".to_string(), "codex".to_string()),
    ]);
    agents.extend(file.agents);
    let default_agent = file.default_agent.unwrap_or_else(|| "claude".into());
    if !agents.contains_key(&default_agent) {
        let names: Vec<&str> = agents.keys().map(String::as_str).collect();
        return Err(format!(
            "default_agent = \"{default_agent}\" is not one of the agents: {}",
            names.join(", ")
        ));
    }

    Ok(Config {
        path: path.to_path_buf(),
        default_agent,
        show_ignored: file.show_ignored,
        tmux: file.tmux,
        agents,
        keys: file.keys,
        projects: Vec::new(),
    })
}

/// Expands `~` in the project's paths and canonicalises its directory; problems
/// are kept on the project so the others still load.
fn resolve_project(project: ProjectFile, home: Option<&Path>, file: &Path) -> Project {
    let mut error = None;
    let path = match expand_tilde(&project.path, home) {
        Ok(path) => match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(err) => {
                error = Some(format!("{}: {err}", path.display()));
                path
            }
        },
        Err(message) => {
            error = Some(message);
            PathBuf::from(&project.path)
        }
    };
    let worktree_dir = match project.worktree_dir.map(|dir| expand_tilde(&dir, home)) {
        Some(Ok(dir)) => Some(dir),
        Some(Err(message)) => {
            error.get_or_insert(message);
            None
        }
        None => None,
    };
    Project {
        path,
        base: project.base,
        worktree_dir,
        on_create: project.on_create,
        error,
        file: file.to_path_buf(),
    }
}

/// `~` and `~/…` become the home directory; other paths must be absolute.
fn expand_tilde(path: &str, home: Option<&Path>) -> std::result::Result<PathBuf, String> {
    let rest = match path.strip_prefix('~') {
        Some("") => Some(""),
        Some(rest) => rest.strip_prefix('/'),
        None => None,
    };
    match (rest, home) {
        (Some(rest), Some(home)) => Ok(home.join(rest)),
        (Some(_), None) => Err(format!("{path}: cannot expand ~, $HOME is not set")),
        (None, _) if Path::new(path).is_absolute() => Ok(PathBuf::from(path)),
        (None, _) => Err(format!("{path}: must be an absolute path or start with ~/")),
    }
}

/// `$XDG_CONFIG_HOME/tatou`, or `~/.config/tatou`; the same on every platform, macOS included.
pub fn config_dir() -> Result<PathBuf> {
    Ok(config_home()?.join("tatou"))
}

/// `$XDG_STATE_HOME/tatou`, or `~/.local/state/tatou`; holds the log file.
pub fn state_dir() -> Result<PathBuf> {
    let base = xdg_base(
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
        ".local/state",
    )?;
    Ok(base.join("tatou"))
}

/// `$XDG_CONFIG_HOME`, or `~/.config`; also where tmux looks for the user's `tmux/tmux.conf`.
pub fn config_home() -> Result<PathBuf> {
    xdg_base(
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
        ".config",
    )
}

/// `$HOME`, which must be absolute.
pub fn home_dir() -> Result<PathBuf> {
    absolute_home(env::var_os("HOME"))
}

/// An XDG base directory. Following the XDG spec, the variable is ignored when it
/// is empty or relative, and `$HOME/<default>` is used.
fn xdg_base(var: Option<OsString>, home: Option<OsString>, default: &str) -> Result<PathBuf> {
    match var.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        _ => Ok(absolute_home(home)?.join(default)),
    }
}

fn absolute_home(home: Option<OsString>) -> Result<PathBuf> {
    match home.map(PathBuf::from) {
        Some(home) if home.is_absolute() => Ok(home),
        _ => Err(Error::NoHome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(var: Option<&str>, home: Option<&str>) -> Result<PathBuf> {
        xdg_base(var.map(OsString::from), home.map(OsString::from), ".config")
    }

    fn parse_ok(text: &str) -> Config {
        parse(text, Path::new("/c/config.toml")).unwrap()
    }

    fn parse_err(text: &str) -> String {
        parse(text, Path::new("/c/config.toml")).unwrap_err()
    }

    /// The projects loaded from `files`, written as (name, text) in a `projects/`
    /// folder, with `/home/me` as `$HOME`.
    fn projects(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<Project>) {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        fs::create_dir(&projects).unwrap();
        for (name, text) in files {
            fs::write(projects.join(name), text).unwrap();
        }
        let loaded = load_projects(&projects, Some(Path::new("/home/me"))).unwrap();
        (dir, loaded)
    }

    #[test]
    fn uses_the_xdg_variable_when_absolute() {
        let got = dir(Some("/xdg/config"), Some("/home/me")).unwrap();
        assert_eq!(got, PathBuf::from("/xdg/config"));
    }

    #[test]
    fn falls_back_to_home_when_unset_empty_or_relative() {
        for var in [None, Some(""), Some("relative/config")] {
            let got = dir(var, Some("/home/me")).unwrap();
            assert_eq!(got, PathBuf::from("/home/me/.config"), "{var:?}");
        }
    }

    #[test]
    fn fails_without_an_absolute_home() {
        for home in [None, Some(""), Some("relative/home")] {
            assert!(matches!(dir(None, home), Err(Error::NoHome)), "{home:?}");
        }
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = parse_ok("");
        assert_eq!(config.path, PathBuf::from("/c/config.toml"));
        assert_eq!(config.default_agent, "claude");
        assert!(!config.show_ignored);
        assert_eq!(config.tmux, Tmux::default());
        assert_eq!(
            config.agents.keys().collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        assert_eq!(config.keys, Keys::default());
        assert_eq!(config.projects, []);
    }

    #[test]
    fn the_template_gives_the_defaults() {
        assert_eq!(parse_ok(TEMPLATE), parse_ok(""));
    }

    #[test]
    fn the_design_example_parses() {
        let config = parse_ok(
            r#"
            default_agent = "claude"
            show_ignored = true

            [tmux]
            popup = "M-g"
            back  = "M-d"
            prefix = "C-a"
            inherit_user_conf = true

            [agents]
            claude = "claude"
            codex  = "codex"

            [keys]
            jump = "Enter"
            new_worktree = "n"
            send_to_agent = "@"
            [keys.custom]
            "g" = "display-popup -E -d {path} lazygit"

            [keys.tmux]
            "M-|" = "split-window -h -c '#{pane_current_path}'"
            "#,
        );
        assert!(config.show_ignored);
        assert_eq!(config.tmux.prefix.as_deref(), Some("C-a"));
        assert!(config.tmux.inherit_user_conf);
        assert_eq!(config.keys.actions["send_to_agent"], "@");
        assert_eq!(config.keys.actions.len(), 3);
        assert_eq!(
            config.keys.custom["g"],
            "display-popup -E -d {path} lazygit"
        );
        assert_eq!(
            config.keys.tmux["M-|"],
            "split-window -h -c '#{pane_current_path}'"
        );
    }

    #[test]
    fn a_project_file_has_the_projects_settings() {
        let text = "path = \"/\"\nbase = \"main\"\nworktree_dir = \"~/worktrees/my-app\"\n\
                    on_create = \"cp ../my-app/.env . && npm install\"\n";
        let (dir, projects) = projects(&[("my-app.toml", text)]);
        let project = &projects[0];
        assert_eq!(project.file, dir.path().join("projects/my-app.toml"));
        assert_eq!(project.path, PathBuf::from("/"));
        assert_eq!(project.base.as_deref(), Some("main"));
        assert_eq!(
            project.worktree_dir,
            Some(PathBuf::from("/home/me/worktrees/my-app"))
        );
        assert_eq!(
            project.on_create.as_deref(),
            Some("cp ../my-app/.env . && npm install")
        );
        assert_eq!(project.error, None);
    }

    #[test]
    fn unknown_keys_are_errors_with_their_line() {
        let message = parse_err("show_ignored = true\nshow_hidden = true\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("show_hidden"), "{message}");

        let message = parse_err("[tmux]\npopup_key = \"M-g\"\n");
        assert!(message.contains("popup_key"), "{message}");

        let (_dir, projects) = projects(&[("app.toml", "path = \"/\"\nbranch = \"main\"\n")]);
        let message = projects[0].error.as_deref().unwrap();
        assert!(message.contains("app.toml: "), "{message}");
        assert!(message.contains("branch"), "{message}");
    }

    #[test]
    fn projects_in_config_toml_point_to_their_files() {
        let message = parse_err("[[project]]\npath = \"/\"\n");
        assert!(
            message.starts_with("[[project]] is no longer read from config.toml"),
            "{message}"
        );
        assert!(message.contains("/c/projects"), "{message}");
    }

    #[test]
    fn projects_come_from_their_toml_files_by_name() {
        let (_dir, projects) = projects(&[
            ("web.toml", "path = \"/tmp\"\n"),
            ("app.toml", "path = \"/\"\n"),
            (".hidden.toml", "path = \"/\"\n"),
            ("app.toml~", "path = \"/\"\n"),
            ("app.toml.123.tmp", "path = \"/\"\n"),
            ("notes.txt", "hello"),
        ]);
        let names: Vec<_> = projects
            .iter()
            .map(|p| p.file.file_name().unwrap())
            .collect();
        assert_eq!(names, ["app.toml", "web.toml"]);
    }

    #[test]
    fn no_projects_folder_is_no_projects() {
        let dir = tempfile::tempdir().unwrap();
        let none = load_projects(&dir.path().join("projects"), None).unwrap();
        assert_eq!(none, []);
    }

    #[test]
    fn two_files_with_one_path_mark_the_second() {
        let (dir, projects) =
            projects(&[("a.toml", "path = \"/\"\n"), ("b.toml", "path = \"/\"\n")]);
        assert_eq!(projects[0].error, None);
        let first = dir.path().join("projects/a.toml");
        let expected = format!("the same project as {}", first.display());
        assert_eq!(projects[1].error, Some(expected));
    }

    #[test]
    fn agents_are_merged_with_the_built_in_ones() {
        let config = parse_ok("[agents]\naider = \"aider\"\nclaude = \"claude --verbose\"\n");
        assert_eq!(config.agents["aider"], "aider");
        assert_eq!(config.agents["claude"], "claude --verbose");
        assert_eq!(config.agents.len(), 3);
        assert!(!config.agents.contains_key("shell"), "no built-in shell");
    }

    #[test]
    fn a_shell_can_still_be_an_agent_of_ones_own() {
        let config = parse_ok("default_agent = \"shell\"\n[agents]\nshell = \"$SHELL\"\n");
        assert_eq!(config.default_agent, "shell");
        assert_eq!(config.agents["shell"], "$SHELL");
    }

    #[test]
    fn default_agent_must_be_an_agent() {
        let message = parse_err("default_agent = \"aider\"\n");
        assert!(
            message.contains("not one of the agents: claude, codex"),
            "{message}"
        );
        let config = parse_ok("default_agent = \"aider\"\n[agents]\naider = \"aider\"\n");
        assert_eq!(config.default_agent, "aider");
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(expand_tilde("~", home).unwrap(), PathBuf::from("/home/me"));
        assert_eq!(
            expand_tilde("~/code/app", home).unwrap(),
            PathBuf::from("/home/me/code/app")
        );
        assert_eq!(
            expand_tilde("/abs/~/x", home).unwrap(),
            PathBuf::from("/abs/~/x")
        );
        assert!(expand_tilde("~other/app", home).is_err());
        assert!(expand_tilde("code/app", home).is_err());
        assert!(expand_tilde("~/code", None).is_err());
    }

    #[test]
    fn project_paths_are_canonicalised() {
        let repo = tempfile::tempdir().unwrap();
        let text = format!("path = \"{}/./\"\n", repo.path().display());
        let (_dir, projects) = projects(&[("repo.toml", &text)]);
        assert_eq!(projects[0].path, repo.path().canonicalize().unwrap());
        assert_eq!(projects[0].error, None);
    }

    #[test]
    fn a_bad_project_only_marks_that_project() {
        let (_dir, projects) = projects(&[
            ("a.toml", "path = \"/tatou/no/such/dir\"\n"),
            ("b.toml", "path = \"relative/dir\"\n"),
            ("c.toml", "path = \"/\"\n"),
            ("d.toml", "path = ["),
        ]);
        let errors: Vec<_> = projects.iter().map(|p| p.error.is_some()).collect();
        assert_eq!(errors, [true, true, false, true]);
        let missing = projects[0].error.as_deref().unwrap();
        assert!(missing.starts_with("/tatou/no/such/dir: "), "{missing}");
        assert_eq!(projects[0].path, PathBuf::from("/tatou/no/such/dir"));
        // A file that does not parse stands for itself
        assert_eq!(projects[3].path, projects[3].file);
    }

    #[test]
    fn a_config_loads_its_projects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "").unwrap();
        fs::create_dir(dir.path().join("projects")).unwrap();
        fs::write(dir.path().join("projects/root.toml"), "path = \"/\"\n").unwrap();
        let config = load_from(&path, None).unwrap();
        assert_eq!(config.projects[0].path, PathBuf::from("/"));
    }

    #[test]
    fn the_stamp_changes_with_the_config_and_its_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let at = |file: &Path, secs| {
            let file = fs::File::options().write(true).open(file).unwrap();
            let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            file.set_modified(time).unwrap();
        };
        fs::write(&path, "").unwrap();
        at(&path, 1_000);
        let first = stamp(&path);
        assert_eq!(stamp(&path), first, "unchanged");

        let projects = dir.path().join("projects");
        fs::create_dir(&projects).unwrap();
        let app = projects.join("app.toml");
        fs::write(&app, "path = \"/\"\n").unwrap();
        at(&app, 2_000);
        let added = stamp(&path);
        assert_ne!(added, first, "a file added");
        at(&app, 3_000);
        assert_ne!(stamp(&path), added, "a file edited");
        fs::remove_file(&app).unwrap();
        assert_eq!(stamp(&path), first, "removed again");
        at(&path, 4_000);
        assert_ne!(stamp(&path), first, "config.toml edited");
    }

    #[test]
    fn a_missing_file_gets_the_template_and_an_existing_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tatou/config.toml");
        write_template(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        let config = load_from(&path, Some(Path::new("/home/me"))).unwrap();
        assert_eq!(config.projects, []);

        fs::write(&path, "show_ignored = true\n").unwrap();
        write_template(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "show_ignored = true\n");
    }

    #[test]
    fn an_invalid_file_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "show_ignored = maybe\n").unwrap();
        let err = load_from(&path, None).unwrap_err();
        assert!(matches!(err, Error::Config { path: ref p, .. } if *p == path));
        assert!(err.to_string().contains("line 1"), "{err}");
    }

    /// A directory in `home` to add as a project, canonical like git prints it.
    fn repo(home: &Path, name: &str) -> PathBuf {
        let repo = home.join(name);
        fs::create_dir_all(&repo).unwrap();
        repo.canonicalize().unwrap()
    }

    #[test]
    fn adding_a_project_writes_its_file() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let dir = home.join(".config/tatou/projects");
        let app = repo(home, "code/my app");
        let file = dir.join("my app.toml");
        let added = add_project_to(&dir, &app, Some(home)).unwrap();
        assert_eq!(added, Added::Added(file.clone()));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "path = \"~/code/my app\"\n"
        );
        let projects = load_projects(&dir, Some(home)).unwrap();
        assert_eq!(projects[0].path, app);
        assert_eq!(projects[0].error, None);
        assert_eq!(
            added.message(&app, Some(home)),
            "Added ~/code/my app: ~/.config/tatou/projects/my app.toml"
        );
    }

    #[test]
    fn adding_the_same_project_again_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let dir = home.join("projects");
        let app = repo(home, "app");
        let file = dir.join("app.toml");
        assert_eq!(
            add_project_to(&dir, &app, Some(home)).unwrap(),
            Added::Added(file.clone())
        );
        // Through a symlink it is the same directory
        let link = home.join("link");
        std::os::unix::fs::symlink(&app, &link).unwrap();
        for again in [&app, &link] {
            let added = add_project_to(&dir, again, Some(home)).unwrap();
            assert_eq!(added, Added::AlreadyThere(file.clone()), "{again:?}");
        }
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["app.toml"]);
        let already = Added::AlreadyThere(file).message(&app, Some(home));
        assert_eq!(already, "~/app is already a project: ~/projects/app.toml");
    }

    #[test]
    fn a_name_already_taken_gets_a_number() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let dir = home.join("projects");
        let first = repo(home, "work/app");
        let second = repo(home, "personal/app");
        add_project_to(&dir, &first, Some(home)).unwrap();
        let added = add_project_to(&dir, &second, Some(home)).unwrap();
        assert_eq!(added, Added::Added(dir.join("app-2.toml")));
        let paths: Vec<_> = load_projects(&dir, Some(home))
            .unwrap()
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert_eq!(paths, [second, first], "by file name");
    }

    #[test]
    fn odd_names_are_written_as_toml() {
        let elsewhere = tempfile::tempdir().unwrap();
        let odd = repo(elsewhere.path(), "it's \"quoted\" \\ here");
        let dir = elsewhere.path().join("projects");
        add_project_to(&dir, &odd, Some(Path::new("/home/me"))).unwrap();
        let projects = load_projects(&dir, None).unwrap();
        assert_eq!(projects[0].path, odd);
        assert_eq!(projects[0].error, None);
    }

    #[test]
    fn home_is_written_with_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            tilde(Path::new("/home/me/code/app"), Some(home)),
            "~/code/app"
        );
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/meow"), Some(home)), "/home/meow");
        assert_eq!(tilde(Path::new("/srv/app"), None), "/srv/app");
    }

    #[test]
    fn write_atomic_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/tmux.conf");
        write_atomic(&path, "old\n").unwrap();
        write_atomic(&path, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["tmux.conf"]);
    }

    #[test]
    fn write_atomic_keeps_a_symlink_and_the_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles/config.toml");
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&target, "old\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomic(&link, "new\n").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
