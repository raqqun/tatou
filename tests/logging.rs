//! Runs the tatou binary with its state directory in a temporary directory.

use std::fs;
use std::process::Command;

#[test]
fn startup_writes_to_the_log_file() {
    let state = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tatou"))
        .env("XDG_STATE_HOME", state.path())
        .env_remove("TATOU_LOG")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);

    let log = fs::read_to_string(state.path().join("tatou/tatou.log")).unwrap();
    assert!(log.contains("INFO"), "{log}");
    assert!(log.contains("started"), "{log}");
}

#[test]
fn an_unusable_state_dir_only_disables_logging() {
    let state = tempfile::tempdir().unwrap();
    // A file where the tatou directory should be makes creating it fail
    fs::write(state.path().join("tatou"), "").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tatou"))
        .env("XDG_STATE_HOME", state.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("logging disabled"), "{stderr}");
}
