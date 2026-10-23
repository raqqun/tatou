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

    /// The program could not be started, e.g. it is not installed.
    #[error("cannot run {program}: {source}")]
    Spawn { program: String, source: io::Error },

    /// The command ran and failed; `stderr` is its own message, shown to the user as is.
    #[error("`{command}` failed ({status}): {stderr}")]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
