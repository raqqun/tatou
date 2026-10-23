//! `tatou` with no argument: prepares tatou's tmux server and attaches the
//! terminal to the dashboard session, starting both when needed.

use std::env;
use std::fs;
use std::io::{self, ErrorKind};
use std::process::ExitCode;

use crate::config;
use crate::error::{Error, Result};
use crate::exec;
use crate::tmux::{self, Nesting, Server};

pub fn run() -> ExitCode {
    match launch() {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(%err, "launch failed");
            eprintln!("tatou: {err}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> Result<ExitCode> {
    check_tools()?;
    let config = config::load()?;

    let conf = tmux::conf_path()?;
    // Read before it is replaced: the server still has the bindings it made
    let previous = fs::read_to_string(&conf).ok();
    tmux::write_conf(&config)?;
    let server = Server::tatou(&conf);
    server.reload(previous.as_deref())?;

    // By absolute path, so the session works whatever its $PATH
    let exe = match env::current_exe() {
        Ok(exe) => exe.to_string_lossy().into_owned(),
        Err(_) => "tatou".to_string(),
    };
    let dashboard = format!("{} --dashboard", exec::shell_quote(&exe));

    match tmux::nesting() {
        Nesting::Outside => {}
        Nesting::Tatou => {
            switch_to_dashboard(&server, &dashboard)?;
            return Ok(ExitCode::SUCCESS);
        }
        Nesting::Other => {
            if !confirm_nesting() {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    tracing::info!("attaching to the dashboard");
    let args = server.args(&[
        "new-session",
        "-A",
        "-s",
        tmux::DASHBOARD,
        "-n",
        tmux::DASHBOARD,
        &dashboard,
    ]);
    // TMUX is unset so that tmux agrees to attach inside another tmux
    Err(exec::exec_replace("tmux", &args, &["TMUX"]))
}

/// tmux (recent enough) and git must be installed; gh is optional.
fn check_tools() -> Result<()> {
    let missing = |program, needs| {
        move |err| match err {
            Error::Spawn { source, .. } if source.kind() == ErrorKind::NotFound => {
                Error::Missing { program, needs }
            }
            err => err,
        }
    };
    let tmux = tmux::check_version().map_err(missing("tmux", "tmux 3.2 or later"))?;
    let git = exec::run("git", &["--version"], None).map_err(missing("git", "git"))?;
    tracing::info!(tmux, git = git.stdout.trim(), "tools");
    Ok(())
}

/// Already on tatou's server: moves this client to the dashboard, starting it again
/// if its session was closed.
fn switch_to_dashboard(server: &Server, dashboard: &str) -> Result<()> {
    let target = format!("={}", tmux::DASHBOARD);
    if server.run(&["has-session", "-t", &target]).is_err() {
        let name = tmux::DASHBOARD;
        server.run(&["new-session", "-d", "-s", name, "-n", name, dashboard])?;
    }
    server.run(&["switch-client", "-t", &target])?;
    Ok(())
}

/// Inside another tmux server, attaching would nest two tmux clients in one
/// terminal. Asks before doing it; anything but yes quits.
fn confirm_nesting() -> bool {
    eprint!(
        "tatou: this terminal is already inside tmux. Attaching runs tatou's tmux \
         inside it, with two prefix keys and two status bars; tatou works best in a \
         terminal tab of its own.\nAttach anyway? [y/N] "
    );
    let mut answer = String::new();
    // No answer (end of input, no terminal) means no
    let _ = io::stdin().read_line(&mut answer);
    is_yes(&answer)
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_attaches() {
        for answer in ["y\n", "Y\n", " yes \n", "YES"] {
            assert!(is_yes(answer), "{answer:?}");
        }
        for answer in ["", "\n", "n\n", "no\n", "yep\n", "q\n"] {
            assert!(!is_yes(answer), "{answer:?}");
        }
    }
}
