//! `tatou` with no argument: prepares tatou's tmux server and attaches the
//! terminal to the dashboard session, starting both when needed.

use std::env;
use std::fs;
use std::io::{self, ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::exec;
use crate::git;
use crate::tmux::{self, Nesting, Server};

pub fn run() -> ExitCode {
    match launch() {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(%err, "launch failed");
            eprintln!("tatou: {err}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> Result<ExitCode> {
    check_tools()?;
    let config = config::load()?;

    let conf = tmux::conf_path()?;
    // Read before it is replaced: the server still has the bindings it made
    let previous = fs::read_to_string(&conf).ok();
    tmux::write_conf(&config)?;
    let server = Server::tatou(&conf);
    server.reload(previous.as_deref())?;

    let dashboard = tmux::dashboard_command();

    let nesting = tmux::nesting();
    if nesting == Nesting::Other && !confirm_nesting() {
        return Ok(ExitCode::FAILURE);
    }
    offer_to_add(&config);
    if nesting == Nesting::Tatou {
        switch_to_dashboard(&server, &dashboard)?;
        return Ok(ExitCode::SUCCESS);
    }
    tracing::info!("attaching to the dashboard");
    let args = server.args(&[
        "new-session",
        "-A",
        "-s",
        tmux::DASHBOARD,
        "-n",
        tmux::DASHBOARD,
        &dashboard,
    ]);
    // TMUX is unset so that tmux agrees to attach inside another tmux
    Err(exec::exec_replace("tmux", &args, &["TMUX"]))
}

/// tmux (recent enough) and git must be installed; gh is optional.
fn check_tools() -> Result<()> {
    let missing = |program, needs| {
        move |err| match err {
            Error::Spawn { source, .. } if source.kind() == ErrorKind::NotFound => {
                Error::Missing { program, needs }
            }
            err => err,
        }
    };
    let tmux = tmux::check_version().map_err(missing("tmux", "tmux 3.2 or later"))?;
    let git = exec::run("git", &["--version"], None).map_err(missing("git", "git"))?;
    tracing::info!(tmux, git = git.stdout.trim(), "tools");
    Ok(())
}

/// Already on tatou's server: moves this client to the dashboard, starting it again
/// if its session was closed.
fn switch_to_dashboard(server: &Server, dashboard: &str) -> Result<()> {
    let target = format!("={}", tmux::DASHBOARD);
    if server.run(&["has-session", "-t", &target]).is_err() {
        let name = tmux::DASHBOARD;
        server.run(&["new-session", "-d", "-s", name, "-n", name, dashboard])?;
    }
    server.run(&["switch-client", "-t", &target])?;
    Ok(())
}

/// Inside another tmux server, attaching would nest two tmux clients in one
/// terminal. Asks before doing it; anything but yes quits.
fn confirm_nesting() -> bool {
    eprint!(
        "tatou: this terminal is already inside tmux. Attaching runs tatou's tmux \
         inside it, with two prefix keys and two status bars; tatou works best in a \
         terminal tab of its own.\nAttach anyway? [y/N] "
    );
    let mut answer = String::new();
    // No answer (end of input, no terminal) means no
    let _ = io::stdin().read_line(&mut answer);
    is_yes(&answer)
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

/// Started in a repository that is not a project yet: asks whether to add it, and
/// adds it on yes, as `tatou add` does. A failure is shown, and the launch goes on.
fn offer_to_add(config: &Config) {
    let terminal = io::stdin().is_terminal();
    // git runs only when someone can answer
    let here = terminal.then(|| git::main_worktree(Path::new(".")).ok());
    let Some(repo) = to_offer(config, here.flatten(), terminal) else {
        return;
    };
    let home = env::var_os("HOME").map(PathBuf::from);
    eprint!(
        "Add {} to tatou? [Y/n] ",
        config::tilde(&repo, home.as_deref())
    );
    let mut answer = String::new();
    // No answer (end of input) means no
    if io::stdin().read_line(&mut answer).unwrap_or(0) == 0 || !is_yes_or_enter(&answer) {
        return;
    }
    match config::add_project(&repo) {
        Ok(added) => {
            tracing::info!(?repo, ?added, "added at launch");
            eprintln!("{}", added.message(&repo, home.as_deref()));
        }
        Err(err) => {
            tracing::warn!(%err, "cannot add at launch");
            eprintln!("tatou: {err}");
        }
    }
}

/// The repository to offer: `here`, the main worktree of the current folder's
/// repository, when it is not a project yet and a terminal can answer.
fn to_offer(config: &Config, here: Option<PathBuf>, terminal: bool) -> Option<PathBuf> {
    let repo = here.filter(|_| terminal)?;
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.clone());
    let known = config
        .projects
        .iter()
        .any(|project| project.path == canonical);
    (!known).then_some(repo)
}

/// Yes is the default: an empty answer is yes.
fn is_yes_or_enter(answer: &str) -> bool {
    answer.trim().is_empty() || is_yes(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_attaches() {
        for answer in ["y\n", "Y\n", " yes \n", "YES"] {
            assert!(is_yes(answer), "{answer:?}");
        }
        for answer in ["", "\n", "n\n", "no\n", "yep\n", "q\n"] {
            assert!(!is_yes(answer), "{answer:?}");
        }
    }

    #[test]
    fn adding_defaults_to_yes() {
        for answer in ["\n", "  \n", "y\n", "yes\n"] {
            assert!(is_yes_or_enter(answer), "{answer:?}");
        }
        for answer in ["n\n", "no\n", "q\n"] {
            assert!(!is_yes_or_enter(answer), "{answer:?}");
        }
    }

    #[test]
    fn offers_a_repository_only_when_it_is_new_and_someone_can_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (known, new) = (dir.path().join("known"), dir.path().join("new"));
        fs::create_dir(&known).unwrap();
        fs::create_dir(&new).unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "").unwrap();
        config::add_project_to(&config::projects_dir(&path), &known, None).unwrap();
        let config = config::load_from(&path, None).unwrap();

        assert_eq!(
            to_offer(&config, Some(known.clone()), true),
            None,
            "a project"
        );
        assert_eq!(
            to_offer(&config, Some(new.clone()), true),
            Some(new.clone())
        );
        assert_eq!(to_offer(&config, None, true), None, "outside a repository");
        assert_eq!(to_offer(&config, Some(new), false), None, "no terminal");
    }
}
