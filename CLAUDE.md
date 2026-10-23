# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project

**tatou**: a minimal terminal UI (ratatui) for running coding agents in parallel across several repositories, one git worktree per task. It lists projects and worktrees with agent status, shows an actionable file tree for the selected worktree, and delegates everything else to `tmux`, `git`, `gh`, `$EDITOR` and `$PAGER`. It never renders agent output, and it has no state of its own beyond a small config file.

Targets Linux and macOS (Apple silicon). Windows and the BSDs are out of scope.

Read `DESIGN.md` before starting any feature. Its philosophy section is the deciding rule for every design question: **reuse existing tools, keep no state, never get in the user's way, no terminal-in-terminal, stay small.** If a change conflicts with it, stop and ask.

## Tech stack

- Rust (stable), single crate
- `ratatui` + `crossterm` for the UI
- `ignore` for gitignore-aware directory walking
- `serde` + `toml` for config; paths follow XDG on every platform (`~/.config/tatou`, `~/.local/state/tatou`), macOS included, honouring `$XDG_*` when set
- `thiserror` for errors, `tracing` + `tracing-subscriber` (`fmt` only) for logs (to `$XDG_STATE_HOME/tatou/tatou.log`, rotated at startup past 1 MiB, never to the terminal)
- `std::thread` + `mpsc` for background work; no async runtime
- External tools, called as commands: `git`, `tmux` ≥ 3.2 (required), `gh` (optional); notifications via `notify-send` (Linux) or `osascript` (macOS)
- Releases: GoReleaser with the Rust builder (`.goreleaser.yaml`); GitHub Actions, one file per workflow in `.github/workflows/` (`test.yaml`, `release.yaml`)

Keep the dependency list short. Before adding a crate, check whether the standard library or one of the external tools already does the job.

## Layout

```
src/
  main.rs     # arg parsing: dashboard, --popup, `status`, `hooks`
  error.rs    # crate-wide Error enum and Result alias
  logging.rs  # log file setup and rotation
  config.rs   # load/save config.toml; XDG config and state directories
  exec.rs     # the only place that runs external commands
  platform.rs # the only platform-specific code: open(path), notify(title, body)
  git.rs      # worktrees, branches, status; output parsing
  tmux.rs     # generated tmux.conf; sessions, options, switch/popup/window/send-keys; output parsing
  gh.rs       # PR list; output parsing
  files.rs    # file tree for a worktree (ignore walk + git markers)
  model.rs    # merges config + git + tmux + gh into what the UI shows
  keys.rs     # keymap: defaults, config overrides, action dispatch
  ui/         # ratatui rendering and input handling only
tests/fixtures/  # captured git/tmux/gh outputs used by parser tests
```

## Commands

```bash
cargo run                # dashboard (run it inside tmux)
cargo run -- --popup
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

A task is done only when `cargo fmt`, `cargo clippy` (no warnings) and `cargo test` all pass.

## Rules

- **No terminal-in-terminal.** Never capture or render agent/pane output. To show something, switch the tmux client, or open a tmux window or popup.
- **No persistent state** besides `config.toml` and the generated `tmux.conf` (rebuilt from `config.toml` at every launch). No database, cache or state file; derive everything from `git`, `tmux` and `gh`.
- **tatou's own tmux server only.** Every tmux invocation goes through `tmux.rs` and uses `-L tatou -f <generated conf>`. Never talk to the user's default tmux server, never read or write the user's `~/.tmux.conf` or `~/.config/tmux/tmux.conf` (except sourcing them when `inherit_user_conf = true`), never create `tmux.local.conf`.
- **Don't reimplement tools.** No built-in editor, pager, diff viewer, file operations, splits or session persistence; delegate to `$EDITOR`, `$PAGER`, `git diff`, tmux.
- **Everything tatou creates must be usable without tatou:** plain worktrees, plain tmux sessions.
- **Linux and macOS only, no platform sprawl.** All `cfg(target_os)` code lives in `platform.rs`. Shell snippets and command templates are POSIX `sh` and must not assume GNU userland (macOS ships BSD tools).
- **Never modify files tatou doesn't own** (agent configs, `.tmux.conf`, shell rc files, git config). Print the snippet for the user instead.
- **All external commands go through `exec.rs`**, using argument vectors, never shell strings built from paths or user input. Exceptions, each run deliberately with `sh -c`: the user's configured `on_create` command and custom tmux-command templates from config. When substituting `{path}`/`{file}` into templates, shell-quote the values.
- **The UI loop never blocks on a command.** Run commands on background threads; send results back over a channel; keep rendering responsive.
- **Restore the terminal** (raw mode, alternate screen, mouse capture) on every exit path, including panics.
- **Never force destructive git operations silently.** If `git worktree remove` fails, show git's message and require explicit confirmation to force.
- **Match tmux sessions to worktrees by path** (`@tatou_path`, then `#{session_path}`), never by parsing session names.
- No `unwrap()`/`expect()` outside tests unless the invariant is impossible to break; comment why.
- Code written before anything calls it gets `#[allow(dead_code)]` on the item, with a comment naming the issue that will use it; that issue removes the attribute. Never allow dead code crate-wide.
- Prefer plain, obvious code over abstractions. No traits or generics until there are two real uses.

## Testing

- Parsers for git, tmux and gh output are pure functions tested against fixtures in `tests/fixtures/`; one-line outputs (like `tmux -V`) are inline strings in the test.
- Git tests use throwaway repos in a `tempfile::TempDir`.
- tmux tests use a unique `-L` socket per test and kill that server afterwards; skip if `tmux` is missing.
- Keymap parsing and model merging are unit-tested.
- UI rendering can be checked with ratatui's `TestBackend` for key screens; don't over-test layout.
- No test touches the network, the user's real repositories or their tmux server.
- CI runs fmt, clippy and tests on both Linux and macOS, plus a GoReleaser snapshot build, on every pull request.

## Versioning

- Every build is an alpha until a stable v1. `VERSION` holds the version as its git tag: `v0.1.0-alpha.N`.
- Every task's pull request bumps `N` by one in `VERSION` and in `Cargo.toml` (`0.1.0-alpha.N`, no `v`); a unit test checks they match.
- Merging into `main` makes `release.yaml` tag the version and publish a GitHub pre-release. Never create or push tags by hand.

## Workflow

- Follow the milestones in `DESIGN.md` in order; `MILESTONE.md` breaks them into steps, each a GitHub issue in the **tatou** project. One step = one issue = one pull request.
- Plan first for anything touching more than two modules, and wait for approval.
- Small steps, one logical change per commit, imperative commit messages.
- Every commit is signed off for the DCO: `git commit -s` (also when amending).
- When a design decision changes, update `DESIGN.md` in the same commit.
- If something is ambiguous, ask instead of guessing.
