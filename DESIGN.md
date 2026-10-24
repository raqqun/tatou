# tatou — Design

A minimal terminal UI for running coding agents (Claude Code, Codex, Gemini CLI, …) in parallel across several repositories, one git worktree per task. tatou navigates and acts; tmux shows the agents.

Targets Linux and macOS (Apple silicon). Windows and the BSDs are out of scope.

## 1. Philosophy

**tatou is a view over tools you already have, not a new system.**

- **No state of its own.** Git knows the worktrees, tmux knows the sessions, `gh` knows the PRs. tatou reads them every time; no database, no daemon, no cache that can drift.
- **Never in the way.** Everything tatou creates is a plain git worktree and a plain tmux session on tatou's own tmux server (`tmux -L tatou`). Create, attach, split or delete them from anywhere and tatou reflects it. Your personal tmux server and config are never touched. Uninstalling tatou loses nothing.
- **Reuse, don't rebuild.** Terminals, persistence, splits, popups → tmux. Git → `git`. GitHub → `gh`. Editing → `$EDITOR`. Viewing → `$PAGER`. Diffs → `git diff` (and the user's pager, e.g. delta).
- **No terminal-in-terminal.** tatou never renders agent output. It switches tmux to the right session or opens tmux windows/popups.
- **Small.** If a feature needs a new subsystem, it probably doesn't belong in tatou.

## 2. What it does

1. Lists projects and their worktrees with a status per worktree: busy / needs input / idle / no session.
2. Creates a worktree (name + base branch) and starts an agent in a tmux session inside it; removes both when done.
3. Jumps to a worktree's agent session with one key.
4. Shows the selected worktree's files on the right, with git status markers, and acts on them (open, view, diff, send path to agent, …).
5. Notifies when an agent needs input.
6. Shows a PR/CI badge per worktree.
7. Exposes everything through configurable keybindings.

## 3. What it deliberately does not do

- Render terminals or agent output.
- Its own session persistence, splits or panes — tmux.
- A database or state file beyond its config: `config.toml` and one small file per project (§9).
- Edit, create, rename or delete files itself — `$EDITOR`, or a shell opened in the worktree.
- A diff viewer or PR review UI — `git diff` in a popup, `gh`/browser for PRs.
- Modify agent configs, `.tmux.conf` or shell rc files — it shows the snippet to add instead.
- Web UI, MCP server, remote machines, issue trackers.
- Windows.

## 4. Building blocks

| Need | Brick |
|---|---|
| UI | `ratatui` + `crossterm` |
| Sessions, switching, windows, popups | tmux |
| Worktrees, file status, diffs | `git` |
| Directory walking (gitignore-aware) | `ignore` crate (from ripgrep) |
| Agent status | Agent hooks setting a tmux session option |
| PR / CI | `gh pr list --json …` |
| Notifications | `notify-send` (Linux) or `osascript` (macOS), plus `tmux display-message` |
| Open with system app | `xdg-open` (Linux) or `open` (macOS) |
| Editing / viewing | `$EDITOR`, `$PAGER` |
| Clipboard | `tmux set-buffer -w` (OSC 52) |
| Config | one TOML file |

## 5. Running modes

tatou runs everything on **its own tmux server** (`tmux -L tatou`) with **its own generated tmux config** (§8.3). Same binary, same UI:

- **Launcher:** `tatou` — typed in any terminal. Regenerates tatou's tmux config, then attaches to the tatou server, creating the `tatou` dashboard session if needed (`tmux -L tatou -f <conf> new-session -A -s tatou "tatou --dashboard"`). Running `tatou` again from another terminal just re-attaches; from inside tatou's server, it switches to the dashboard, starting it again if its session was closed. An invalid `config.toml` stops the launch with the error. When the server is already running, the regenerated config is applied with `source-file`, after unbinding the keys the previous one bound.
- **Dashboard:** `tatou --dashboard` — the full-screen UI, living in the `tatou` session. Selecting a worktree switches the client to that worktree's session; the "back" key (default `Alt-d`) returns to the dashboard.
- **Popup:** `tatou --popup` — the same UI in a floating tmux popup over whatever you're doing, opened with the popup key (default `Alt-g`). Jumping to a session or launching an action closes the popup. The binding is part of tatou's generated tmux config; nothing to add by hand.
- **Add a project:** `tatou add [path]` — adds the repository at `path` (default: the current folder) as a file in `projects/` (§9). Launching `tatou` inside a repository that is not a project offers to add it (`Add ~/code/my-app to tatou? [Y/n]`, asked again next time after a no), and `a` in the dashboard does the same from a path dialog.
- **Status line:** `tatou status` — prints a short summary (e.g. `⚠2 ●3`); tatou's tmux config shows it in the status bar.

**Nesting:** if `tatou` is started from inside the user's own tmux, attaching would nest two tmux servers. tatou warns and offers to attach anyway (with `TMUX` unset) or to quit; the recommended setup is a dedicated terminal tab/window for tatou.

## 6. Layout

```
┌ Projects / worktrees ───────────┬ Files: my-app/fix-login ──────────────────┐
│ ▾ my-app                        │ ▾ src/                                    │
│   ● fix-login     #42 ✓         │     M auth.rs                             │
│   ⚠ new-api       #45 ●         │       main.rs                             │
│   ○ refactor-db                 │   ? notes.md                              │
│ ▸ website                       │   Cargo.toml                              │
│                                 │                                           │
├─────────────────────────────────┴───────────────────────────────────────────┤
│ ⏎ jump  n new  d remove  e edit  v view  D diff  @ send to agent  ? help    │
└─────────────────────────────────────────────────────────────────────────────┘
```

- **Left:** projects (collapsible) → worktrees. Status: `●` busy, `⚠` needs input, `○` idle, `·` no session; without hooks (§8.4), a dim `●` for recent output and a dim `○` otherwise. Each worktree shows its branch (its directory's name when detached); the main worktree is tagged `repo`. PR badge: number + checks `✓ ✗ ●`.
- **Right:** file tree of the selected worktree. Git markers from `git status --porcelain`: `M` modified, `A` added, `?` untracked, `D` deleted. Respects `.gitignore` by default (toggle to show ignored/hidden files). Optional filter: changed files only.
- **Bottom:** key hints for the selected row: on a project, only what applies to a project; on a worktree without a session, no jump. A message (e.g. a failed command) replaces them until the next key. `?` shows the full keymap.
- `Tab` moves focus between panes. The right pane hides automatically when the terminal is narrow.
- Dialogs open over the panes and take every key while the dashboard keeps refreshing behind them: a form (Tab/↑↓ between fields, ←→ through a choice, Enter to confirm, Esc to cancel; text fields edit like a shell line, Ctrl-a/e/u/k/w included), a choice list, a `[y/N]` confirmation whose default each dialog picks, and a scrollable message for command output. Their keys are fixed and shown in the hint bar.
- Mouse support (click to select, scroll) via crossterm.

## 7. Actions

### On a worktree (left pane)
| Default key | Action | Implemented as |
|---|---|---|
| `Enter` | Jump to agent | `tmux switch-client -t <session>` |
| `n` | New worktree | dialog → `git worktree add`, `on_create`, `tmux new-session` |
| `s` | Start / restart agent | `tmux new-session` / `respawn-pane` |
| `t` | Open shell in worktree | `tmux new-window -t <session> -c <path>` |
| `p` | Open PR | `gh pr view --web` |
| `d` | Remove worktree | kill session, `git worktree remove` (confirm; force only if explicitly confirmed) |
| `a` | Add project | path dialog → write a project file in `projects/` (as `tatou add`) |

### On a file (right pane)
| Default key | Action | Implemented as |
|---|---|---|
| `e` | Edit | `tmux new-window -c <worktree> "$EDITOR <file>"` |
| `v` | View | `tmux display-popup -E "$PAGER <file>"` |
| `D` | Diff | `tmux display-popup -E "git -C <worktree> diff -- <file>"` (honours the user's git pager) |
| `@` | Send path to agent | `tmux send-keys -t <agent pane> -l "@<relative path> "` (no Enter) |
| `y` | Copy path | `tmux set-buffer -w <path>` |
| `o` | Open with system app | `xdg-open <file>` / `open <file>` (see §10) |

All keys are configurable (§9). Any action can also be bound to a custom tmux command template.

## 8. How it works

### 8.1 Data flow
```
projects/*.toml ─► projects
git worktree list --porcelain (per project) ─► worktrees
tmux list-panes -a -F '#{session_id}\t#{session_name}\t#{@tatou_path}\t#{@tatou_status}\t#{@tatou_agent_pane}\t#{session_path}\t#{pane_id}\t#{window_activity}'
                                             ─► session, status, agent pane per worktree (matched by path)
gh pr list --state all --json number,headRefName,state,isDraft,statusCheckRollup,url (per project)
                                             ─► PR badge per branch
git status --porcelain + ignore walk (selected worktree only) ─► file tree
```
Refresh: tmux every ~1 s; each project's worktrees every 5 s, on a thread of its own so a slow repository delays only itself; the selected worktree's files on selection change; `gh` every ~60 s and on demand. tatou's own actions wake the matching refresh at once. Commands run on background threads and send results to the UI loop over a channel.

### 8.2 Worktrees
- Create: `git -C <repo> worktree add -b <branch> -- <path> <base>`. Path defaults to `<repo>/../<repo>.worktrees/<name>` (configurable with `worktree_dir`); a `/` in the branch name becomes `-` in the folder's, so `feature/login` goes in `feature-login`.
- Optional per-project `on_create` command, run with `sh -c` in the new worktree.
- Remove: kill the tmux session, then `git worktree remove <path>`. On refusal (dirty tree), show git's message; force only after explicit confirmation.
- Worktrees created outside tatou appear automatically.

### 8.3 Sessions
**Dedicated server.** All tmux commands use `tmux -u -L tatou -f ~/.config/tatou/tmux.conf` (`-u` forces UTF-8: without a UTF-8 locale, tmux prints tabs and non-ASCII characters in `-F` output as `_`). This gives tatou predictable behaviour and its own keybindings, keeps `tmux ls` on the user's personal server uncluttered, and lets tatou change server-wide settings without affecting anything else. The sessions remain plain tmux: `tmux -L tatou attach -t <session>` works from any terminal.

**Generated config.** `~/.config/tatou/tmux.conf` is written by tatou at every launch from `config.toml`, and is never edited by hand. It contains:
- Settings agents need: `escape-time 10`, `extended-keys on` (e.g. Shift+Enter), `focus-events on`, `set-clipboard on`, `allow-passthrough on`, `mouse on`, a large `history-limit`, true-colour terminal features.
- tatou's bindings: popup key, back-to-dashboard key, and any `[keys.tmux]` entries from config.
- A status bar showing `#(tatou status)`.
- As its last line: `source-file -q ~/.config/tatou/tmux.local.conf`, the user's own overrides (created by the user, never by tatou). Optionally, `inherit_user_conf = true` also sources the user's own tmux config first: `~/.tmux.conf` and `$XDG_CONFIG_HOME/tmux/tmux.conf`, the files tmux itself loads.

**One session per worktree:**
```
tmux -L tatou new-session -d -s <repo>-<name> -c <path> -e TATOU=1 -P -F '#{session_id} #{pane_id}' <agent command>
tmux -L tatou set-option -t <session_id> @tatou_path <path>
tmux -L tatou set-option -t <session_id> @tatou_agent_pane <pane_id>
```
Sessions are matched to worktrees by `@tatou_path` (fallback `#{session_path}`, comparing paths with symlinks resolved), never by parsing names; the dashboard's own `tatou` session is never a worktree's. Sessions are targeted by their id (`$3`): `set-option -t =<name>` fails, as it takes a pane target. Extra shells, splits and windows are plain tmux.

### 8.4 Agent status
Agents report their own state by setting a tmux option; `$TMUX` inside a session points at the tatou server and session, `TATOU=1` makes it a no-op elsewhere.

Claude Code (`~/.claude/settings.json`), added by the user:
```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tmux set-option -q @tatou_status busy; true" }] }],
    "Notification":     [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tmux set-option -q @tatou_status input; true" }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tmux set-option -q @tatou_status idle; true" }] }]
  }
}
```
Other agents run the same one-liner from their own notify/hook mechanism. `tatou hooks` prints these snippets.

Without hooks: output in the agent's window within the last 10 s (`#{window_activity}` of the `@tatou_agent_pane` window, or of the most recent window when there is none) gives "active", else "quiet". `#{session_activity}` does not work for this: it changes on keyboard input only. No further guessing.

On a transition to `input` for a worktree that isn't the current tmux session: a desktop notification (see §10; skipped silently if unavailable) and `tmux display-message`.

## 9. Configuration

`$XDG_CONFIG_HOME/tatou/config.toml`, defaulting to `~/.config/tatou/config.toml` on every platform, macOS included, holds the settings; each project is a file of its own in `projects/` next to it. tatou writes only these files and the generated `tmux.conf` (§8.3): `config.toml` once, the commented template when it is missing, and a project file when you add a project. Everything else is yours and left as written. A running dashboard reloads them when they change. A full `config.toml`:

```toml
default_agent = "claude"
# show_ignored = false

[tmux]
popup = "M-g"                # opens `tatou --popup`
back  = "M-d"                # returns to the dashboard
# prefix = "C-b"
# inherit_user_conf = false  # also source your own tmux config before tatou's settings

[agents]
claude = "claude"
codex  = "codex"
shell  = "$SHELL"

[keys]                       # override any default
jump = "Enter"
new_worktree = "n"
send_to_agent = "@"
# custom actions in the tatou UI: key = tmux command template; {path}, {file}, {session}, {pane} are substituted
[keys.custom]
"g" = "display-popup -E -d {path} lazygit"

# extra bindings written into tatou's tmux config (active everywhere on the tatou server)
[keys.tmux]
"M-|" = "split-window -h -c '#{pane_current_path}'"
```

And a project, `projects/my-app.toml`, as `tatou add ~/code/my-app` writes it with the optional settings added:

```toml
path = "~/code/my-app"
base = "main"
# worktree_dir = "~/worktrees/my-app"
# on_create = "cp ../my-app/.env . && npm install"
```

- Every setting is optional. When `config.toml` does not exist, tatou writes it with every setting commented out; without project files, the dashboard says where they go.
- Project files are `projects/*.toml`, listed by file name; hidden files and other names (editor backups) are ignored. `tatou add` names a file after the repository's folder, with `-2`, `-3`, … when the name is taken; matching always goes by `path`. `[[project]]` in `config.toml` is an error that points here.
- Unknown keys are errors, reported with the line. `[keys]` action names are checked against the keymap.
- `[agents]` is merged with the built-in `claude`, `codex` and `shell`: new names add an agent, a built-in name replaces its command. `default_agent` must name one of them.
- `path` and `worktree_dir` accept `~/`; any other path must be absolute. A project whose file does not parse, or whose `path` is missing, invalid or another file's, is shown with its error, and the other projects still load.
- `base` is optional: without it, new worktrees start from the branch checked out in the project's main worktree.

## 10. Implementation notes

- Single Rust crate. Dependencies: `ratatui`, `crossterm`, `ignore`, `serde`, `toml`, `thiserror`, `tracing` + `tracing-subscriber` (logging to `$XDG_STATE_HOME/tatou/tatou.log`, never to the terminal; rotated to `tatou.log.old` at startup once past 1 MiB; level from `$TATOU_LOG`, default `info`, and `warn` for `tatou status`, which runs every few seconds; `--help` and `--version` do not log).
- Paths follow XDG on every platform (`~/.config/tatou`, `~/.local/state/tatou`), honouring `$XDG_*` variables when set. No `~/Library/...` on macOS: CLI users expect `~/.config`.
- All platform differences live in one `platform` module behind two functions, `open(path)` and `notify(title, body)`, using `cfg(target_os)`. Nothing else in the codebase is platform-specific.
- Shell snippets and command templates must be POSIX `sh` and must not assume GNU userland (macOS ships BSD tools).
- Plain `std::thread` + `mpsc` channels for background commands; no async runtime.
- Commands' stdout and stderr are read as UTF-8, lossily: file names that are not valid UTF-8 are not supported. Commands have no timeout; they run on background threads with stdin closed and git/gh prompts disabled, so a stuck command only stalls its own refresh.
- Modules: `error`, `logging`, `config`, `exec`, `launcher`, `platform`, `git`, `tmux`, `gh`, `files`, `model`, `refresh`, `actions`, `keys`, `ui`.
- Parsers for git/tmux/gh output are pure and tested against captured fixtures.
- Runtime requirements: `git`, `tmux` (3.2+ for `display-popup` and `set-buffer -w`). Optional: `gh`, `notify-send` (Linux; macOS uses the built-in `osascript`).
- Versioning: every build is an alpha until a stable v1. The `VERSION` file holds the version as its git tag (`v0.1.0-alpha.N`), mirrored in `Cargo.toml`; each task's pull request bumps `N`. Merging into `main` makes CI tag that version and publish it as a GitHub pre-release with GoReleaser (Rust builder). Tags are never created by hand.

## 11. Milestones

1. **Navigate:** config, generated tmux config, launcher (attach to the tatou server), list projects/worktrees from git, sessions/status from tmux, jump with Enter and back key. Dashboard mode.
2. **Manage:** new/remove worktree, start/restart agent, open shell, popup mode and its binding.
3. **Status:** hook-based status, `tatou hooks`, notifications, `tatou status` for the tmux status line.
4. **Files:** right-pane tree with git markers, edit/view/diff/send-to-agent/copy/open, ignored-files toggle.
5. **PRs:** badges and open PR.
6. **Keys:** configurable keymap, custom tmux-command actions, `?` help overlay.
7. **Ship:** `cargo install`; Linux: AUR, `.deb`/`.rpm`; macOS: Homebrew tap, Apple silicon only (`darwin-arm64`; Intel Macs are not supported). All packaging goes through GoReleaser. From milestone 1, CI tests on Linux and macOS and every merged task is released as an alpha (static `linux-amd64` and `linux-arm64` binaries, a `darwin-arm64` binary).

Milestone 2 is already a useful tool. `MILESTONE.md` breaks these milestones into steps, one GitHub issue each.

## 12. Open questions

- ~~Default tmux server vs. dedicated socket~~ — decided: dedicated server and generated config (§8.3). Revisit only if many users want tatou sessions on their personal server.
- Best default for nesting (§5): warn and ask, or auto-switch to a different prefix when nested.
- Polling tmux vs. control mode (`tmux -C`): start with polling; change only if measurably costly.
- Whether the file tree should auto-refresh via inotify or on interval/selection only: start with interval.
