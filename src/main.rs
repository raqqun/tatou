mod config;
mod error;
mod exec;
mod files;
mod gh;
mod git;
mod keys;
mod logging;
mod model;
mod platform;
mod tmux;
mod ui;

/// tatou's version, read from the `VERSION` file, which holds it as its git tag (`v0.1.0-alpha.1`).
fn version() -> &'static str {
    include_str!("../VERSION").trim()
}

fn main() {
    // tatou works without its log file, so a logging failure is reported and ignored
    if let Err(err) = logging::init() {
        eprintln!("tatou: logging disabled: {err}");
    }
    tracing::info!(
        version = version(),
        pid = std::process::id(),
        config_dir = ?config::config_dir().ok(),
        "started"
    );

    // Placeholder until the command line is parsed (step 1.10).
    println!("tatou {}", version());
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cargo.toml cannot read VERSION, so both are bumped together; this catches a missed one.
    #[test]
    fn version_matches_cargo_toml() {
        assert_eq!(version().strip_prefix('v'), Some(env!("CARGO_PKG_VERSION")));
    }
}
