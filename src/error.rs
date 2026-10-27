//! The crate-wide error type.

use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot find the home directory: $HOME is not set to an absolute path")]
    NoHome,

    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },

    /// `config.toml` is invalid; `message` says where and why.
    #[error("{}: {message}", path.display())]
    Config { path: PathBuf, message: String },

    /// A program tatou requires is not installed, or not in `$PATH`.
    #[error("tatou needs {needs}, but `{program}` was not found; install it and try again")]
    Missing {
        program: &'static str,
        needs: &'static str,
    },

    /// The terminal could not be set up, drawn on or read from.
    #[error("terminal: {0}")]
    Terminal(io::Error),

    /// The program could not be started, e.g. it is not installed.
    #[error("cannot run {program}: {source}")]
    Spawn { program: String, source: io::Error },

    /// Rejected by `git check-ref-format --branch`, before anything else runs.
    #[error("`{name}` is not a valid branch name")]
    InvalidBranch { name: String },

    /// Removing it would remove the repository itself.
    #[error("{} is the repository's main worktree; it cannot be removed", path.display())]
    MainWorktree { path: PathBuf },

    /// `git worktree list` succeeded but listed nothing, which git never does.
    #[error("{}: git lists no worktree", path.display())]
    NoWorktree { path: PathBuf },

    /// `gh pr list` printed something that is not the JSON it was asked for.
    #[error("cannot read gh's pull requests: {0}")]
    PrList(String),

    /// The daemon said no, or cannot be reached; its own message.
    #[error("{0}")]
    Daemon(String),

    /// The command ran and failed; `stderr` is its own message, shown to the user as is.
    #[error("`{command}` failed ({status}): {stderr}")]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
