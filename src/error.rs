//! The crate-wide error type.

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot find the home directory: $HOME is not set to an absolute path")]
    NoHome,

    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

pub type Result<T> = std::result::Result<T, Error>;
