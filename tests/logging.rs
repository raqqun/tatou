//! Runs the tatou binary and reads its log file.

mod common;

use std::fs;

use common::Sandbox;

#[test]
fn startup_writes_to_the_log_file() {
    let sandbox = Sandbox::new();
    let output = sandbox.tatou(&["stop"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("logging disabled"), "{stderr}");

    let log = fs::read_to_string(sandbox.state().join("tatou/tatou.log")).unwrap();
    assert!(log.contains("INFO"), "{log}");
    assert!(log.contains("started"), "{log}");
}

#[test]
fn an_unusable_state_dir_only_disables_logging() {
    let sandbox = Sandbox::new();
    // A file where the tatou directory should be makes creating it fail
    fs::create_dir_all(sandbox.state()).unwrap();
    fs::write(sandbox.state().join("tatou"), "").unwrap();
    let output = sandbox.tatou(&["stop"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("logging disabled"), "{stderr}");
    // The command still runs
    assert!(output.status.success(), "{stderr}");
}
