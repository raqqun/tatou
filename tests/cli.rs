//! Runs the tatou binary with each kind of command line.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::Sandbox;

fn run(command: &mut Command) -> Output {
    command.stdin(Stdio::null()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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
    for args in [&["--nope"][..], &["stop", "extra"]] {
        let output = run(&mut sandbox.tatou(args));
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stdout), "", "{args:?}");
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with("tatou: "), "{stderr}");
        assert!(stderr.contains("Usage: tatou"), "{stderr}");
    }
}

#[test]
fn the_launcher_needs_git() {
    let sandbox = Sandbox::new();
    let output = run(sandbox.tatou(&[]).env("PATH", "/nonexistent"));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "tatou: tatou needs git, but `git` was not found; install it and try again\n"
    );
}

#[test]
fn the_launcher_stops_on_an_invalid_config() {
    let sandbox = Sandbox::new();
    let dir = sandbox.config().join("tatou");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), "[tmux]\nback = \"M-d\"\n").unwrap();
    let output = run(&mut sandbox.tatou(&[]));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("config.toml"), "{stderr}");
    assert!(stderr.contains("tmux"), "{stderr}");
    // No daemon was started
    assert!(!sandbox.state().join("tatou/tatou.sock").exists());
}

#[test]
fn stop_without_a_daemon_does_nothing() {
    let sandbox = Sandbox::new();
    let output = run(&mut sandbox.tatou(&["stop"]));
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[test]
fn the_daemon_listens_until_stopped() {
    let sandbox = Sandbox::new();
    let socket = sandbox.state().join("tatou/tatou.sock");
    let mut daemon = sandbox
        .tatou(&["daemon"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !socket.exists() {
        assert!(started.elapsed() < Duration::from_secs(5), "no socket");
        thread::sleep(Duration::from_millis(20));
    }
    // A second one refuses: one daemon per user
    let second = run(&mut sandbox.tatou(&["daemon"]));
    assert_eq!(second.status.code(), Some(1));
    assert!(text(&second.stderr).contains("already running"));

    let output = run(&mut sandbox.tatou(&["stop"]));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let status = daemon.wait().unwrap();
    assert!(status.success());
    assert!(!socket.exists());
}

/// Runs git in `dir` without the user's or the system's config.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "tatou")
        .env("GIT_AUTHOR_EMAIL", "tatou@example.com")
        .env("GIT_COMMITTER_NAME", "tatou")
        .env("GIT_COMMITTER_EMAIL", "tatou@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        text(&output.stderr)
    );
}

#[test]
fn add_adds_the_main_worktree_once_from_anywhere_in_it() {
    let sandbox = Sandbox::new();
    let home = sandbox.home();
    let repo = home.join("code/app");
    fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "fix", "../app.worktrees/fix"],
    );

    // From a folder inside it, with no config yet: the template is written first
    let output = run(sandbox.tatou(&["add"]).current_dir(repo.join("src")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "Added ~/code/app: ~/config/tatou/projects/app.toml\n"
    );
    let config = sandbox.config().join("tatou/config.toml");
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .starts_with("# tatou configuration")
    );
    let file = sandbox.config().join("tatou/projects/app.toml");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "path = \"~/code/app\"\n"
    );

    // The repository's root, and a linked worktree by its path: the same project
    let linked = home.join("code/app.worktrees/fix");
    let from_root = sandbox.tatou(&["add"]).current_dir(&repo).output().unwrap();
    let by_path = run(&mut sandbox.tatou(&["add", linked.to_str().unwrap()]));
    for output in [from_root, by_path] {
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            "~/code/app is already a project: ~/config/tatou/projects/app.toml\n"
        );
    }
    let files = fs::read_dir(sandbox.config().join("tatou/projects")).unwrap();
    assert_eq!(files.count(), 1);
}

#[test]
fn add_outside_a_repository_says_why() {
    let sandbox = Sandbox::new();
    let output = run(sandbox.tatou(&["add"]).current_dir(sandbox.home()));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("tatou: cannot add .: fatal: not a git repository"),
        "{stderr}"
    );
    assert!(!sandbox.config().join("tatou").exists(), "nothing written");
}
