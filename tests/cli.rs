//! Runs the tatou binary with each kind of command line.

mod common;

use std::fs;
use std::path::Path;
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
    // Not run inside: the dashboard would open the terminal running the tests
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
        "Added ~/code/app to ~/config/tatou/config.toml\n"
    );
    let config = sandbox.config().join("tatou/config.toml");
    let added = fs::read_to_string(&config).unwrap();
    assert!(added.starts_with("# tatou configuration"), "{added}");
    assert!(
        added.ends_with("\n\n[[project]]\npath = \"~/code/app\"\n"),
        "{added}"
    );

    // The repository's root, and a linked worktree by its path: the same project
    let linked = home.join("code/app.worktrees/fix");
    let from_root = sandbox.tatou(&["add"]).current_dir(&repo).output().unwrap();
    let by_path = run(&mut sandbox.tatou(&["add", linked.to_str().unwrap()]));
    for output in [from_root, by_path] {
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            "~/code/app is already a project in ~/config/tatou/config.toml\n"
        );
    }
    assert_eq!(fs::read_to_string(&config).unwrap(), added);
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
    assert!(
        !sandbox.config().join("tatou/config.toml").exists(),
        "nothing written"
    );
}
