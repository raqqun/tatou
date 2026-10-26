//! Runs the tatou binary in a sandbox, so that no test reaches the user's config,
//! state or daemon: `$HOME` and the XDG directories, the daemon's socket with
//! them, all point into a temporary directory.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

pub struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        Sandbox {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// `$XDG_CONFIG_HOME`; tatou's config is in its `tatou` directory.
    // Not every test file reads it
    #[allow(dead_code)]
    pub fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    /// `$HOME`.
    // Not every test file reads it
    #[allow(dead_code)]
    pub fn home(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// `$XDG_STATE_HOME`; tatou's log is in its `tatou` directory.
    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    /// tatou with `args`, not run yet.
    pub fn tatou(&self, args: &[&str]) -> Command {
        let dir = self.dir.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_tatou"));
        command
            .args(args)
            .env("HOME", dir)
            .env("XDG_CONFIG_HOME", self.config())
            .env("XDG_STATE_HOME", self.state())
            .env_remove("TATOU_LOG");
        command
    }
}
