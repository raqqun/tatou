//! Runs the tatou binary with each kind of command line.

mod common;

use std::fs;
use std::process::{Command, Output, Stdio};

use common::Sandbox;

fn run(command: &mut Command) -> Output {
    command.stdin(Stdio::null()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn tmux_installed() -> bool {
    let installed = Command::new("tmux").arg("-V").output().is_ok();
    if !installed {
        eprintln!("tmux is not installed: skipping");
    }
    installed
}

#[test]
fn version_prints_the_version_file() {
    let sandbox = Sandbox::new();
    let expected = format!("tatou {}\n", include_str!("../VERSION").trim());
    for flag in ["--version", "-V"] {
        let output = run(&mut sandbox.tatou(&[flag]));
        assert!(output.status.success(), "{flag}");
        assert_eq!(text(&output.stdout), expected, "{flag}");
        assert_eq!(text(&output.stderr), "", "{flag}");
    }
    // Printing the version or the help does not touch the log
    assert!(!sandbox.state().exists());
}

#[test]
fn help_prints_the_usage() {
    let sandbox = Sandbox::new();
    for flag in ["--help", "-h"] {
        let output = run(&mut sandbox.tatou(&[flag]));
        assert!(output.status.success(), "{flag}");
        assert!(text(&output.stdout).starts_with("Usage: tatou"), "{flag}");
    }
    assert!(!sandbox.state().exists());
}

#[test]
fn a_wrong_argument_prints_the_usage_and_exits_with_2() {
    let sandbox = Sandbox::new();
    for args in [&["--nope"][..], &["status", "extra"]] {
        let output = run(&mut sandbox.tatou(args));
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stdout), "", "{args:?}");
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with("tatou: "), "{stderr}");
        assert!(stderr.contains("Usage: tatou"), "{stderr}");
    }
}

#[test]
fn commands_not_implemented_yet_say_so() {
    let sandbox = Sandbox::new();
    let output = run(&mut sandbox.tatou(&["hooks"]));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "tatou: hooks is not implemented yet (#26)\n"
    );
}

#[test]
fn status_logs_only_problems() {
    let sandbox = Sandbox::new();
    let log = sandbox.state().join("tatou/tatou.log");
    run(&mut sandbox.tatou(&["status"]));
    assert_eq!(fs::read_to_string(&log).unwrap(), "");

    let output = run(sandbox.tatou(&["status"]).env("TATOU_LOG", "info"));
    assert_eq!(output.status.code(), Some(1));
    let log = fs::read_to_string(&log).unwrap();
    assert!(log.contains("started"), "{log}");
}

#[test]
fn the_dashboard_only_runs_inside_tatous_server() {
    let sandbox = Sandbox::new();
    let output = run(&mut sandbox.tatou(&["--dashboard"]));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("run `tatou` to start it"));

    let inside = "/tmp/tmux-501/tatou,1234,0";
    let output = run(sandbox.tatou(&["--dashboard"]).env("TMUX", inside));
    assert!(text(&output.stderr).contains("--dashboard is not implemented yet"));
}

#[test]
fn the_launcher_needs_tmux() {
    let sandbox = Sandbox::new();
    let output = run(sandbox.tatou(&[]).env("PATH", "/nonexistent"));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "tatou: tatou needs tmux 3.2 or later, but `tmux` was not found; install it and try again\n"
    );
}

#[test]
fn the_launcher_stops_on_an_invalid_config() {
    if !tmux_installed() {
        return;
    }
    let sandbox = Sandbox::new();
    let dir = sandbox.config().join("tatou");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), "[tmux]\npopop = \"M-g\"\n").unwrap();
    let output = run(&mut sandbox.tatou(&[]));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("config.toml"), "{stderr}");
    assert!(stderr.contains("popop"), "{stderr}");
    assert!(!dir.join("tmux.conf").exists());
}

#[test]
fn the_launcher_asks_before_nesting_and_quits_without_an_answer() {
    if !tmux_installed() {
        return;
    }
    let sandbox = Sandbox::new();
    let other = "/tmp/tmux-501/default,1234,0";
    let output = run(sandbox.tatou(&[]).env("TMUX", other));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("already inside tmux"), "{stderr}");
    assert!(stderr.ends_with("Attach anyway? [y/N] "), "{stderr}");
    // The config was prepared before asking
    let dir = sandbox.config().join("tatou");
    assert!(dir.join("config.toml").exists());
    assert!(dir.join("tmux.conf").exists());
}
