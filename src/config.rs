//! Loads and saves `config.toml`.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::{Error, Result};

/// `$XDG_CONFIG_HOME/tatou`, or `~/.config/tatou`; the same on every platform, macOS included.
pub fn config_dir() -> Result<PathBuf> {
    xdg_dir(
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
        ".config",
    )
}

/// `$XDG_STATE_HOME/tatou`, or `~/.local/state/tatou`; holds the log file.
pub fn state_dir() -> Result<PathBuf> {
    xdg_dir(
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
        ".local/state",
    )
}

/// tatou's directory under an XDG base directory. Following the XDG spec, the
/// variable is ignored when it is empty or relative, and `$HOME/<default>` is used.
fn xdg_dir(var: Option<OsString>, home: Option<OsString>, default: &str) -> Result<PathBuf> {
    let base = match var.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => match home.map(PathBuf::from) {
            Some(home) if home.is_absolute() => home.join(default),
            _ => return Err(Error::NoHome),
        },
    };
    Ok(base.join("tatou"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(var: Option<&str>, home: Option<&str>) -> Result<PathBuf> {
        xdg_dir(var.map(OsString::from), home.map(OsString::from), ".config")
    }

    #[test]
    fn uses_the_xdg_variable_when_absolute() {
        let got = dir(Some("/xdg/config"), Some("/home/me")).unwrap();
        assert_eq!(got, PathBuf::from("/xdg/config/tatou"));
    }

    #[test]
    fn falls_back_to_home_when_unset_empty_or_relative() {
        for var in [None, Some(""), Some("relative/config")] {
            let got = dir(var, Some("/home/me")).unwrap();
            assert_eq!(got, PathBuf::from("/home/me/.config/tatou"), "{var:?}");
        }
    }

    #[test]
    fn fails_without_an_absolute_home() {
        for home in [None, Some(""), Some("relative/home")] {
            assert!(matches!(dir(None, home), Err(Error::NoHome)), "{home:?}");
        }
    }
}
