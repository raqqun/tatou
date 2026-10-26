//! The only place that runs external commands.
//!
//! Commands take argument vectors, never shell strings. `run_sh` is the deliberate
//! exception, for the user's `on_create` command and custom tmux-command templates.
//! stdin is closed and git/gh prompts are disabled, so a command run in the
//! background never waits for input; `feed` gives a command its input instead.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// What a successful command printed, converted to UTF-8 lossily.
#[derive(Debug)]
pub struct Output {
    pub stdout: String,
}

/// Runs `program` with `args` in `cwd` (or the current directory) and waits for it.
/// A non-zero exit becomes `Error::Command` carrying the command's stderr.
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
    // A successful command's warnings are only worth a debug line
    if !stderr.is_empty() {
        tracing::debug!(command = %line, stderr = stderr.trim(), "stderr");
    }
    Ok(Output {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Runs `program` with `args`, `input` as its stdin, and waits for it. Its output
/// is not read: a clipboard tool such as `xclip` stays behind to serve what it was
/// given, and would hold a pipe open. A non-zero exit is `Error::Command`, without
/// stderr.
pub fn feed(program: &str, args: &[&str], input: &[u8]) -> Result<()> {
    let line = command_line(program, args);
    tracing::debug!(command = %line, bytes = input.len(), "feed");
    let mut command = command(program, args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let spawn_error = |source| Error::Spawn {
        program: program.to_string(),
        source,
    };
    let mut child = command.spawn().map_err(spawn_error)?;
    // Taken, so that it closes once written and the program sees the input end
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(spawn_error)?;
    }
    let status = child.wait().map_err(spawn_error)?;
    if !status.success() {
        return Err(Error::Command {
            command: line,
            status,
            stderr: String::new(),
        });
    }
    Ok(())
}

/// Runs `script` with `sh -c`. Only for commands the user wrote in the config, with
/// any substituted values passed through `shell_quote`.
pub fn run_sh(script: &str, cwd: Option<&Path>) -> Result<Output> {
    run("sh", &["-c", script], cwd)
}

/// Quotes `value` for POSIX `sh` so that it is always read as one literal word.
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
            // Tests never see the user's or the system's git config: their hooks or
            // commit signing must not get in the way
            if cfg!(test) {
                command.env("GIT_CONFIG_GLOBAL", "/dev/null");
                command.env("GIT_CONFIG_NOSYSTEM", "1");
            }
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
    fn feed_gives_the_program_its_input() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out");
        let script = format!("cat > {}", shell_quote(&file.to_string_lossy()));
        feed("sh", &["-c", &script], "héllo\n".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "héllo\n");
        let failed = feed("sh", &["-c", "exit 3"], b"");
        assert!(matches!(failed, Err(Error::Command { .. })), "{failed:?}");
        let missing = feed("tatou-no-such-program", &[], b"");
        assert!(matches!(missing, Err(Error::Spawn { .. })), "{missing:?}");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let err = run("tatou-no-such-program", &[], None).unwrap_err();
        assert!(
            matches!(err, Error::Spawn { ref program, .. } if program == "tatou-no-such-program")
        );
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
        assert_eq!(
            env("git"),
            [
                // These two in unit tests only
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ]
        );
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
