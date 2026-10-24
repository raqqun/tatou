mod config;
mod error;
mod exec;
mod files;
mod gh;
mod git;
mod keys;
mod launcher;
mod logging;
mod model;
mod platform;
mod refresh;
mod tmux;
mod ui;

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use error::Error;

use tracing::level_filters::LevelFilter;

const USAGE: &str = "\
Usage: tatou [command]

Runs coding agents in parallel, each in its own git worktree and tmux session.

Commands:
  (none)         attach to tatou's tmux server and its dashboard
  add [path]     add the repository at path (default: here) to config.toml
  status         print a short summary of agent statuses, for tmux's status line
  hooks          print the hooks that let agents report their status
  --dashboard    run the dashboard; `tatou` starts it inside tatou's tmux server
  --popup        run the dashboard in a tmux popup; bound to the popup key
  -h, --help     print this help
  -V, --version  print the version
";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Command {
    Launch,
    /// The folder given, if any.
    Add(Option<PathBuf>),
    Dashboard,
    Popup,
    Status,
    Hooks,
    Help,
    Version,
}

/// tatou's version, read from the `VERSION` file, which holds it as its git tag (`v0.1.0-alpha.1`).
fn version() -> &'static str {
    include_str!("../VERSION").trim()
}

/// Reads the arguments after the program name: a command, and for `add` a path.
fn parse_args(args: &[OsString]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Launch),
        [add] if add == "add" => Ok(Command::Add(None)),
        [add, path] if add == "add" => Ok(Command::Add(Some(PathBuf::from(path)))),
        [add, _, extra, ..] if add == "add" => {
            Err(format!("unexpected argument `{}`", extra.to_string_lossy()))
        }
        [arg] => match arg.to_str() {
            Some("--dashboard") => Ok(Command::Dashboard),
            Some("--popup") => Ok(Command::Popup),
            Some("status") => Ok(Command::Status),
            Some("hooks") => Ok(Command::Hooks),
            Some("-h" | "--help") => Ok(Command::Help),
            Some("-V" | "--version") => Ok(Command::Version),
            _ => Err(format!("unknown argument `{}`", arg.to_string_lossy())),
        },
        [_, extra, ..] => Err(format!("unexpected argument `{}`", extra.to_string_lossy())),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let command = match parse_args(&args) {
        Ok(command) => command,
        Err(message) => {
            eprint!("tatou: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let level = match command {
        Command::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("tatou {}", version());
            return ExitCode::SUCCESS;
        }
        // Runs every few seconds from tmux's status line: only problems are worth a log line
        Command::Status => LevelFilter::WARN,
        _ => LevelFilter::INFO,
    };

    // tatou works without its log file, so a logging failure is reported and ignored
    if let Err(err) = logging::init(level) {
        eprintln!("tatou: logging disabled: {err}");
    }
    tracing::info!(
        version = version(),
        pid = std::process::id(),
        ?command,
        config_dir = ?config::config_dir().ok(),
        "started"
    );

    match command {
        Command::Launch => launcher::run(),
        Command::Add(path) => add(path.as_deref().unwrap_or(Path::new("."))),
        Command::Dashboard => dashboard(),
        Command::Popup => not_implemented("--popup", 25),
        Command::Status => not_implemented("status", 30),
        Command::Hooks => not_implemented("hooks", 26),
        // Handled before logging starts
        Command::Help | Command::Version => ExitCode::SUCCESS,
    }
}

/// The dashboard lives in the `tatou` session on tatou's tmux server, where the
/// launcher starts it.
fn dashboard() -> ExitCode {
    if tmux::nesting() != tmux::Nesting::Tatou {
        eprintln!("tatou: the dashboard runs inside tatou's tmux server; run `tatou` to start it");
        return ExitCode::FAILURE;
    }
    match ui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "dashboard failed");
            eprintln!("tatou: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `tatou add`: adds the repository `dir` is in, by its main worktree.
fn add(dir: &Path) -> ExitCode {
    let home = env::var_os("HOME").map(PathBuf::from);
    let added = git::main_worktree(dir).and_then(|repo| {
        let added = config::add_project(&repo)?;
        Ok((repo, added, config::path()?))
    });
    match added {
        Ok((repo, added, config)) => {
            let (repo, config) = (
                config::tilde(&repo, home.as_deref()),
                config::tilde(&config, home.as_deref()),
            );
            match added {
                config::Added::Added => println!("Added {repo} to {config}"),
                config::Added::AlreadyThere => println!("{repo} is already a project in {config}"),
            }
            tracing::info!(repo, ?added, "add");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::warn!(%err, "add failed");
            match err {
                // git's own message says why, e.g. not a repository
                Error::Command { stderr, .. } => {
                    eprintln!("tatou: cannot add {}: {stderr}", dir.display())
                }
                err => eprintln!("tatou: {err}"),
            }
            ExitCode::FAILURE
        }
    }
}

/// Placeholder until the command's own step fills it in.
fn not_implemented(what: &str, issue: u32) -> ExitCode {
    eprintln!("tatou: {what} is not implemented yet (#{issue})");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse_args(&args)
    }

    // Cargo.toml cannot read VERSION, so both are bumped together; this catches a missed one.
    #[test]
    fn version_matches_cargo_toml() {
        assert_eq!(version().strip_prefix('v'), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn parses_every_command() {
        let cases = [
            (&[][..], Command::Launch),
            (&["--dashboard"], Command::Dashboard),
            (&["--popup"], Command::Popup),
            (&["status"], Command::Status),
            (&["hooks"], Command::Hooks),
            (&["add"], Command::Add(None)),
            (&["add", "../app"], Command::Add(Some("../app".into()))),
            (&["-h"], Command::Help),
            (&["--help"], Command::Help),
            (&["-V"], Command::Version),
            (&["--version"], Command::Version),
        ];
        for (args, expected) in cases {
            assert_eq!(parse(args), Ok(expected), "{args:?}");
        }
    }

    #[test]
    fn rejects_unknown_and_extra_arguments() {
        assert_eq!(
            parse(&["--stauts"]),
            Err("unknown argument `--stauts`".into())
        );
        assert_eq!(parse(&["help"]), Err("unknown argument `help`".into()));
        assert_eq!(
            parse(&["status", "-v"]),
            Err("unexpected argument `-v`".into())
        );
        assert_eq!(
            parse(&["add", "a", "b"]),
            Err("unexpected argument `b`".into())
        );
    }

    #[test]
    fn a_non_utf8_argument_is_unknown() {
        use std::os::unix::ffi::OsStringExt;
        let arg = OsString::from_vec(vec![b'-', 0xff]);
        assert_eq!(
            parse_args(&[arg]),
            Err("unknown argument `-\u{fffd}`".into())
        );
    }
}
