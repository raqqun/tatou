//! Runs the tatou binary with each kind of command line.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// Runs tatou with `args`, its state directory in `state`.
fn tatou(args: &[&str], state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tatou"))
        .args(args)
        .env("XDG_STATE_HOME", state)
        .env_remove("TATOU_LOG")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_prints_the_version_file() {
    let state = tempfile::tempdir().unwrap();
    let expected = format!("tatou {}\n", include_str!("../VERSION").trim());
    for flag in ["--version", "-V"] {
        let output = tatou(&[flag], state.path());
        assert!(output.status.success(), "{flag}");
        assert_eq!(text(&output.stdout), expected, "{flag}");
        assert_eq!(text(&output.stderr), "", "{flag}");
    }
    // Printing the version or the help does not touch the log
    assert!(!state.path().join("tatou").exists());
}

#[test]
fn help_prints_the_usage() {
    let state = tempfile::tempdir().unwrap();
    for flag in ["--help", "-h"] {
        let output = tatou(&[flag], state.path());
        assert!(output.status.success(), "{flag}");
        assert!(text(&output.stdout).starts_with("Usage: tatou"), "{flag}");
    }
    assert!(!state.path().join("tatou").exists());
}

#[test]
fn a_wrong_argument_prints_the_usage_and_exits_with_2() {
    let state = tempfile::tempdir().unwrap();
    for args in [&["--nope"][..], &["status", "extra"]] {
        let output = tatou(args, state.path());
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stdout), "", "{args:?}");
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with("tatou: "), "{stderr}");
        assert!(stderr.contains("Usage: tatou"), "{stderr}");
    }
}

#[test]
fn commands_not_implemented_yet_say_so() {
    let state = tempfile::tempdir().unwrap();
    let output = tatou(&["hooks"], state.path());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "tatou: hooks is not implemented yet (#26)\n"
    );
}

#[test]
fn status_logs_only_problems() {
    let state = tempfile::tempdir().unwrap();
    tatou(&["status"], state.path());
    let log = fs::read_to_string(state.path().join("tatou/tatou.log")).unwrap();
    assert_eq!(log, "");

    let output = Command::new(env!("CARGO_BIN_EXE_tatou"))
        .arg("status")
        .env("XDG_STATE_HOME", state.path())
        .env("TATOU_LOG", "info")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let log = fs::read_to_string(state.path().join("tatou/tatou.log")).unwrap();
    assert!(log.contains("started"), "{log}");
}
