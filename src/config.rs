//! Loads and saves `config.toml`.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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

#[derive(Debug, PartialEq, Deserialize)]
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

/// Loads `<config dir>/config.toml`, first writing the commented template if it
/// does not exist.
pub fn load() -> Result<Config> {
    let path = config_dir()?.join("config.toml");
    write_template(&path)?;
    let home = env::var_os("HOME").map(PathBuf::from);
    load_from(&path, home.as_deref())
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
}
