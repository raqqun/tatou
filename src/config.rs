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

# Commands that start an agent; added to the built-in claude, codex and shell,
# or replacing one of them when the name is the same
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

# One [[project]] per repository
# [[project]]
# path = "~/code/my-app"
# base = "main"                        # default: the branch checked out in the main worktree
# worktree_dir = "~/worktrees/my-app"  # default: ~/code/my-app.worktrees
# on_create = "cp ../my-app/.env . && npm install"
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
    /// Why the project cannot be used, e.g. its directory is missing; shown next to it.
    pub error: Option<String>,
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
    #[serde(default, rename = "project")]
    projects: Vec<ProjectFile>,
}

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

/// When `path` was last modified; `None` when it cannot be read.
pub fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

pub fn load_from(path: &Path, home: Option<&Path>) -> Result<Config> {
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text, path, home).map_err(|message| Error::Config {
        path: path.to_path_buf(),
        message,
    })
}

/// What `add_project` did.
#[derive(Debug, PartialEq)]
pub enum Added {
    Added,
    /// A project already has this path; nothing was written.
    AlreadyThere,
}

/// Adds `repo` to `<config dir>/config.toml`, first writing the template if it
/// does not exist; see `add_project_to`.
pub fn add_project(repo: &Path) -> Result<Added> {
    let path = path()?;
    write_template(&path)?;
    let home = env::var_os("HOME").map(PathBuf::from);
    add_project_to(&path, repo, home.as_deref())
}

/// Appends a `[[project]]` for `repo`, a main worktree, to the config at `path`,
/// unless a project already has its canonical path. The rest of the file, comments
/// included, is left as written, and the new text must load before it replaces it.
pub fn add_project_to(path: &Path, repo: &Path, home: Option<&Path>) -> Result<Added> {
    let invalid = |message| Error::Config {
        path: path.to_path_buf(),
        message,
    };
    let text = fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let config = parse(&text, path, home).map_err(invalid)?;
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    if config.projects.iter().any(|project| project.path == repo) {
        return Ok(Added::AlreadyThere);
    }
    let new = append_project(&text, &tilde(&repo, home));
    parse(&new, path, home).map_err(invalid)?;
    write_atomic(path, &new)?;
    Ok(Added::Added)
}

/// `text` with a `[[project]]` block for `path` at the end, after a blank line.
fn append_project(text: &str, path: &str) -> String {
    let mut new = text.to_string();
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    if !new.is_empty() && !new.ends_with("\n\n") {
        new.push('\n');
    }
    let quoted = toml::Value::String(path.to_string()).to_string();
    new.push_str(&format!("[[project]]\npath = {quoted}\n"));
    new
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

fn parse(text: &str, path: &Path, home: Option<&Path>) -> std::result::Result<Config, String> {
    let file: File = toml::from_str(text).map_err(|err| err.to_string())?;

    let mut agents = BTreeMap::from([
        ("claude".to_string(), "claude".to_string()),
        ("codex".to_string(), "codex".to_string()),
        ("shell".to_string(), "$SHELL".to_string()),
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

    let projects = file
        .projects
        .into_iter()
        .map(|project| resolve_project(project, home))
        .collect();

    Ok(Config {
        path: path.to_path_buf(),
        default_agent,
        show_ignored: file.show_ignored,
        tmux: file.tmux,
        agents,
        keys: file.keys,
        projects,
    })
}

/// Expands `~` in the project's paths and canonicalises its directory; problems
/// are kept on the project so the others still load.
fn resolve_project(project: ProjectFile, home: Option<&Path>) -> Project {
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
        parse(
            text,
            Path::new("/c/config.toml"),
            Some(Path::new("/home/me")),
        )
        .unwrap()
    }

    fn parse_err(text: &str) -> String {
        parse(
            text,
            Path::new("/c/config.toml"),
            Some(Path::new("/home/me")),
        )
        .unwrap_err()
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
            ["claude", "codex", "shell"]
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
            shell  = "$SHELL"

            [keys]
            jump = "Enter"
            new_worktree = "n"
            send_to_agent = "@"
            [keys.custom]
            "g" = "display-popup -E -d {path} lazygit"

            [keys.tmux]
            "M-|" = "split-window -h -c '#{pane_current_path}'"

            [[project]]
            path = "/"
            base = "main"
            worktree_dir = "~/worktrees/my-app"
            on_create = "cp ../my-app/.env . && npm install"
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
        let project = &config.projects[0];
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

        let message = parse_err("[[project]]\npath = \"/\"\nbranch = \"main\"\n");
        assert!(message.contains("branch"), "{message}");
    }

    #[test]
    fn agents_are_merged_with_the_built_in_ones() {
        let config = parse_ok("[agents]\naider = \"aider\"\nclaude = \"claude --verbose\"\n");
        assert_eq!(config.agents["aider"], "aider");
        assert_eq!(config.agents["claude"], "claude --verbose");
        assert_eq!(config.agents["shell"], "$SHELL");
    }

    #[test]
    fn default_agent_must_be_an_agent() {
        let message = parse_err("default_agent = \"aider\"\n");
        assert!(
            message.contains("not one of the agents: claude, codex, shell"),
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
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let text = format!("[[project]]\npath = \"{}/./repo\"\n", dir.path().display());
        let config = parse_ok(&text);
        assert_eq!(config.projects[0].path, repo.canonicalize().unwrap());
        assert_eq!(config.projects[0].error, None);
    }

    #[test]
    fn a_bad_project_path_only_marks_that_project() {
        let config = parse_ok(
            "[[project]]\npath = \"/tatou/no/such/dir\"\n\
             [[project]]\npath = \"relative/dir\"\n\
             [[project]]\npath = \"/\"\n",
        );
        let errors: Vec<_> = config.projects.iter().map(|p| p.error.is_some()).collect();
        assert_eq!(errors, [true, true, false]);
        let missing = config.projects[0].error.as_deref().unwrap();
        assert!(missing.starts_with("/tatou/no/such/dir: "), "{missing}");
        assert_eq!(config.projects[0].path, PathBuf::from("/tatou/no/such/dir"));
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

    /// A config file in a temporary directory, which also stands for `$HOME`.
    fn config_file(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, text).unwrap();
        (dir, path)
    }

    /// A directory in `home` to add as a project, canonical like git prints it.
    fn repo(home: &Path, name: &str) -> PathBuf {
        let repo = home.join(name);
        fs::create_dir_all(&repo).unwrap();
        repo.canonicalize().unwrap()
    }

    #[test]
    fn adding_a_project_appends_it_and_keeps_the_rest() {
        let original = "# my settings\ndefault_agent = \"codex\" # inline comment\n";
        let (dir, path) = config_file(original);
        let home = dir.path();
        let app = repo(home, "code/my app");
        assert_eq!(
            add_project_to(&path, &app, Some(home)).unwrap(),
            Added::Added
        );

        let text = fs::read_to_string(&path).unwrap();
        let expected = "\n[[project]]\npath = \"~/code/my app\"\n";
        assert_eq!(text, format!("{original}{expected}"));
        let config = load_from(&path, Some(home)).unwrap();
        assert_eq!(config.projects[0].path, app);
        assert_eq!(config.projects[0].error, None);
    }

    #[test]
    fn adding_the_same_project_again_changes_nothing() {
        let (dir, path) = config_file("");
        let home = dir.path();
        let app = repo(home, "app");
        assert_eq!(
            add_project_to(&path, &app, Some(home)).unwrap(),
            Added::Added
        );
        let once = fs::read_to_string(&path).unwrap();
        assert_eq!(
            once, "[[project]]\npath = \"~/app\"\n",
            "no blank line first"
        );
        // Through a symlink it is the same directory
        let link = home.join("link");
        std::os::unix::fs::symlink(&app, &link).unwrap();
        for again in [&app, &link] {
            let added = add_project_to(&path, again, Some(home)).unwrap();
            assert_eq!(added, Added::AlreadyThere, "{again:?}");
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), once);
    }

    #[test]
    fn a_project_outside_home_and_odd_names_are_written_as_toml() {
        let (dir, path) = config_file("[tmux]\nback = \"M-b\"");
        let elsewhere = tempfile::tempdir().unwrap();
        let odd = repo(elsewhere.path(), "it's \"quoted\" \\ here");
        add_project_to(&path, &odd, Some(dir.path())).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("[tmux]\nback = \"M-b\"\n\n[[project]]\n"),
            "a newline, then a blank line: {text}"
        );
        let config = load_from(&path, Some(dir.path())).unwrap();
        assert_eq!(config.projects[0].path, odd);
        assert_eq!(config.tmux.back, "M-b", "still in [tmux]");
    }

    #[test]
    fn an_invalid_config_is_left_alone() {
        let (dir, path) = config_file("popop = 1\n");
        let app = repo(dir.path(), "app");
        let err = add_project_to(&path, &app, Some(dir.path())).unwrap_err();
        assert!(matches!(err, Error::Config { .. }), "{err:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "popop = 1\n");
    }

    #[test]
    fn a_config_that_would_not_load_with_the_project_is_left_alone() {
        // Valid, but an inline array cannot be extended with a [[project]] table
        let original = "project = [{ path = \"/tmp\" }]\n";
        let (dir, path) = config_file(original);
        let app = repo(dir.path(), "app");
        let err = add_project_to(&path, &app, Some(dir.path())).unwrap_err();
        assert!(matches!(err, Error::Config { .. }), "{err:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
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
