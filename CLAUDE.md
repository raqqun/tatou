# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project

**tatou**: a minimal terminal UI (ratatui) for running coding agents in parallel across several repositories, one git worktree per task. It lists projects and worktrees with agent status, shows the selected agent's, shell's or editor's terminal in a pane beside them, and keeps every terminal running in a small daemon of its own, so that agents outlive the dashboard. It delegates everything else to `git`, `gh` and `$EDITOR`, and it has no state of its own beyond its config files (`config.toml` and one small file per project) and what its daemon holds in memory.

Targets Linux and macOS (Apple silicon). Windows and the BSDs are out of scope.

Read `DESIGN.md` before starting any feature. Its philosophy section is the deciding rule for every design question: **reuse existing tools, keep no state, never get in the user's way, one terminal at a time and no multiplexer, stay small.** If a change conflicts with it, stop and ask.

## Tech stack

- Rust (stable), single crate
- `ratatui` + `crossterm` for the UI
- `portable-pty` for pseudo-terminals, `tui-term` (and its `vt100`) to keep and draw their screens
- `serde` + `toml` for config; paths follow XDG on every platform (`~/.config/tatou`, `~/.local/state/tatou`), macOS included, honouring `$XDG_*` when set
- `thiserror` for errors, `tracing` + `tracing-subscriber` (`fmt` only) for logs (to `$XDG_STATE_HOME/tatou/tatou.log`, rotated at startup past 1 MiB, never to the terminal)
- `std::thread` + `mpsc` for background work; no async runtime
- External tools, called as commands: `git` (required), `gh` (optional); notifications via `notify-send` (Linux) or `osascript` (macOS); the clipboard via `pbcopy` (macOS) or `wl-copy`, `xclip`, `xsel` (Linux, optional; else OSC 52)
- Releases: GoReleaser with the Rust builder (`.goreleaser.yaml`); GitHub Actions, one file per workflow in `.github/workflows/` (`test.yaml`, `release.yaml`)

Keep the dependency list short. Before adding a crate, check whether the standard library or one of the external tools already does the job.

## Layout

```
src/
  main.rs     # arg parsing: dashboard, `add`, `daemon`, `stop`
  launcher.rs # `tatou`: check git, offer to add the repository, start the daemon, run the dashboard
  error.rs    # crate-wide Error enum and Result alias
  logging.rs  # log file setup and rotation
  config.rs   # load config.toml and projects/*.toml, write project files; XDG directories
  theme.rs    # themes: the dashboard's colours, built in (themes/) or from the config's themes/
  exec.rs     # the only place that runs external commands, but the terminals' programs
  platform.rs # the only platform-specific code: copy(text), notify(title, body)
  git.rs      # worktrees, branches, status; output parsing
  gh.rs       # PR list; output parsing
  daemon.rs   # `tatou daemon`: terminals (pseudo-terminal + vt100 screen), sessions, the socket
  protocol.rs # the daemon's frames: requests and replies
  client.rs   # the dashboard's side of the socket; starts the daemon
  term.rs     # keys and pastes into the bytes a terminal program reads
  model.rs    # merges config + git + the daemon's sessions + gh into what the UI shows
  refresh.rs  # background worker threads feeding the UI loop over a channel
  actions.rs  # the requests and commands behind each action, on threads of their own, reporting events
  keys.rs     # keymap: defaults, config overrides, action dispatch
  ui/         # ratatui rendering and input handling only, the shown terminal included
themes/          # the built-in themes, one TOML file each, compiled in
tests/fixtures/  # captured git/gh outputs used by parser tests
```

## Commands

```bash
cargo run                # dashboard (starts the daemon)
cargo run -- stop        # ends every terminal and the daemon
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

A task is done only when `cargo fmt`, `cargo clippy` (no warnings) and `cargo test` all pass.

## Rules

- **One terminal at a time, no multiplexer.** The dashboard shows one terminal, beside the projects. No splits, layouts, tabs, keyboard copy mode or scripting; if a feature needs them, it does not belong in tatou. Selecting with the mouse in the shown terminal, as any terminal does, is the one exception (6.9).
- **No persistent state** besides `config.toml`, the project files in `projects/` next to it (one per repository, written by `tatou add` and the launch offer, plain TOML the user may edit) and the user's own themes in `themes/`. The daemon keeps its terminals in memory only, and its socket is a runtime file. No database, cache or other state file; derive everything from `git`, `gh` and the daemon.
- **The daemon owns the terminals.** Every terminal is started, written to, resized and ended through the daemon's protocol (`protocol.rs`, via `client.rs`); the dashboard never runs a program in a pseudo-terminal itself. One daemon per user, on `$XDG_STATE_HOME/tatou/tatou.sock`.
- **Don't reimplement tools.** No built-in editor, pager, diff viewer or file operations; delegate to `$EDITOR`, a shell, or the agent.
- **Everything tatou creates in a repository is a plain git worktree**, usable without tatou.
- **Linux and macOS only, no platform sprawl.** All `cfg(target_os)` code lives in `platform.rs`; `portable-pty` hides the pseudo-terminals' differences. Shell snippets and command templates are POSIX `sh` and must not assume GNU userland (macOS ships BSD tools).
- **Never modify files tatou doesn't own** (agent configs, shell rc files, git config). Print the snippet for the user instead.
- **All external commands go through `exec.rs`**, using argument vectors, never shell strings built from paths or user input. Exceptions, each run deliberately with `sh -c`: the user's configured `on_create` command, custom-action templates from config, and the programs the daemon runs in terminals (agents' commands from config, through `portable-pty`). When substituting `{path}` and the like into templates, shell-quote the values.
- **The UI loop never blocks on a command or the socket.** Run commands and requests on background threads, and write to the shown terminal's connection through its writer thread; send results back over a channel; keep rendering responsive.
- **Restore the terminal** (raw mode, alternate screen, bracketed paste, keyboard enhancement) on every exit path, including panics.
- **Never force destructive git operations silently.** If `git worktree remove` fails, show git's message and require explicit confirmation to force.
- **Match sessions to worktrees by the path they were started in**, never by parsing session names.
- No `unwrap()`/`expect()` outside tests unless the invariant is impossible to break; comment why.
- Code written before anything calls it gets `#[allow(dead_code)]` on the item, with a comment naming the issue that will use it; that issue removes the attribute. Never allow dead code crate-wide.
- Prefer plain, obvious code over abstractions. No traits or generics until there are two real uses.

## Testing

- Parsers for git and gh output are pure functions tested against fixtures in `tests/fixtures/`; one-line outputs are inline strings in the test.
- Git tests use throwaway repos in a `tempfile::TempDir`, and run git with the user's config disabled (`GIT_CONFIG_GLOBAL=/dev/null`), so their hooks or commit signing never interfere.
- The protocol is tested by round trips. The daemon is tested end to end in-process on a temporary socket that never ends the test process (`client::testing::daemon`); terminals end a moment after their program, so wait for it (`sessions_become`) rather than assert at once.
- Keymap parsing, key encoding and model merging are unit-tested.
- UI rendering can be checked with ratatui's `TestBackend` for key screens; don't over-test layout.
- No test touches the network, the user's real repositories or their daemon. Tests that run the tatou binary use the sandbox in `tests/common/` (`$HOME` and `$XDG_*` in a temporary directory).
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
