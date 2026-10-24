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

    #[error("tatou needs tmux 3.2 or later, found `{found}`")]
    TmuxVersion { found: String },

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

    /// `git worktree list` succeeded but listed nothing, which git never does.
    #[error("{}: git lists no worktree", path.display())]
    NoWorktree { path: PathBuf },

    /// The command ran and failed; `stderr` is its own message, shown to the user as is.
    #[error("`{command}` failed ({status}): {stderr}")]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
