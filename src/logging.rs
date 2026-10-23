//! Logging to `<state dir>/tatou.log`, never to the terminal.
//!
//! Every tatou process (launcher, dashboard, popup, `tatou status`) appends to the
//! same file. It is rotated at startup once it grows past `MAX_SIZE`, so at most
//! two files exist: `tatou.log` and the previous `tatou.log.old`.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::level_filters::LevelFilter;

use crate::config;
use crate::error::{Error, Result};

const MAX_SIZE: u64 = 1024 * 1024;

/// Starts logging to the log file at the level set by `$TATOU_LOG`, or `default`
/// when it is unset or invalid, and returns the file's path.
pub fn init(default: LevelFilter) -> Result<PathBuf> {
    let dir = config::state_dir()?;
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join("tatou.log");
    rotate(&path, &dir.join("tatou.log.old"), MAX_SIZE)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;

    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_max_level(level(std::env::var("TATOU_LOG").ok().as_deref(), default))
        .init();
    Ok(path)
}

/// Moves `path` to `old`, replacing it, when `path` is larger than `max` bytes.
fn rotate(path: &Path, old: &Path, max: u64) -> Result<()> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() > max => match fs::rename(path, old) {
            // Another tatou process starting at the same time rotated it first
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            }),
        },
        _ => Ok(()),
    }
}

/// The log level from `$TATOU_LOG` (`off`, `error`, `warn`, `info`, `debug`, `trace`);
/// `default` when unset or invalid.
fn level(var: Option<&str>, default: LevelFilter) -> LevelFilter {
    var.and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_falls_back_to_the_default() {
        assert_eq!(level(None, LevelFilter::INFO), LevelFilter::INFO);
        assert_eq!(level(Some("loud"), LevelFilter::WARN), LevelFilter::WARN);
    }

    #[test]
    fn level_parses_names_in_any_case() {
        assert_eq!(level(Some("debug"), LevelFilter::WARN), LevelFilter::DEBUG);
        assert_eq!(level(Some("WARN"), LevelFilter::INFO), LevelFilter::WARN);
        assert_eq!(level(Some("off"), LevelFilter::INFO), LevelFilter::OFF);
    }

    #[test]
    fn rotate_keeps_a_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = (dir.path().join("tatou.log"), dir.path().join("old"));
        fs::write(&path, "small").unwrap();
        rotate(&path, &old, 10).unwrap();
        assert!(path.exists());
        assert!(!old.exists());
    }

    #[test]
    fn rotate_moves_a_large_file_over_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = (dir.path().join("tatou.log"), dir.path().join("old"));
        fs::write(&path, "larger than ten bytes").unwrap();
        fs::write(&old, "previous").unwrap();
        rotate(&path, &old, 10).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&old).unwrap(), "larger than ten bytes");
    }

    #[test]
    fn rotate_ignores_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, old) = (dir.path().join("tatou.log"), dir.path().join("old"));
        rotate(&path, &old, 10).unwrap();
        assert!(!old.exists());
    }
}
