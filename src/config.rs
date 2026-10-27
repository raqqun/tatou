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
use crate::theme::{self, Theme};

/// Written when `config.toml` does not exist: every setting, commented out.
const TEMPLATE: &str = r#"# tatou configuration: https://github.com/raqqun/tatou
# Every setting is optional; the values shown are the defaults.

# Colours of the dashboard: a built-in theme, terminal (the terminal's own
# palette) or monokai-pro, or one of yours, a file in themes/ next to this one.
# `T` in the dashboard chooses one and writes it on this line
# theme = "terminal"

# Override any default key by action name
# [keys]
# jump = "Enter"
# new_worktree = "n"

# Custom actions in the tatou UI: key = shell command template;
# {path} is substituted
# [keys.custom]
# "g" = "lazygit"

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
    pub keys: Keys,
    pub theme: Theme,
    /// From the files in `projects/`, by file name.
    pub projects: Vec<Project>,
}

// Not read yet: the keymap uses it (#41, #42)
#[allow(dead_code)]
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Keys {
    /// `[keys.custom]`: key → shell command template run from the tatou UI.
    pub custom: BTreeMap<String, String>,
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
    #[serde(default)]
    keys: Keys,
    theme: Option<String>,
    /// Where projects were before they moved to `projects/`; an error with a hint.
    project: Option<toml::Value>,
    /// The agents' settings before agents were removed; errors with a hint.
    default_agent: Option<toml::Value>,
    agents: Option<toml::Value>,
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

/// The folder of theme files, next to the config at `path`.
pub fn themes_dir(path: &Path) -> PathBuf {
    path.with_file_name("themes")
}

/// When the config at `path`, its project files and its theme files were last
/// modified, to tell when it needs loading again.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    config: Option<SystemTime>,
    /// By file name; adding or removing a file changes it too.
    projects: Vec<(PathBuf, Option<SystemTime>)>,
    /// As `projects`; any theme's, not only the one in use, which is simpler.
    themes: Vec<(PathBuf, Option<SystemTime>)>,
}

pub fn stamp(path: &Path) -> Stamp {
    let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let files = |dir: PathBuf| -> Vec<(PathBuf, Option<SystemTime>)> {
        let files = toml_files(&dir).unwrap_or_default();
        files
            .into_iter()
            .map(|file| {
                let modified = modified(&file);
                (file, modified)
            })
            .collect()
    };
    Stamp {
        config: modified(path),
        projects: files(projects_dir(path)),
        themes: files(themes_dir(path)),
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
pub fn toml_files(dir: &Path) -> Result<Vec<PathBuf>> {
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
    for file in toml_files(dir)? {
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

/// Sets `theme = "<name>"` in the config at `path`, changing that line only, so
/// that every other setting and comment stays as written; see `with_theme`.
pub fn save_theme(path: &Path, name: &str) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => TEMPLATE.to_string(),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    write_atomic(path, &with_theme(&text, name))
}

/// `text` with its `theme` line set to `name`: the setting's line, else the
/// template's commented one, else a new first line. Only lines before the first
/// table are settings of `config.toml` itself; a `theme` under `[keys]` is an action.
fn with_theme(text: &str, name: &str) -> String {
    let line = format!("theme = {}\n", toml::Value::String(name.to_string()));
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    let top = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let is_theme = |line: &str, commented: bool| {
        let mut line = line.trim_start();
        if commented {
            match line.strip_prefix('#') {
                Some(rest) => line = rest.trim_start(),
                None => return false,
            }
        }
        line.strip_prefix("theme")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let at = (0..top)
        .find(|&i| is_theme(lines[i], false))
        .or_else(|| (0..top).find(|&i| is_theme(lines[i], true)));
    match at {
        Some(i) => lines[i] = &line,
        None => lines.insert(0, &line),
    }
    lines.concat()
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

    if file.default_agent.is_some() || file.agents.is_some() {
        return Err(
            "default_agent and [agents] are no longer read: tatou starts shells, \
                    in which you run any agent; remove them from config.toml"
                .to_string(),
        );
    }

    let name = file.theme.as_deref().unwrap_or(theme::DEFAULT);
    let theme = theme::load(&themes_dir(path), name)?;

    Ok(Config {
        path: path.to_path_buf(),
        keys: file.keys,
        theme,
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

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> Result<PathBuf> {
    xdg_base(
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
        ".config",
    )
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
        assert_eq!(config.keys, Keys::default());
        assert_eq!(config.theme, theme::terminal());
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
            theme = "monokai-pro"

            [keys]
            jump = "Enter"
            new_worktree = "n"
            edit = "e"
            [keys.custom]
            "g" = "lazygit"
            "#,
        );
        assert_eq!(config.keys.actions["edit"], "e");
        assert_eq!(config.keys.actions.len(), 3);
        assert_eq!(config.keys.custom["g"], "lazygit");
        assert_eq!(config.theme.name, "monokai-pro");
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
        let message = parse_err("theme = \"terminal\"\nshow_hidden = true\n");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("show_hidden"), "{message}");

        let message = parse_err("[tmux]\nback = \"M-d\"\n");
        assert!(message.contains("tmux"), "{message}");

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
    fn agents_in_config_toml_are_errors_with_a_hint() {
        for text in [
            "default_agent = \"claude\"\n",
            "[agents]\naider = \"aider\"\n",
        ] {
            let message = parse_err(text);
            assert!(
                message.starts_with("default_agent and [agents] are no longer read"),
                "{message}"
            );
        }
    }

    #[test]
    fn the_theme_is_built_in_or_a_file_in_themes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(dir.path().join("themes")).unwrap();
        fs::write(dir.path().join("themes/mine.toml"), "accent = \"green\"\n").unwrap();
        let theme = |text: &str| parse(text, &path).map(|config| config.theme);
        assert_eq!(
            theme("theme = \"monokai-pro\"").unwrap().name,
            "monokai-pro"
        );
        let mine = theme("theme = \"mine\"").unwrap();
        assert_eq!(mine.accent, ratatui::style::Color::Green);
        let message = theme("theme = \"nope\"").unwrap_err();
        assert_eq!(
            message,
            "theme = \"nope\" is not one of the themes: mine, monokai-pro, terminal"
        );
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
    fn the_stamp_changes_with_the_config_its_project_files_and_themes() {
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
        let edited = stamp(&path);
        assert_ne!(edited, first, "config.toml edited");

        let themes = dir.path().join("themes");
        fs::create_dir(&themes).unwrap();
        let mine = themes.join("mine.toml");
        fs::write(&mine, "").unwrap();
        at(&mine, 5_000);
        let added = stamp(&path);
        assert_ne!(added, edited, "a theme added");
        at(&mine, 6_000);
        assert_ne!(stamp(&path), added, "a theme edited");
    }

    #[test]
    fn a_missing_file_gets_the_template_and_an_existing_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tatou/config.toml");
        write_template(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        let config = load_from(&path, Some(Path::new("/home/me"))).unwrap();
        assert_eq!(config.projects, []);

        fs::write(&path, "theme = \"terminal\"\n").unwrap();
        write_template(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "theme = \"terminal\"\n");
    }

    #[test]
    fn an_invalid_file_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "theme = maybe\n").unwrap();
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
    fn the_theme_line_is_replaced_and_nothing_else() {
        let text = "# mine\ntheme = \"terminal\"  # old\n\n[keys]\ntheme = \"T\"\n";
        assert_eq!(
            with_theme(text, "monokai-pro"),
            "# mine\ntheme = \"monokai-pro\"\n\n[keys]\ntheme = \"T\"\n"
        );
    }

    #[test]
    fn the_templates_theme_line_is_uncommented() {
        let text = with_theme(TEMPLATE, "monokai-pro");
        assert!(text.contains("\ntheme = \"monokai-pro\"\n"), "{text}");
        assert!(!text.contains("# theme ="), "{text}");
        assert_eq!(text.lines().count(), TEMPLATE.lines().count());
        assert_eq!(parse_ok(&text).theme.name, "monokai-pro");
    }

    #[test]
    fn without_a_theme_line_one_is_added_first() {
        let text = "themes = 1\n# theme: mine\n[keys]\ntheme = \"T\"\n# theme = \"x\"\n";
        assert_eq!(
            with_theme(text, "mine"),
            format!("theme = \"mine\"\n{text}"),
            "`themes`, prose and the lines of a table stay"
        );
        assert_eq!(with_theme("", "mine"), "theme = \"mine\"\n");
        assert_eq!(
            with_theme("[keys]\njump = \"Enter\"", "a\"b"),
            "theme = 'a\"b'\n[keys]\njump = \"Enter\""
        );
    }

    #[test]
    fn saving_the_theme_writes_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[keys]\njump = \"Enter\"\n").unwrap();
        save_theme(&path, "monokai-pro").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "theme = \"monokai-pro\"\n[keys]\njump = \"Enter\"\n");
        let config = load_from(&path, None).unwrap();
        assert_eq!(config.theme.name, "monokai-pro");
    }

    #[test]
    fn write_atomic_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/config.toml");
        write_atomic(&path, "old\n").unwrap();
        write_atomic(&path, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
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
