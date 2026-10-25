# tatou — Milestones

The step-by-step plan from an empty repository to the first complete version of tatou. It breaks down the milestones of `DESIGN.md` §11; `DESIGN.md` remains the reference for *what* and *why*, this file says *in which order*.

Each step is one GitHub issue (tracked in the **tatou** GitHub project, grouped by GitHub milestone) and one pull request. Steps are listed in dependency order; "Depends on" names the steps that must be merged first.

**Versioning:** every build is an alpha until a stable v1. The `VERSION` file holds the current version as its git tag, `v0.1.0-alpha.N`; every task's pull request bumps `N` by one (in `VERSION` and in `Cargo.toml`, without the `v`). Merging the pull request into `main` tags that version and publishes it as a GitHub pre-release with GoReleaser (set up in 1.2). Tags are created by CI only.

**Definition of done for every step:** `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on Linux and macOS (CI), the GoReleaser snapshot build passes, `VERSION` is bumped, the rules in `CLAUDE.md` hold, and `DESIGN.md` is updated in the same change if a design decision moved.

**Checkpoints:** after M1 tatou is dogfoodable (navigate and jump). After M2 it is already a useful tool. After M7 it is feature-complete and packaged, still an alpha; v1 is a separate decision.

---

## M1 — Navigate

Config, generated tmux config, launcher, list projects/worktrees from git, sessions/status from tmux, jump with Enter and back with the back key. Dashboard mode only.

### 1.1 Scaffold the crate (#1)

Depends on: —

- `cargo init` a single binary crate named `tatou`, edition 2024, MSRV = current stable noted in `Cargo.toml` (`rust-version`).
- Dependencies: `ratatui`, `crossterm`, `serde` (derive), `toml`, `thiserror`, `tracing`. Dev: `tempfile`.
- Empty modules matching `CLAUDE.md` layout: `config`, `exec`, `platform`, `git`, `tmux`, `gh`, `files`, `model`, `keys`, `ui/`; `tests/fixtures/` with a `.gitkeep`.
- `rustfmt.toml` only if a non-default setting is actually wanted.
- `Cargo.lock` committed (binary crate).
- `VERSION` file containing `v0.1.0-alpha.1`; `Cargo.toml` `version = "0.1.0-alpha.1"`. The binary reads its version from `VERSION` (`include_str!`), and a unit test fails when `Cargo.toml`'s version differs from `VERSION` without its `v`.

**Done when**
- `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass on an empty crate.

### 1.2 CI and alpha releases with GoReleaser (#2)

Depends on: 1.1

One file per workflow under `.github/workflows/`, each starting with a comment saying what it does and when it runs (same layout as raqqun/kcvm).

- `test.yaml`, on every pull request:
  - Job `test`, matrix `ubuntu-latest` + `macos-latest`, stable toolchain: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Install `tmux` (apt / brew) so tmux tests run instead of being skipped. Cache cargo.
  - Job `build`: builds exactly like a release without publishing anything, `goreleaser release --snapshot --clean`.
- `release.yaml`, when a pull request is merged into `main`: check out the merge commit, read `VERSION`; if that tag does not exist yet, create and push it, then `goreleaser release --clean`; otherwise log that the version is already released.
- `.goreleaser.yaml` (version 2) with the Rust builder (`builder: rust`, `cargo zigbuild`) for three platforms: `linux-amd64`, `linux-arm64` (both statically linked) and `darwin-arm64` (Apple silicon only; Intel Macs are not supported). Everything users see (archives, docs, release notes) names platforms by OS and architecture only; the full Rust target names, which cargo requires, appear only in `.goreleaser.yaml`, with a comment mapping each to its OS-architecture name.
- tar.gz archives named `{{ .ProjectName }}-{{ .Tag }}-{{ .Os }}-{{ .Arch }}` (e.g. `tatou-v0.1.0-alpha.2-linux-amd64.tar.gz`); `sha256sums.txt`; `release.prerelease: auto` so `-alpha.N` versions are pre-releases.
- The workflows install what the Rust builder needs: the Rust targets, `zig` and `cargo-zigbuild`. If cross-building the macOS targets from Linux does not work, build them on a macOS runner and document why.

**Done when**
- A PR shows green `test` (both OSes) and `build` checks; a deliberate clippy warning fails the build.
- Merging this step's PR (which bumps `VERSION` to `v0.1.0-alpha.2`) creates the tag and a GitHub pre-release with three archives and checksums; the Linux binaries are static (`file` says so).

### 1.3 Errors, XDG paths and file logging (#3)

Depends on: 1.1

- Crate error type with `thiserror` (one enum, variants added as modules need them).
- XDG helpers: `config_dir()` = `$XDG_CONFIG_HOME/tatou` or `~/.config/tatou`; `state_dir()` = `$XDG_STATE_HOME/tatou` or `~/.local/state/tatou`. Same on macOS (no `~/Library`). Empty `$XDG_*` is treated as unset (XDG spec).
- Logging with `tracing` to `<state_dir>/tatou.log`, never to the terminal; level from `TATOU_LOG` (default `info`).
- **Decided:** `tracing-subscriber` with only its `fmt` and `std` features (ANSI off), listed in `DESIGN.md` §10 and `CLAUDE.md`. The error enum lives in `src/error.rs`, log setup in `src/logging.rs`. Every tatou process appends to the same file; at startup it is moved to `tatou.log.old` once past 1 MiB.

**Done when**
- Unit tests for path resolution with and without `$XDG_*` (set via a test-local env, not the user's).
- Running the binary creates the log file and writes a startup line.

### 1.4 `exec.rs`: the only way to run commands (#4)

Depends on: 1.3

- `run(program, args, cwd) -> Result<Output>`: argument vector, captured stdout/stderr, exit status; non-zero exit becomes an error carrying stderr (shown to the user later, e.g. git's refusal message).
- `run_sh(script, cwd)`: the deliberate `sh -c` path, used only for `on_create` and custom templates.
- `exec_replace(program, args)`: replace the current process (`CommandExt::exec`), used by the launcher to hand the terminal to tmux.
- `shell_quote(&str) -> String`: POSIX single-quote quoting for values substituted into templates.
- Set `GIT_TERMINAL_PROMPT=0` for `git` and `GH_PROMPT_DISABLED=1` for `gh` so background commands never wait for input. Only for those two: a tmux server inherits the environment of the command that starts it and would pass the variables on to every agent session.
- **Decided:** stdout/stderr as lossy UTF-8 strings; no timeout (revisited in 5.2); code not called yet gets a per-item `#[allow(dead_code)]` naming the issue that will use it.
- Log every command (program + args) at `debug`.

**Done when**
- Tests: success, non-zero exit with stderr captured, missing program, `shell_quote` with spaces, quotes, `$`, newlines (round-trip through `sh -c 'printf %s ...'`).

### 1.5 `config.rs`: load `config.toml` (#5)

Depends on: 1.3

- Types for the whole schema in `DESIGN.md` §9 (`default_agent`, `show_ignored`, `[tmux]`, `[agents]`, `[keys]`, `[keys.custom]`, `[keys.tmux]`, `[[project]]` with `path`, `base`, `worktree_dir`, `on_create`), with defaults; unknown keys are an error with a clear message (`deny_unknown_fields`).
- `~` expansion in paths; project paths canonicalised; a missing project path is reported per project, not fatal.
- Load from `<config_dir>/config.toml`. Parse errors show file, line and message, then exit cleanly.
- **Decided:** when the file is missing, write a commented template (all settings commented out, no projects) and start with an empty project list, showing the config path in the UI's empty state. `base` is optional (default: the main worktree's branch). `[agents]` is merged with the built-in agents. A relative project path is an error on that project only. `[keys]` action names are checked in 6.1.

**Done when**
- Unit tests: full example from `DESIGN.md` §9 parses; empty file gives defaults; unknown key errors; `~` expansion.

### 1.6 Generate tatou's `tmux.conf` (#6)

Depends on: 1.5

- Pure function `config -> String`, then an atomic write (temp file + rename) to `<config_dir>/tmux.conf` at every launch.
- Contents (DESIGN §8.3): `escape-time 10`, `extended-keys on`, `focus-events on`, `set-clipboard on`, `allow-passthrough on`, `mouse on`, large `history-limit`, true-colour `terminal-features`; optional `prefix`.
- Back key: `bind-key -n <back> switch-client -t '=tatou'` (the `=` forces an exact match so a session named `tatou-…` is never picked).
- If `inherit_user_conf = true`: `source-file -q` the user's own tmux config before tatou's settings, both files tmux itself loads: `~/.tmux.conf` and `$XDG_CONFIG_HOME/tmux/tmux.conf`.
- Last line: `source-file -q <config_dir>/tmux.local.conf` (never created by tatou).
- Values from the config are single-quoted for tmux's parser.
- Popup binding (2.8), status line (3.5) and `[keys.tmux]` (6.3) are added by their own steps; the first two refer to the tatou binary by absolute path (`std::env::current_exe()`), so bindings work without `$PATH`.

**Done when**
- Unit tests on the generated string: defaults, custom back key, prefix, `inherit_user_conf` order, local conf is the last line.
- `tmux -L <test socket> -f /dev/null start-server \; source-file <generated>` succeeds (config is valid tmux; errors in a file given with `-f` are silently ignored, `source-file` reports them).

### 1.7 `tmux.rs`: client and version check (#7)

Depends on: 1.4, 1.6

- A small `Server { socket, conf }` value (named apart from `config::Tmux`); every invocation builds `tmux -u -L <socket> -f <conf> …` through `exec.rs` (`-u` added in 1.8). Production socket is `tatou`; tests pass a unique socket.
- `tmux -V` parser (`tmux 3.4`, `tmux 3.3a`, `tmux next-3.5`) and a check for ≥ 3.2 with a clear error. A version without a number (`tmux master`, built from git) is accepted with a warning in the log.
- Test helper: unique socket per test, server killed on drop, test skipped when `tmux` is missing.

**Done when**
- Parser tests over fixture strings; one integration test starts and kills a server on a unique socket.

### 1.8 Parse tmux sessions (#8)

Depends on: 1.7

- `list-sessions -F` with tab-separated fields: `#{session_id}`, `#{session_name}`, `#{@tatou_path}`, `#{@tatou_status}`, `#{@tatou_agent_pane}`, `#{session_path}`, `#{session_activity}`. (`session_id` and `session_path` extend the format in DESIGN §8.1: the id is an unambiguous target, the path is the matching fallback. Update DESIGN.)
- Pure parser over fixtures in `tests/fixtures/tmux/`; "no server running" maps to an empty list, not an error.
- tmux replaces tabs and non-ASCII characters in `-F` output with `_` unless the client is UTF-8, so every invocation passes `-u`.
- `@tatou_status` parses to busy / input / idle; any other value counts as not set. A line in an unexpected shape (a tab or newline in a path) is skipped with a warning in the log.

**Done when**
- Fixture tests: no sessions, sessions with and without `@tatou_*` options, paths with spaces.
- Integration test: create two sessions on a unique socket, set options, read them back.

### 1.9 Parse `git worktree list --porcelain` (#9)

Depends on: 1.4

- Pure parser to `Worktree { path, head, branch: Option, detached, bare, locked, prunable }`; the first entry is the main worktree. `locked` and `prunable` carry git's reason (empty when a lock has none); `branch` drops `refs/heads/`.
- Read line by line, without `-z` (which needs git ≥ 2.36; Ubuntu 22.04 ships 2.34): a path containing a newline cannot be read back, and tatou does not support one.
- Fetch function `worktrees(repo)` running `git -C <repo> worktree list --porcelain`.
- Errors per project (not a repo, git missing) are values, not panics.

**Done when**
- Fixture tests in `tests/fixtures/git/`: single worktree, several, detached HEAD, bare main, locked (with reason), prunable.
- TempDir test: init a repo, add a worktree, list it.

### 1.10 Command-line entry points (#10)

Depends on: 1.3

- `main.rs` with plain `std::env::args` parsing (no CLI crate): `tatou` (launcher), `tatou --dashboard`, and stubs for `--popup`, `status`, `hooks` that later steps fill in; `--help`, `--version`.
- Unknown arguments print usage and exit with status 2.
- `-h` and `-V` are accepted as short forms. Until its step lands, each command prints `tatou: <command> is not implemented yet (#n)` and exits with status 1.
- `status` logs at `warn` by default (it runs every few seconds from the status line); `--help` and `--version` do not open the log.

**Done when**
- Unit tests for argument parsing; `--version` prints the version from `VERSION`.

### 1.11 Launcher (#11)

Depends on: 1.6, 1.7, 1.10

- Check `tmux` exists and is ≥ 3.2; check `git` exists. Clear message and exit otherwise.
- Regenerate `tmux.conf`. If the tatou server is already running, `source-file` it so changes apply without a restart.
- Nesting (DESIGN §5): if `$TMUX` points at the tatou server, just `switch-client -t '=tatou'`. If it points at another server, warn and ask: attach anyway (with `TMUX` unset) or quit.
- Attach: `exec_replace` of `tmux -L tatou -f <conf> new-session -A -s tatou '<exe> --dashboard'`.
- `--dashboard` started outside the tatou server prints a hint to run `tatou` and exits.
- Lives in `launcher.rs`. An invalid `config.toml` prints the error and exits 1. Reloading unbinds the keys and prefix the previous `tmux.conf` set, so a changed key does not stay bound. Inside the tatou server, a closed dashboard session is created again before switching. The dashboard window is named `tatou`.

**Done when**
- Manual check documented in the PR: first launch creates the session; a second terminal re-attaches; launching inside the user's tmux warns.

### 1.12 `model.rs`: merge config, git and tmux (#12)

Depends on: 1.5, 1.8, 1.9

- Pure function `(config, worktrees per project, sessions) -> Vec<ProjectView>`; each `WorktreeView` has path, branch/name, optional session (id, name, agent pane) and status.
- Session matching by path only (`@tatou_path`, then `session_path`), comparing canonicalised paths (macOS `/var` vs `/private/var`). Never by session name.
- Status: `@tatou_status` (`busy` / `input` / `idle`) when set; otherwise activity-based (`active` vs `quiet` from `session_activity` and a threshold); `none` without a session. Verify `session_activity` tracks pane output; if not, use the agent window's `window_activity` instead and update DESIGN.
- The `tatou` dashboard session is never shown as a worktree.
- Verified: `session_activity` changes on keyboard input only, so sessions are read with `list-panes -a` (session fields plus `#{pane_id}` and `#{window_activity}` per pane) and the activity is the agent pane's window, or the most recent window without one. Active means output within 10 s. The dashboard session is recognised by its exact name `tatou`; bare main repositories are not listed.

**Done when**
- Unit tests: match by option, fallback to session path, two sessions on one worktree (first wins, deterministic), worktree without session, session without worktree ignored, each status.

### 1.13 Background refresh (#13)

Depends on: 1.12

- Worker threads (`std::thread` + `mpsc`) send results to the UI loop: tmux sessions every ~1 s, git worktrees every few seconds and on demand.
- One message enum; each result carries its project so a slow or failing project does not block others.
- The UI loop never waits on a command; errors become visible status text and log lines.
- In `refresh.rs`: one long-lived thread for tmux (every 1 s) and one per project (every 5 s), each woken at once on request (`recv_timeout`). Errors are logged when they appear, change or clear, not on every refresh. `model::Sources` keeps the latest result of each and builds the view; on a tmux error the last sessions read are kept.

**Done when**
- Unit test with a fake source feeding the channel; no `unwrap` on send/recv (a closed channel ends the worker quietly).

### 1.14 UI shell and keymap skeleton (#14)

Depends on: 1.10, 1.13

- Terminal setup: raw mode, alternate screen, mouse capture; restore on every exit path, including a panic hook that restores before printing the panic.
- Event loop: `crossterm::event::poll` with a short timeout, then drain the refresh channel, then render.
- `keys.rs` with an `Action` enum and the hard-coded default keymap from DESIGN §7; the UI dispatches actions, never raw keys (so 6.1 only adds overrides). Quit with `q` / `Ctrl-c`.
- Layout frame: left pane, (empty) right pane, bottom hint bar.
- Terminal handling is tatou's own (`ui/terminal.rs`) rather than `ratatui::init`, whose panic hook leaves mouse capture on. It restores once only: a second "leave alternate screen" after a panic moved the cursor back over the panic message. A panic in a background thread is logged instead of printed over the UI.
- The loop waits up to 100 ms for input, applies updates, and redraws every tick. Every action has its `[keys]` name, a hint label and its pane; the hint bar lists the focused pane's actions. In debug builds, `TATOU_PANIC=1` panics after the first frame, for the restore check.

**Done when**
- After a forced panic in a debug build, the terminal is usable again.
- `TestBackend` test renders the empty frame.

### 1.15 Left pane: projects and worktrees (#15)

Depends on: 1.14

- Collapsible projects (`▾` / `▸`), worktrees with status icons (`●` busy, `⚠` input, `○` idle, `·` no session), branch or name, main worktree marked.
- Navigation: `j`/`k`/arrows, `h`/`l` or Left/Right to collapse/expand, selection kept across refreshes (by path).
- Empty state with the config path; per-project errors shown inline.
- Project rows are selectable, tree-style: `h` on a worktree moves to its project, then folds it; `l` unfolds it, then moves to its first worktree. Folding lasts while the dashboard runs. The main worktree is tagged `repo`; without hooks, `active` and `quiet` show as a dim `●` and `○`. A project's error shows under its name, a tmux error at the bottom of the pane.

**Done when**
- `TestBackend` tests: two projects, collapsed project, each status icon, empty state.

### 1.16 Jump to a session (#16)

Depends on: 1.15

- `Enter` on a worktree with a session: `switch-client -t <session_id>`.
- Without a session: a message in the hint bar (starting agents comes in 2.5).
- Hint bar shows the keys available in the current context.
- `switch-client` runs on a thread of its own; without `-c`, tmux finds the dashboard's client from `$TMUX`. A failure shows tmux's message in the hint bar and goes to the log. Enter on a project row folds or unfolds it. The hint bar shows only `n`, `?` and `q` on a project row, and no jump on a worktree without a session; a message replaces it until the next key.

**Done when**
- Manual check documented in the PR: jump to a session started by hand with `tmux -L tatou new-session -c <worktree>`, back with `Alt-d`.

### 1.17 Reload `config.toml` in the running dashboard (#67)

Depends on: 1.13, 1.15

- The dashboard checks `config.toml`'s modification time every few seconds and reloads it when it changed, so a project added by `tatou add` (1.18), at launch (1.19) or by hand appears without a restart.
- `Refresh` starts listing new projects and stops removed ones; the selection is kept by path.
- An invalid edit shows its error in the dashboard and keeps the last valid config, rather than emptying the screen.
- `[tmux]` changes need `tatou` to be run again (the launcher regenerates `tmux.conf`); the dashboard says so instead of reloading the tmux server itself.
- A worker in `refresh.rs` checks the modification time every 2 s and loads the file on a change, so the UI thread never reads it; the time is taken before the dashboard's own load, so an edit made meanwhile is not missed. Refresh now starts with an invalid config too, which recovers once fixed. The reload error shows on the Projects pane's bottom border, the reason first, as `config.toml: <reason> (line, column)`; the `[tmux]` notice is a hint-bar message, given once per change and not when `[tmux]` goes back to what tatou was launched with.

**Done when**
- Unit tests: a changed file is reloaded once, an unchanged one is not, an invalid one keeps the previous config and reports the error; new and removed projects start and stop their refresh.

### 1.18 `tatou add [path]` (#68)

Depends on: 1.5, 1.9, 1.10

- `tatou add` (the current folder) or `tatou add <path>`: resolves the repository's main worktree (the first entry of `git worktree list`, so running it inside a linked worktree adds the main repository), and fails with git's message if the path is not in a repository.
- Already a project (same canonical path): says so and changes nothing.
- Appends a `[[project]]` block with `path` (written with `~/` when under `$HOME`) to the end of `config.toml`, written atomically; the rest of the file, comments included, is left as it was. The appended file must still load (checked before replacing).
- Prints what was added and the config path.
- `git::main_worktree` and `config::add_project` are what 1.19 and 6.5 reuse. Already a project exits 0. Outside a repository: `tatou: cannot add <path>: <git's message>`. A symlinked `config.toml` (dotfiles) is written through to its target, keeping the link and the file's permissions; `write_atomic` moved from `tmux.rs` to `config.rs`, so `tmux.conf` gets the same.

**Done when**
- TempDir tests: add from the repository root, from a subfolder and from a linked worktree (all add the main worktree); already present; not a repository; the result loads and keeps the original text byte for byte.

### 1.19 Offer to add the current repository at launch (#69)

Depends on: 1.11, 1.18

- `tatou` started inside a git repository that is not a project asks `Add ~/code/my-app to tatou? [Y/n]` before attaching, and adds it with 1.18's code on yes.
- Only when stdin is a terminal; the launcher's nesting question (1.11) comes first.
- Decided: a declined repository is not remembered and is asked about again at the next launch. The offer runs wherever `tatou` runs with a terminal, inside tatou's server too; Enter means yes; a failure to add is shown and the launch goes on.
- Projects move out of `config.toml` into one file each, `projects/<folder name>.toml` (DESIGN §9): `tatou add` writes the file instead of appending `[[project]]` (now an error pointing to `projects/`), and the reload's watch covers the project files too (added, removed or edited). CLAUDE.md's no-state rule and DESIGN §1, §3, §5, §7, §8.1 and §9 say so.

**Done when**
- Unit tests for the decision (inside a project, inside a new repository, outside any repository, no terminal); manual check documented in the PR.

### 1.20 Minimal README for dogfooding (#17)

Depends on: 1.16, 1.19

- What tatou is (one paragraph), requirements, `cargo install --path .`, adding projects (`tatou add`, the launch offer, or a file in `projects/`), how to run, how to get back to the dashboard.

**Done when**
- Following the README on a clean machine gets to a working dashboard.

---

## M2 — Manage

New and remove worktrees, start/restart agents, open a shell, popup mode. After this milestone tatou is a useful tool.

### 2.1 Git: add and remove worktrees (#18)

Depends on: 1.9

- `add(repo, name, base, path)`: `git -C <repo> worktree add -b <name> <path> <base>`. Default path `<repo>/../<repo>.worktrees/<name>`, or `<worktree_dir>/<name>` when configured.
- Validate the branch name with `git check-ref-format --branch` before running anything.
- `remove(repo, path, force: bool)`: `git worktree remove [--force] <path>`; git's stderr is returned verbatim on failure.
- The main worktree can never be removed.
- `worktree_path(repo, name, worktree_dir)` gives the default path; a `/` in the name becomes `-` in the folder. A name is valid when `check-ref-format --branch` prints it back unchanged (it expands `@{-1}`), else `Error::InvalidBranch`. `--` comes before the path, so a base starting with `-` is a commit, never an option. Removing the main worktree is `Error::MainWorktree`, compared by canonical path. In unit tests, `exec` runs git with `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1`.

**Done when**
- TempDir tests: add with default and custom dir, invalid name rejected, remove clean, remove dirty fails with git's message, forced remove succeeds.

### 2.2 tmux: create, restart and kill sessions (#19)

Depends on: 1.8

- `new_session(name, path, command)`: `new-session -d -s <name> -c <path> -e TATOU=1 -P -F '#{session_id} #{pane_id}' <command>`, then set `@tatou_path` and `@tatou_agent_pane` with `-t <session_id>` (`set-option -t =<name>` fails: it takes a pane target).
- Session names: `<repo>-<name>`, with characters tmux rejects (`.`, `:`) replaced; append `-2`, `-3`… if taken. Names are cosmetic; matching stays by path.
- `respawn_agent(pane, command)`: `respawn-pane -k -t <pane>`; `new_window(session, path, command)`; `kill_session(id)`.
- tmux 3.8 accepts `.` and `:` in names but reads them in targets as window and pane separators, so `session_name` replaces them with `-`, like `/` in worktree folders (2.1); a taken name is found from tmux's `duplicate session` error. `new_session` returns `NewSession { id, name, pane }`; if tagging fails, the session still matches by its start directory. `new_window` takes an optional command (none: the default shell) and returns its pane. Tests run on servers whose default shell is `/bin/sh`, and `TestServer` removes its socket file, which tmux leaves behind.

**Done when**
- Integration tests on a unique socket: create (options set, `TATOU=1` in the session environment), respawn, new window, kill; name collision handling.

### 2.3 UI dialogs (#20)

Depends on: 1.15

- Reusable modal widgets: text input (with cursor, backspace, Esc to cancel), choice list, yes/no confirm, scrollable message (for git errors).
- Modals take focus; the background keeps refreshing.
- In `ui/dialog.rs`. Several values are one `Form` of text and choice fields (2.4: name, base, agent in one dialog), so a single text input is a one-field form; a form can show an error and stay open (2.4, 6.5). Text editing: ←/→, Home/End, Backspace, Delete, Ctrl-a/e/u/k/w, by characters. Confirm: `y`/`n`, Enter takes the dialog's default, Esc is no. Messages wrap by character, keeping command output as printed. Keys are fixed, not in the keymap; the hint bar shows them. `App::answered` is where each step acts on its dialog.

**Done when**
- `TestBackend` tests for each dialog's rendering; unit tests for the text input editing.

### 2.4 New worktree (`n`) (#21)

Depends on: 2.1, 2.2, 2.3

- Dialog: name, base branch (default: project `base`, else the branch checked out in the main worktree), agent (default: `default_agent`, choices from `[agents]`).
- Runs on a background thread: `git worktree add` → `on_create` via `run_sh` in the new worktree (if set) → `new-session` with the agent command → set options. Progress shown; the UI stays responsive.
- `on_create` failure: show its output, keep the worktree, still start the session.
- The new worktree is selected when it appears.
- The sequence lives in `actions.rs` (the commands behind each action, run on threads of their own), which reports events to the UI: progress, git's refusal, the worktree added, `on_create` failed, the agent started, a failure. The form stays open and busy until git answers; a refusal shows git's message in the form, to fix and confirm again. Once added, the form closes and progress shows in the hint bar until the agent runs. A failed `on_create` opens a message with its output. The worktrees' and sessions' refresh is woken after each.

**Done when**
- Manual check documented in the PR; unit test for the step sequencing with failures injected where practical.

### 2.5 Start / restart agent (`s`) (#22)

Depends on: 2.2, 2.3

- No session: create one as in 2.4 with the default agent.
- Session exists: confirm, then `respawn-pane -k` on `@tatou_agent_pane`; if that pane no longer exists, open a new window with the agent and update `@tatou_agent_pane`.
- `s` asks for the agent every time, in a choice list with `default_agent` selected (no memory of the last one). The restart confirmation is `Restart <agent> in <worktree>? [Y/n]`, Enter meaning yes. A session started by hand, without `@tatou_agent_pane`, gets the agent in a new window too, so nothing the user started is killed; tmux's `can't find pane` tells a pane that is gone.

**Done when**
- Integration test for the pane-gone fallback on a unique socket.

### 2.6 Open a shell in the worktree (`t`) (#23)

Depends on: 2.2

- Session exists: `new-window -t <session> -c <path>`, then switch to it.
- Decided: no session yet → create one whose first window is `$SHELL` (tmux's default shell), tagged with `@tatou_path` but no `@tatou_agent_pane`; a later `s` starts the agent in a window of its own there (2.5). `Server::new_session` takes an optional command for it, like `new_window`.

**Done when**
- Manual check documented in the PR.

### 2.7 Remove worktree (`d`) (#24)

Depends on: 2.1, 2.2, 2.3

- Confirm dialog naming the worktree path and branch. Then kill the session, then `git worktree remove`.
- If git refuses, show its message and offer a second, explicit "force remove" confirmation. Never force silently. The branch is never deleted.
- Not available on the main worktree.
- Decided: `git worktree remove` first, and the session is killed only once it succeeded, so a refusal leaves the agent running (the reverse of the order above). Both confirmations are `[y/N]`; the second shows git's message as printed, then `Force the removal? …`. A forced removal git still refuses (a locked worktree) opens its message. `d` is hidden on the main worktree.

**Done when**
- Manual check with a dirty worktree documented in the PR.

### 2.8 Popup mode (#25)

**On hold** (label `on hold` on #25): not needed now, and to be studied in depth before it is built. Until then `tatou --popup` says it is not implemented, and the `popup` key in `[tmux]` binds nothing.

Depends on: 1.16, 1.6

- `tatou --popup`: same UI; jumping or launching an action exits, which closes the popup.
- Generated `tmux.conf` binds the popup key: `bind-key -n <popup> display-popup -E -w 90% -h 80% '<exe> --popup'`.
- Verify which client `switch-client` targets from inside a popup; if ambiguous, pass `#{client_name}` from the binding as an argument.
- Verify how actions that open their own popup (custom actions such as `display-popup … lazygit`, 6.2) behave when started from a popup (tmux does not nest popups) and document the chosen approach.
- Only the dashboard instance will send notifications (3.4); the popup must not.

**Done when**
- Unit test on the generated binding; manual check from a worktree session documented in the PR.

### Post-milestone fixes

Found while using M2; each is an issue on the M2 milestone.

### 2.9 Sessions pane below Projects (#85)

Depends on: 1.16

- The left column splits: **Projects** on top, **Sessions** below, listing every session on tatou's server except the dashboard's, including sessions that match no worktree: its status icon, its name, and dimmed, the `project/worktree` it belongs to, or the folder it started in (with `~/`).
- `j`/`k` and arrows move; `Enter` jumps (`switch-client`, as in 1.16). `Tab` cycles Projects → Sessions → Files. The selection follows its session by id across refreshes.
- Decided: Sessions is as tall as its rows, up to a third of the column, and scrolls past that; with none, one line says "No sessions". When a third of the column has no room for one row (a terminal under about ten rows), it is not drawn, so the projects keep theirs. tmux's errors move to its bottom border (to the projects' when it is not drawn).
- The rows come from the sessions already listed (1.13): `Sources::session_rows`.

**Done when**
- `TestBackend` tests: sessions matched and unmatched, the dashboard's session left out, no sessions, the third of the column; `Enter` queues the switch; `Tab` cycles the three panes; the selection follows its session.

### 2.11 Drop the built-in `shell` agent (#87)

Depends on: 2.5

- `s` always starts an agent: the built-in `shell = "$SHELL"` agent is dropped, so neither `s` nor the `n` form offers a shell; shells are `t`'s job.
- Built-in agents become `claude` and `codex`. A user can still define a shell-like agent in `[agents]`, and it is offered like any other, also as `default_agent`.
- The config template and DESIGN §9 follow.

**Done when**
- Config unit tests: the built-in agents without `shell`; a user-defined `shell` still works and can be `default_agent`.
- UI tests: the `s` choice and the `n` form list no `shell` by default.

### 2.12 New worktree base from the selected row (#88)

Depends on: 2.4

- The `n` form's **Base** follows the selected row:
  - **On a worktree row** (the main one too): that worktree's branch, so the new worktree starts from it.
  - **On a project row**, or a detached worktree: the project's `base` setting first, then the remote's default branch as `origin/main` (the remote-tracking branch, as of the last fetch; tatou never fetches), then the branch checked out in the main worktree.
- `git::remote_default` reads `origin/HEAD` (`symbolic-ref --quiet`: exit 1 means not set) on the project's refresh thread, with its worktrees; the model keeps it on the project's view.

**Done when**
- git tests in a TempDir with a local "remote": `origin/HEAD` read, and its absence handled.
- UI tests: the Base prefilled on a project row and on a worktree row, and for a detached worktree.

### 2.13 Sessions pane shows the program in each session's current window (#93)

Depends on: 2.9

- Each row of the Sessions pane shows what runs in the session's current window, as tmux names its foreground program: `● app-fix  claude  app/fix`, `● notes  vim  ~/notes`.
- Read with the sessions, in the same `list-panes -a` call: its format gains `#{window_active}`, `#{pane_active}` and `#{pane_current_command}`. Every window has an active pane; the session's is the one active in its current window. No new tmux command, no tag; it follows window switches within a refresh.
- tmux names a program by its file, and Claude Code's file is named after its version (`~/.local/share/claude/versions/2.1.310`, started through the link `~/.local/bin/claude`), so `#{pane_current_command}` shows `2.1.310`. So the program comes from `ps -A -o pid=,ppid=,pgid=,tpgid=,tty=,args=`, once per refresh: for the current pane's terminal (`#{pane_tty}`), the leader of its foreground process group, or the leader's command in the group when the leader is a shell (`sh -c 'cd x && claude'`), by the name it was started under (its first argument, without folder or a login shell's `-`). `claude` typed by hand in a shell shows as `claude` too. Without `ps`, or nothing for that terminal, tmux's name.

**Done when**
- Parser tests on the captured `list-panes` fixture, extended with the four fields: the current window's active pane's program is the session's.
- `TestBackend` test: the program shows between the name and the place.

### 2.14 Show which pane has the focus (#95)

Depends on: 2.9

- `Tab` moves the focus between Projects, Sessions and Files; a bold border alone was hard to see.
- Unfocused panes are drawn dim (the block's base style, which their borders, titles and contents inherit); the focused pane keeps full brightness, with its border and title in cyan and bold, as the `repo` tag.
- The selection highlight stays visible in a dim pane. Error titles on borders (config reload, tmux) stay bright red. Dialogs are unchanged.

**Done when**
- `TestBackend` tests on the buffer's styles: the focused pane's border and title in the color, the others dim, for each of the three panes after `Tab`; contents dim with their pane, the selection kept; errors bright.

### 2.15 Sessions pane as a tree of sessions and their windows (#97)

Depends on: 2.13

- The Sessions pane becomes a tree, like Projects: every session (as in 2.9), and under each, its windows: index, the program in the foreground of its active pane (as in 2.13), `*` on the session's current window.
- Sessions start unfolded; `j`/`k` move over sessions and windows; `h`/`l` fold and unfold, `h` on a window moves to its session, `l` on an unfolded session moves to its first window. Folding lasts while the dashboard runs.
- `Enter` on a session jumps to it; on a window, `select-window -t <window id>`, then the jump to its session (`Task::Window`).
- The selection follows its session or window by id across refreshes; the pane's height counts the visible rows, up to a third of the column as before.
- `list-panes -a`'s format gains `#{window_id}` and `#{window_index}`; each window is named by its active pane.

**Done when**
- Parser tests: windows per session, in index order, each with its active pane's program.
- `TestBackend` tests: the tree with its windows and the current window's `*`; folding and unfolding; `h` from a window to its session; Enter on a window queues selecting it and the switch; the selection kept by id.
- Manual check documented in the PR: Enter on a window lands on that window.

### 2.16 Back to the dashboard when a window's program exits (#99)

Depends on: 2.4, 2.5, 2.6

- When the agent or shell of a window tatou opened exits, the client that was looking at that window goes back to the dashboard (tmux would show another window, or detach the client from its session's last one).
- Decided: the commands tatou starts are wrapped, in `new_session`, `new_window` and `respawn_agent`: `<command>`, a newline (so a `# comment` at its end comments out nothing more), then a POSIX `sh` snippet run while the pane still exists, switching to `=tatou` only the clients whose current window is this pane's (`list-clients -f "#{==:#{window_id},$w}"`). A program ending in a window nobody looks at moves nobody; without a dashboard session the switch fails quietly. A shell runs as `"${SHELL:-/bin/sh}" -l`, a login shell as tmux would start it. An agent command starting with `exec` never comes back to run it. Windows opened by hand stay plain tmux.
- The program column (2.13) still names `claude` / `zsh`: the wrapping shell leads the foreground group, and its command is what `ps` names.
- Safety net: the generated `tmux.conf` sets `detach-on-destroy off`.
- Found in review, and fixed here: with the safety net, `q` (which ends the dashboard's session) left the client in another session, and the back key's `switch-client -t '=tatou'` then failed. Now `q` leaves tatou: the dashboard detaches the clients looking at it (`detach-client -s =tatou`) once the terminal is restored, then ends. And the back key starts the dashboard again when it is gone: `if-shell -F '#{S:…}'` looks for a session named exactly `tatou`, `{ switch-client }` or `{ new-session -d … ; switch-client }`, braces keeping the dashboard command's quoting (`new-session -A` would attach only a terminal's client). `tmux::dashboard_command` is shared with the launcher.

**Done when**
- Unit tests: the wrapped command for an agent and for a shell; the generated conf has `detach-on-destroy off`.
- Integration tests on a unique socket, with a control-mode client: a wrapped command exiting in the viewed window moves that client to `tatou`; one exiting in another window leaves it where it is.
- Manual check documented in the PR: `exit` in a shell from `t`, and an agent ending, land on the dashboard.

### 2.17 Name windows in the status bar after their session (#101)

Depends on: 1.12

- In an agent's session, tmux's status bar showed `[kdbg-mast] 0:2.1.310* 1:bash-`: the session's name cut at 10 characters (`status-left-length`), and windows named after their program's file, which for Claude is its version.
- Decided: every window shows as `<session>-<index>`, the session's name being `<project>-<worktree>`: `kdbg-master-0* kdbg-master-1-`. The generated `tmux.conf` sets `status-left ''` and both `window-status-format` and `window-status-current-format` to `#S-#I#{?window_flags,#{window_flags}, }` (tmux's own format, the session's name in place of the window's). `tmux.local.conf` can override them.

**Done when**
- Unit test: the generated conf has the three options; tmux reads the format back as written.
- Manual check documented in the PR: an agent's session shows `<project>-<worktree>-<index>` for each window.

### 2.18 Drop the Files pane; `e` edits the worktree (#103)

Depends on: 2.6

- Decided: no Files pane. The right-hand file tree (M4) is dropped with every file action (view `v`, diff `D`, send to agent `@`, copy path `y`, open `o`), the ignored-files toggle (`show_ignored`, now an unknown key in `config.toml`) and `platform::open`. Projects and Sessions take the full width; `Tab` moves between those two.
- `e` on a worktree, or on a project (its main worktree, the repository), opens a window in its session running `${EDITOR:-vi} .` in the worktree, or a new session with just the editor (no agent pane) when it has none, then switches to it; quitting the editor lands on the dashboard (2.16). `t` and `e` share the task (`Task::Program`); `$EDITOR` stays unquoted so that `code -w` keeps its argument.
- M4's steps 4.1–4.6 are closed as not planned; 4.7, mouse support, moves to M6 as 6.6. 3.3 is notify only; `{file}` leaves 6.2's templates.

**Done when**
- `TestBackend` tests: no Files pane, `Tab` between Projects and Sessions only; `e` in the hints of a worktree and a project.
- Unit test on the editor task for a worktree and for a project; integration tests on a unique socket: the editor opens in the worktree's session, or in a new session without an agent pane.
- Manual check documented in the PR: `e` on a worktree opens `$EDITOR`, quitting it lands on the dashboard.

---

## M3 — Status

Hook-based agent status, `tatou hooks`, notifications, `tatou status` in the tmux status line.

### 3.1 `tatou hooks` (#26)

Depends on: 1.10

- Prints, for the user to paste (tatou never edits agent configs): the Claude Code hooks JSON from DESIGN §8.4, and the generic one-liner `[ -n "$TATOU" ] && tmux set-option -q @tatou_status <state>; true` for other agents, with notes for Codex and Gemini CLI where their hook mechanism is known.
- Check whether `input` needs clearing after the user answers a permission prompt (e.g. a `PreToolUse`/`PostToolUse` hook setting `busy`), and include it if so.

**Done when**
- A test checks that the Claude section is valid JSON (`serde_json` as a dev-dependency; it becomes a normal one in 5.1). Snippets are POSIX `sh`.

### 3.2 Verify hook-driven status end to end (#27)

Depends on: 3.1, 1.12

- With the hooks installed, run a Claude Code session in a tatou worktree and confirm the icon cycles busy → input → idle within ~1 s.
- Confirm sessions without hooks fall back to active/quiet and that `TATOU` unset makes the hooks a no-op outside tatou.

**Done when**
- Findings recorded in the PR; DESIGN §8.4 corrected if the hooks needed changes.

### 3.3 `platform.rs`: notify (#28)

Depends on: 1.4

- `notify(title, body)`: `notify-send` on Linux; on macOS `osascript` with the text passed as `argv` (`on run argv … display notification (item 2 of argv) with title (item 1 of argv)`), never interpolated into the script. Missing tool → silently skipped, logged.
- All `cfg(target_os)` lives here and nowhere else.

**Done when**
- Unit tests on the built argument vectors (title/body with quotes and backslashes); no test actually shows a notification.

### 3.4 Notify when an agent needs input (#29)

Depends on: 3.3, 1.13

- The dashboard compares statuses between refreshes (in memory only) and, on a transition to `input`, notifies unless a client is currently viewing that session (`list-clients -F '#{client_session}'`).
- Also `tmux display-message` with the worktree name.
- Popup instances never notify.

**Done when**
- Unit test for transition detection (input from busy/idle/none, no repeat while staying in input, suppressed when viewed).

### 3.5 `tatou status` and the tmux status line (#30)

Depends on: 1.8, 1.12

- `tatou status` queries tmux once and prints a short summary such as `⚠2 ●3` (empty when nothing to report). It must be fast and must not run git or gh.
- Generated `tmux.conf` sets `status-right` to `#(<exe> status)` and a short `status-interval`.

**Done when**
- Unit tests for the summary formatting; generated-conf test includes the status line.

---

## M4 — Files (dropped)

Dropped in 2.18 (#103): no file pane. Editing is `e`'s, with `$EDITOR` (2.18); reading and diffing files is the editor's job and the agent's. Its steps 4.1–4.6 (#31–#36) were closed as not planned; mouse support (4.7, #37) moved to M6 as 6.6.

---

## M5 — PRs

PR/CI badge per worktree and open PR.

### 5.1 `gh.rs`: parse PR list (#38)

Depends on: 1.4

- `gh pr list --state all --limit 100 --json number,headRefName,state,isDraft,statusCheckRollup,url`, run in the project directory.
- Pure parser; badge per branch: number, state (open / draft / merged / closed) and checks summary `✓` all passed, `✗` any failed, `●` pending, from both `CheckRun` and `StatusContext` entries. Open PRs win over closed ones for the same branch.
- Uses `serde_json` (comes with gh's JSON; confirm adding it, or justify).

**Done when**
- Fixture tests in `tests/fixtures/gh/`: no PRs, open with passing / failing / pending checks, draft, merged, two PRs on one branch.

### 5.2 Fetch PRs in the background (#39)

Depends on: 5.1, 1.13

- Every ~60 s and on demand (a refresh key). Missing `gh`, not authenticated, or no GitHub remote: no badges, one log line, no error spam in the UI.
- `gh` talks to the network: decide whether `exec::run` needs a timeout now (it has none since 1.4).

**Done when**
- Unit test: an error from `gh` maps to "no badges" for that project only.

### 5.3 Show badges and open PR (`p`) (#40)

Depends on: 5.2, 1.15

- Badge after the worktree name: `#42 ✓`; muted for draft/merged/closed.
- `p`: `gh pr view --web <number>` in the project directory; message when the branch has no PR.

**Done when**
- `TestBackend` test for each badge kind.

---

## M6 — Keys

Configurable keymap, custom tmux-command actions, help overlay.

### 6.1 Configurable keymap (#41)

Depends on: 1.14, 1.5

- Key notation parser: single characters (`n`, `@`, `D`), `Enter`, `Esc`, `Tab`, `S-Tab`, `Space`, arrows, `F1`–`F12`, `C-x`, `M-x`.
- `[keys]` overrides defaults by action name; unknown action names and duplicate bindings are config errors with clear messages.

**Done when**
- Unit tests: every notation, override, unknown action, conflict.

### 6.2 Custom actions (`[keys.custom]`) (#42)

Depends on: 6.1

- Template substitution of `{path}`, `{session}`, `{pane}` with shell-quoted values, then `run_sh` of `tmux -L tatou -f <conf> <template>`.
- An action needing a value that is not available (no session, no agent pane) is disabled with a message.

**Done when**
- Unit tests for substitution and quoting, including values containing quotes and `{`.

### 6.3 Extra tmux bindings (`[keys.tmux]`) (#43)

Depends on: 1.6, 6.1

- Write each entry as `bind-key -n <key> <command>` into the generated `tmux.conf`, before the local-conf line.

**Done when**
- Generated-conf test; the example from DESIGN §9 produces a config tmux accepts.

### 6.4 Help overlay and hint bar from the keymap (#44)

Depends on: 6.1

- `?` shows the effective keymap (defaults + overrides + custom), grouped by pane.
- The bottom hint bar is generated from the effective keymap for the focused pane.

**Done when**
- `TestBackend` test: an overridden key shows up in the hint bar and the overlay.

### 6.5 Add a project from the dashboard (`a`) (#70)

Depends on: 2.3, 6.1, 1.17, 1.18

- `a` opens a path dialog, prefilled with the folder tatou was launched from; on confirm, the same check and project file as `tatou add`. The reload from 1.17 shows the new project.
- Errors (not a repository, already a project) show in the dialog.

**Done when**
- `TestBackend` test of the dialog; unit test that confirming writes the project file.

### 6.6 Mouse support (#37)

Depends on: 2.15

- Click selects a row in Projects or Sessions and moves the focus there; wheel scrolls the pane under the cursor; clicks on dialogs go to the dialog.
- Moved from M4 (4.7) when the Files pane was dropped (2.18).

**Done when**
- Unit tests mapping mouse events to actions by layout area.

---

## M7 — Ship

Documentation, packaging channels and the first complete alpha. Binaries and GitHub pre-releases already exist since 1.2; packaging is added to `.goreleaser.yaml`, not as new workflows or tools.

### 7.1 User documentation (#45)

Depends on: 6.4, 5.3, 3.5

- README: what and why, requirements (git, tmux ≥ 3.2, optional gh), install, quick start, full config reference, hooks setup (`tatou hooks`), keys, popup mode, `tmux.local.conf`, uninstall (nothing to clean except `~/.config/tatou` and `~/.local/state/tatou`).
- A short screenshot or recording.

**Done when**
- A new user can go from install to an agent running in a worktree using only the README.

### 7.2 `cargo install` readiness (#46)

Depends on: 7.1

- `Cargo.toml` metadata: description, license, repository, readme, keywords, categories, `exclude` for fixtures if large.
- Check the `tatou` name on crates.io; `cargo publish --dry-run` passes. Publishing to crates.io waits for v1.

**Done when**
- `cargo install --path .` and `cargo publish --dry-run` succeed.

### 7.3 Release polish: release notes and archive contents (#47)

Depends on: 1.2, 7.2

- Changelog section grouped from commit messages; README, LICENSE included in archives.

**Done when**
- The next alpha release has the new archives and readable notes; binaries run on a clean Linux and Apple silicon Mac.

### 7.4 `.deb` and `.rpm` packages (#48)

Depends on: 7.3

- `nfpms` in `.goreleaser.yaml`: `deb` and `rpm`, depending on `git` and `tmux`, recommending `gh`.

**Done when**
- Packages from an alpha release install and run on a recent Debian/Ubuntu and Fedora container.

### 7.5 Homebrew tap (#49)

Depends on: 7.3

- A `raqqun/homebrew-tatou` tap repository, filled by GoReleaser (`homebrew_casks`, or `brews` if casks do not fit a CLI binary), with `tmux` and `git` as dependencies; a repository secret with a token that can push to the tap.
- **Decision to confirm:** GoReleaser skips publishing pre-releases to taps by default (`skip_upload: auto`); every build is an alpha until v1, so either publish alphas to the tap or leave the tap empty until v1.

**Done when**
- `brew install raqqun/tatou/tatou` works on an Apple silicon Mac.

### 7.6 AUR package (#50)

Depends on: 7.3

- `aurs` in `.goreleaser.yaml` publishing `tatou-bin` from the release archives, `depends=(git tmux)`, `optdepends=(github-cli)`; an AUR SSH key as a repository secret.
- Same pre-release decision as 7.5.

**Done when**
- `makepkg -si` of the published `PKGBUILD` succeeds in an Arch container.

### 7.7 End-to-end check of the complete alpha (#51)

Depends on: 7.3, 7.4, 7.5, 7.6

- End-to-end manual test on Linux and macOS following the README, from every install channel: launch, create worktree, agent status with hooks, notification, files and actions, PR badge, popup, remove worktree, uninstall.
- Issues found become new issues; the result is the alpha release produced by this step's PR. v1 is decided separately.

**Done when**
- The alpha released by this step installs and passes the check from every channel above.
