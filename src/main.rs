mod config;
mod exec;
mod files;
mod gh;
mod git;
mod keys;
mod model;
mod platform;
mod tmux;
mod ui;

/// tatou's version, read from the `VERSION` file, which holds it as its git tag (`v0.1.0-alpha.1`).
fn version() -> &'static str {
    include_str!("../VERSION").trim()
}

fn main() {
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
