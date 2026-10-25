//! `tatou` with no argument: checks git, offers to add the repository it is
//! started in, starts the daemon unless it runs, then runs the dashboard in this
//! terminal.

use std::env;
use std::io::{self, ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::client::Client;
use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::exec;
use crate::git;
use crate::ui;

pub fn run() -> ExitCode {
    match launch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "launch failed");
            eprintln!("tatou: {err}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> Result<()> {
    check_git()?;
    let config = config::load()?;
    offer_to_add(&config);
    let client = Client::daemon()?;
    client.start_daemon()?;
    ui::run(client)
}

/// git must be installed; gh is optional.
fn check_git() -> Result<()> {
    let git = exec::run("git", &["--version"], None).map_err(|err| match err {
        Error::Spawn { source, .. } if source.kind() == ErrorKind::NotFound => Error::Missing {
            program: "git",
            needs: "git",
        },
        err => err,
    })?;
    tracing::info!(git = git.stdout.trim(), "tools");
    Ok(())
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
    use std::fs;

    use super::*;

    #[test]
    fn only_yes_is_yes() {
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
