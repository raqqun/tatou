//! The only place that runs external commands.
//!
//! Commands take argument vectors, never shell strings. `run_sh` is the deliberate
//! exception, for the user's `on_create` command and custom tmux-command templates.
//! stdin is closed and git/gh prompts are disabled, so a command run in the
//! background never waits for input.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// What a successful command printed, converted to UTF-8 lossily.
// Not read yet: the tmux and git parsers use it (#7, #9)
#[allow(dead_code)]
#[derive(Debug)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Runs `program` with `args` in `cwd` (or the current directory) and waits for it.
/// A non-zero exit becomes `Error::Command` carrying the command's stderr.
// Not called yet: tmux.rs and git.rs use it (#7, #9)
#[allow(dead_code)]
pub fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    let line = command_line(program, args);
    tracing::debug!(command = %line, cwd = ?cwd, "run");
    let mut command = command(program, args);
    command.stdin(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().map_err(|source| Error::Spawn {
        program: program.to_string(),
        source,
    })?;

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(Error::Command {
            command: line,
            status: output.status,
            stderr: stderr.trim().to_string(),
        });
    }
    Ok(Output {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr,
    })
}

/// Runs `script` with `sh -c`. Only for commands the user wrote in the config, with
/// any substituted values passed through `shell_quote`.
// Not called yet: `on_create` uses it (#21)
#[allow(dead_code)]
pub fn run_sh(script: &str, cwd: Option<&Path>) -> Result<Output> {
    run("sh", &["-c", script], cwd)
}

/// Replaces the current process with `program`, which inherits the terminal.
/// Only returns if the program could not be started.
// Not called yet: the launcher uses it to hand the terminal to tmux (#11)
#[allow(dead_code)]
pub fn exec_replace(program: &str, args: &[&str]) -> Error {
    tracing::debug!(command = %command_line(program, args), "exec");
    let source = command(program, args).exec();
    Error::Spawn {
        program: program.to_string(),
        source,
    }
}

/// Quotes `value` for POSIX `sh` so that it is always read as one literal word.
// Not called yet: command templates use it (#35, #42)
#[allow(dead_code)]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn command(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    // Only for git and gh: a tmux server inherits the environment of the command
    // that starts it, and would pass these on to every agent session
    match program {
        "git" => {
            command.env("GIT_TERMINAL_PROMPT", "0");
        }
        "gh" => {
            command.env("GH_PROMPT_DISABLED", "1");
        }
        _ => {}
    }
    command
}

/// The command as a user would type it, for logs and error messages.
fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = program.to_string();
    for arg in args {
        line.push(' ');
        let plain = !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
        if plain {
            line.push_str(arg);
        } else {
            line.push_str(&shell_quote(arg));
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_captures_stdout() {
        let output = run("echo", &["hello", "world"], None).unwrap();
        assert_eq!(output.stdout, "hello world\n");
        assert_eq!(output.stderr, "");
    }

    #[test]
    fn run_uses_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let output = run("pwd", &[], Some(dir.path())).unwrap();
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(output.stdout.trim_end(), expected.to_str().unwrap());
    }

    #[test]
    fn run_closes_stdin() {
        // cat would wait for input forever if stdin were inherited
        let output = run("cat", &[], None).unwrap();
        assert_eq!(output.stdout, "");
    }

    #[test]
    fn a_failing_command_carries_its_stderr() {
        let err = run_sh("echo oops >&2; exit 3", None).unwrap_err();
        let Error::Command {
            command,
            status,
            stderr,
        } = err
        else {
            panic!("unexpected error: {err:?}");
        };
        assert_eq!(command, "sh -c 'echo oops >&2; exit 3'");
        assert_eq!(status.code(), Some(3));
        assert_eq!(stderr, "oops");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let err = run("tatou-no-such-program", &[], None).unwrap_err();
        assert!(
            matches!(err, Error::Spawn { ref program, .. } if program == "tatou-no-such-program")
        );
    }

    #[test]
    fn exec_replace_returns_when_the_program_cannot_start() {
        let err = exec_replace("tatou-no-such-program", &[]);
        assert!(matches!(err, Error::Spawn { .. }));
    }

    #[test]
    fn prompts_are_disabled_only_for_git_and_gh() {
        let env = |program: &str| -> Vec<(String, String)> {
            command(program, &[])
                .get_envs()
                .map(|(k, v)| {
                    let v = v.unwrap_or_default();
                    (k.to_string_lossy().into(), v.to_string_lossy().into())
                })
                .collect()
        };
        assert_eq!(env("git"), [("GIT_TERMINAL_PROMPT".into(), "0".into())]);
        assert_eq!(env("gh"), [("GH_PROMPT_DISABLED".into(), "1".into())]);
        assert_eq!(env("tmux"), []);
    }

    #[test]
    fn shell_quote_round_trips_through_sh() {
        let values = [
            "plain",
            "with space",
            "it's",
            "$HOME",
            "`date`",
            "a\nb",
            "back\\slash",
            "-n",
            "*",
            "",
        ];
        for value in values {
            let script = format!("printf %s {}", shell_quote(value));
            assert_eq!(run_sh(&script, None).unwrap().stdout, value, "{value:?}");
        }
    }

    #[test]
    fn command_line_quotes_only_when_needed() {
        let line = command_line("git", &["-C", "/a b", "log", "--format=%h", ""]);
        assert_eq!(line, "git -C '/a b' log --format=%h ''");
    }
}
