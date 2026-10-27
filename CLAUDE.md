# CLAUDE.md

How to work on tatou's code: the stack, the layout, the rules, testing, versioning and the workflow. The other documents each hold their part, and nothing here repeats them:

- `DESIGN.md`: what tatou does and why. Read it before starting any feature. Its philosophy (§1) and what tatou does not do (§3) are the deciding rule for every design question; if a change conflicts with them, stop and ask.
- `MILESTONE.md`: the order of work, a line per finished step, the spec of each open one.
- `README.md`: what users need, from install to every key.

**tatou** is a minimal terminal UI (ratatui) for running coding agents in parallel across several repositories, one git worktree per task, its terminals kept in a daemon of its own. Linux and macOS (Apple silicon) only.

## Tech stack

- Rust (stable), single crate
- `ratatui` + `crossterm` for the UI
- `portable-pty` for pseudo-terminals, `tui-term` (and its `vt100`) to keep and draw their screens
- `serde` + `toml` for config, `serde_json` for `gh`'s output; paths follow XDG on every platform (`~/.config/tatou`, `~/.local/state/tatou`), macOS included (no `~/Library/...`: CLI users expect `~/.config`), honouring `$XDG_*` when set
- `thiserror` for errors, `tracing` + `tracing-subscriber` (`fmt` only) for logs: to `$XDG_STATE_HOME/tatou/tatou.log`, the daemon's too, never to the terminal; rotated to `tatou.log.old` at startup past 1 MiB; level from `$TATOU_LOG`, default `info`; `--help` and `--version` do not log
- `std::thread` + `mpsc` for background work: per connection and per terminal in the daemon, per request, per refresh worker and for the shown terminal's stream in the dashboard; no async runtime
- External tools, called as commands: `git` (required), `gh` (optional); notifications via `notify-send` (Linux) or `osascript` (macOS); the clipboard via `pbcopy` (macOS) or `wl-copy`, `xclip`, `xsel` (Linux, optional; else OSC 52). Their output is read as UTF-8, lossily (file names that are not valid UTF-8 are not supported), with stdin closed, git and gh prompts disabled, and no timeout: they run on background threads, so a stuck one stalls only its own refresh
- Releases: GoReleaser with the Rust builder (`.goreleaser.yaml`); GitHub Actions, one file per workflow in `.github/workflows/` (`test.yaml`, `release.yaml`). Binaries: `linux-amd64`, `linux-arm64` (static) and `darwin-arm64`; no Intel Macs. Platforms are named OS-arch to people, never as Rust triples outside `.goreleaser.yaml`

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
  reflow.rs   # a terminal's screen at a new size, its lines wrapped again
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

A task is done only when `cargo fmt`, `cargo clippy` (no warnings) and `cargo test` pass, on Linux and macOS in CI with the GoReleaser snapshot build; `VERSION` is bumped; the rules below hold; and the documents are updated in the same change: `DESIGN.md` when a design decision moved, `README.md` when users see a difference, `MILESTONE.md`'s line for the step.

## Rules

- **DESIGN §1 and §3 first:** one terminal at a time and no multiplexer, no state beyond the files of DESIGN §9, no reimplemented tools (delegate to `$EDITOR`, a shell or the agent), nothing in a repository but plain git worktrees, never a file tatou doesn't own modified (print the snippet instead).
- **The daemon owns the terminals.** Every terminal is started, written to, resized and ended through the daemon's protocol (`protocol.rs`, via `client.rs`); the dashboard never runs a program in a pseudo-terminal itself (DESIGN §8.3).
- **Linux and macOS only, no platform sprawl.** All `cfg(target_os)` code lives in `platform.rs`; `portable-pty` hides the pseudo-terminals' differences. Shell snippets and command templates are POSIX `sh` and must not assume GNU userland (macOS ships BSD tools).
- **All external commands go through `exec.rs`**, using argument vectors, never shell strings built from paths or user input. Exceptions, each run deliberately with `sh -c`: the user's configured `on_create` command, custom-action templates from config, and the programs the daemon runs in terminals (the editor and custom actions, through `portable-pty`). When substituting `{path}` and the like into templates, shell-quote the values.
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

- Follow `MILESTONE.md` in order: each step is a GitHub issue in the **tatou** project. One step = one issue = one pull request. A finished step becomes one line there; its spec and decisions stay in the issue and the pull request.
- Plan first for anything touching more than two modules, and wait for approval.
- Small steps, one logical change per commit, imperative commit messages.
- Every commit is signed off for the DCO: `git commit -s` (also when amending).
- When a design decision changes, update `DESIGN.md` in the same commit. Each fact has one home among the documents (above); elsewhere, link to it.
- If something is ambiguous, ask instead of guessing.
