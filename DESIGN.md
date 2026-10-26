# tatou — Design

A minimal terminal UI for running coding agents (Claude Code, Codex, Gemini CLI, …) in parallel across several repositories, one git worktree per task. tatou lists the work, shows the agent you pick in a pane beside it, and keeps every agent running in a small daemon of its own.

Targets Linux and macOS (Apple silicon). Windows and the BSDs are out of scope.

## 1. Philosophy

**tatou is a view over tools you already have, plus the one thing they lack: a place where all your agents run side by side.**

- **No state of its own.** Git knows the worktrees, `gh` knows the PRs. tatou reads them every time. Its daemon holds the running terminals in memory, and nothing else: no database, no cache that can drift, nothing on disk beyond the config.
- **Never in the way.** Everything tatou creates in your repositories is a plain git worktree; the agents are the ordinary programs you would run yourself. Quitting the dashboard leaves them running; `tatou stop` ends them. Uninstalling tatou loses nothing but what is running.
- **Reuse, don't rebuild.** Git → `git`. GitHub → `gh`. Files → `$EDITOR`, or the agent. Pseudo-terminals → `portable-pty`; terminal emulation → `vt100`, drawn with `tui-term`.
- **One terminal at a time, and no multiplexer.** tatou shows the selected agent's, shell's or editor's terminal next to the projects, emulating just enough to draw it and passing keys through. It does not grow multiplexer features: no splits, layouts, tabs, copy mode or scripting. (Decided in 2.19, #109: tatou used to delegate all of this to tmux and never draw an agent itself.)
- **Small.** If a feature needs a new subsystem, it probably doesn't belong in tatou.

## 2. What it does

1. Lists projects and their worktrees with a status per worktree: busy / needs input / idle / active / quiet / no session.
2. Creates a worktree (name + base branch) and starts an agent in it; removes both when done.
3. Shows a worktree's agent, a shell or the editor in a pane beside the projects, and switches between them with one key.
4. Keeps every agent running when the dashboard quits, in its daemon.
5. Notifies when an agent needs input.
6. Shows a PR/CI badge per worktree.
7. Exposes everything through configurable keybindings.

## 3. What it deliberately does not do

- Splits, layouts, tabs, copy mode, or more than one terminal on screen.
- Keep agents across a reboot, or across the daemon ending: terminals live as long as the daemon.
- A database or state file beyond its config: `config.toml` and one small file per project (§9).
- Show, edit, create, rename or delete files itself: no file tree, viewer or diff viewer — `$EDITOR` (`e`), a shell opened in the worktree (`t`), or the agent.
- A PR review UI — `gh`/browser for PRs.
- Modify agent configs or shell rc files — it shows the snippet to add instead.
- Web UI, MCP server, remote machines, issue trackers.
- Windows.

## 4. Building blocks

| Need | Brick |
|---|---|
| UI | `ratatui` + `crossterm` |
| Terminals | `portable-pty` (pseudo-terminals), `vt100` (screens), `tui-term` (drawing) |
| Sessions that outlive the dashboard | tatou's daemon, over a Unix socket |
| Worktrees, branches | `git` |
| Agent status | Output activity, seen by the daemon; agents' hooks reporting to it (3.1) |
| PR / CI | `gh pr list --json …` |
| Notifications | `notify-send` (Linux) or `osascript` (macOS) |
| Editing | `$EDITOR` |
| Config | one TOML file |

## 5. Running modes

One binary:

- **Dashboard:** `tatou` — typed in any terminal. Checks git, offers to add the repository it is started in (`Add ~/code/my-app to tatou? [Y/n]`, asked again next time after a no), starts the daemon unless it runs, then runs the full-screen dashboard in this terminal. An invalid `config.toml` stops it with the error. `q` quits the dashboard only: every agent, shell and editor keeps running, and `tatou` shows them again, from this terminal or any other. Several dashboards can run at once.
- **Daemon:** `tatou daemon` — started by `tatou` when needed, in a process group of its own and without the terminal, so it outlives the dashboard and the terminal closing. It owns the terminals (§8.3), and ends 10 s after it has no terminal left and no dashboard connected.
- **Stop:** `tatou stop` — ends every terminal, then the daemon.
- **Add a project:** `tatou add [path]` — adds the repository at `path` (default: the current folder) as a file in `projects/` (§9); `a` in the dashboard does the same from a path dialog.

## 6. Layout

```
┌ Projects ──────────────────────┐┌ my-app-fix-login:0 claude ─────────────────────┐
│ ▾ my-app                       ││                                                │
│   ● fix-login     #42 ✓        ││  (the shown terminal: the agent, drawn         │
│   ⚠ new-api       #45 ●        ││   from its screen; keys go to it)              │
│   ○ refactor-db                ││                                                │
│ ▸ website                      ││                                                │
├ Sessions ──────────────────────┤│                                                │
│ ▾ ● my-app-fix-login  claude … ││                                                │
│     0 claude *                 ││                                                │
│     1 zsh                      ││                                                │
│ ▸ ○ my-app-new-api  claude …   ││                                                │
└────────────────────────────────┘└────────────────────────────────────────────────┘
 M-d dashboard  M-l/M-h next/previous session  M-j/M-k next/previous window; …
```

- **Left, top:** projects (collapsible) → worktrees. Status: `●` busy, `⚠` needs input, `○` idle, `·` no session; without hooks (§8.4), a dim `●` for output in the last 10 s and a dim `○` otherwise. Each worktree shows its branch (its directory's name when detached); the main worktree is tagged `repo`. PR badge: number + checks `✓ ✗ ●`.
- **Left, below:** every session the daemon has, each with its status, what runs in its current window (what it was started with: `claude`, `zsh`, `vi`), and the `project/worktree` it belongs to, or the folder it started in; under each, as a tree folded and unfolded as the projects are, its windows (its terminals): index, what runs in it, `*` on the current one. As tall as its sessions, up to a third of the column (scrolling past that), one line saying "No sessions" when there are none, and not drawn when a third of the column has no room for a row. The daemon's errors show on its bottom border.
- **Right:** the shown terminal, titled `<session>:<index> <program>`, at the pane's size, which its program follows. When none is shown, what to press to show one.
- **Widths.** The left column is 40 columns wide, or half the screen when it is narrower than 80; the terminal takes the rest.
- **Bottom:** key hints for the selected row: on a project, only what applies to a project; on a worktree without a session, no jump. With the focus in the terminal, its keys. A message (e.g. a failed command) replaces them until the next key. `?` shows the full keymap.
- **Focus.** `Tab` moves between Projects and Sessions only. `Enter` on a worktree shows its session's current terminal, on a session its current one, on a window that one, and gives the terminal the focus; `n`, `s`, `t`, `e` and restarts show what they start. In the terminal every key goes to its program, but four, which work from anywhere but a dialog and which no program ever sees: **Alt-d** gives the focus back to the dashboard; **Alt-l / Alt-h** show the next / previous session's current terminal, **Alt-j / Alt-k** the next / previous window of the shown session, wrapping around, with the focus in the terminal. The focused pane's border and title are in the theme's accent colour (cyan by default); the others are dim, contents included, keeping their selection and their errors' colour.
- **When the shown program ends**, a neighbour shows instead: another window of its session (the one after, else before), else the next session's current terminal, else the previous one's; with none left, the pane empties and the focus goes back to Projects.
- **Keys and the mouse.** Where the dashboard's terminal supports the kitty keyboard protocol, tatou asks for disambiguated keys, so that Shift-Enter is not Enter; it goes to the program as Alt-Enter (`ESC CR`), which agents read as a new line. Pastes go through, bracketed when the program asked.
- **The mouse** is taken from the dashboard's terminal only when needed (decided in 6.6, #37): while the focus is in Projects or Sessions, or a dialog is open; with the focus in the shown terminal, only while its program asked for mouse reports (vim with `mouse=a`, lazygit, htop). Otherwise it is the terminal's, so a plain drag selects an agent's output, and on the alternate screen the terminal sends the wheel as ↑/↓. While it is taken, text is selected holding the terminal's bypass key (Shift in most Linux terminals, kitty, WezTerm and Alacritty; Option in iTerm2). A click selects a row of Projects or Sessions and moves the focus there, on a `▸`/`▾` folds it; a double click (one cell, within 400 ms) does what Enter does; the wheel moves the selection of the pane under it, not the focus. A click on the shown terminal gives it the focus; clicks, releases, drags, motion and the wheel go to its program as it asked for them, encoded as an xterm sends them (SGR, UTF-8 or legacy). In a dialog, a click selects a choice (a double one chooses it) or focuses a form's field, the wheel moves a choice or scrolls a message; confirmations, and clicks outside a dialog, take no clicks. The hint bar is not clickable.
- **Colours.** The dashboard's colours come from its theme (§9): the background and text, the accent, the selection, errors, the statuses, the `repo` tag and the hint bar's keys. The default theme, `terminal`, uses the terminal's own palette and background. The shown terminal keeps its program's colours; only the cells it leaves in the default colours take the theme's background and text, so the pane matches the dashboard. `T` chooses the theme from a list, each shown on the whole dashboard as it is selected (§7).
- No file pane: reading, editing and diffing files is the editor's job (`e`) and the agent's (decided in 2.18, #103).
- Dialogs open over the panes and take every key while the dashboard keeps refreshing behind them: a form (Tab/↑↓ between fields, ←→ through a choice, Enter to confirm, Esc to cancel; text fields edit like a shell line, Ctrl-a/e/u/k/w included), a choice list, a `[y/N]` confirmation whose default each dialog picks, and a scrollable message for command output. Their keys are fixed and shown in the hint bar.

## 7. Actions

### On a worktree
| Default key | Action | Implemented as |
|---|---|---|
| `Enter` | Show its agent | attach to its session's current terminal |
| `n` | New worktree | dialog → `git worktree add`, `on_create`, then the agent in a new session, shown |
| `s` | Start / restart agent | choose the agent → a new session with it, or confirm → end the agent's terminal and start it again in its session, beside the others |
| `t` | Open shell in worktree | a new terminal running `"${SHELL:-/bin/sh}" -l` in its session, or in a new session; shown |
| `e` | Edit the worktree (on a project, its repository) | as `t`, running `${EDITOR:-vi} .` |
| `p` | Open PR | `gh pr view --web` |
| `d` | Remove worktree | `git worktree remove`, then end its session (confirm; force only if explicitly confirmed; never the main worktree; the branch stays) |
| `a` | Add project | path dialog → write a project file in `projects/` (as `tatou add`) |

### On a session or a window (Sessions pane)
| Default key | Action | Implemented as |
|---|---|---|
| `Enter` | Show it | attach to the window, or the session's current one |
| `d` | End it | confirm (`[y/N]`) → end the window's program, or every program of the session; the worktree and its branch stay |

### Anywhere (but the terminal)
| Default key | Action | Implemented as |
|---|---|---|
| `T` | Choose the theme | a list of every theme, the one in use selected, each shown as it is selected; a theme in error shows why in the hint bar and cannot be chosen → Enter writes `theme = "…"` in `config.toml` (§9), which every running dashboard reloads; Esc puts the theme back |
| `?` | Help | the full keymap |
| `q` | Quit | the dashboard only; every terminal keeps running |

All keys are configurable (§9). Custom actions run a shell command in a new terminal of the worktree's session (6.2).

## 8. How it works

### 8.1 Data flow
```
projects/*.toml ─► projects
git worktree list --porcelain (per project) ─► worktrees
the daemon's list (sessions, their terminals, activity) ─► session and status per worktree (matched by path)
gh pr list --state all --json number,headRefName,state,isDraft,statusCheckRollup,url (per project)
                                             ─► PR badge per branch
the daemon's stream for the shown terminal: its screen, then its output
```
Refresh: the daemon's sessions every ~1 s; each project's worktrees every 5 s, on a thread of its own so a slow repository delays only itself; `gh` every ~60 s and on demand. tatou's own actions wake the matching refresh at once. Commands and requests run on background threads and send results to the UI loop over a channel; the shown terminal's output arrives over its connection as it comes, and the dashboard draws it within ~15 ms.

### 8.2 Worktrees
- Create: `git -C <repo> worktree add -b <branch> -- <path> <base>`. Path defaults to `<repo>/../<repo>.worktrees/<name>` (configurable with `worktree_dir`); a `/` in the branch name becomes `-` in the folder's, so `feature/login` goes in `feature-login`.
- Optional per-project `on_create` command, run with `sh -c` in the new worktree.
- Remove: `git worktree remove <path>`, then, once it is gone, end its session, so a refusal leaves the agent running. On refusal (dirty tree), show git's message; force only after a second, explicit confirmation. The branch is kept.
- Worktrees created outside tatou appear automatically.

### 8.3 The daemon
**Socket.** `$XDG_STATE_HOME/tatou/tatou.sock` (default `~/.local/state/tatou/tatou.sock`), one daemon per user; a second refuses to start. A runtime file, removed when the daemon ends.

**Protocol** (`protocol.rs`): frames of a 4-byte big-endian length then a payload whose first byte says what it is; strings and bytes are length-prefixed. Requests: list, spawn (session or new session name, path, command or none for the shell, agent or not), kill (a terminal), kill-session, attach (terminal, size), input (bytes), resize, stop. Replies: ok, error, sessions, spawned (session and terminal ids), screen, output, exited. The dashboard opens one connection per request, and one long-lived connection for the shown terminal, which also keeps the daemon from ending.

**Terminals.** Each is a pseudo-terminal running `/bin/sh -c <command>`, or the user's shell as a login shell (`"$SHELL" -l`), in the worktree, with `TATOU=1` and `TERM=xterm-256color` (and without `TMUX`), and a `vt100` screen kept from its output, with 10 000 lines of scrollback. A terminal ends with its program; a session with its last terminal.

**Sessions.** One per worktree, named `<repo>-<name>` (`-2`, `-3`, … when taken), matched to worktrees by the path they were started in, comparing paths with symlinks resolved, never by their names. Its terminals are its windows, in the order they were opened; the current one is the one started or shown last. Its agent's terminal is the one its status follows.

**Showing.** Attaching gives the terminal the pane's size (resizing the pseudo-terminal: one size per terminal, the last attach's), sends its screen as escape codes that draw it, input modes included (`vt100`'s `state_formatted`), then its output as it comes. The dashboard feeds both into its own `vt100` screen and draws that. Keys are encoded as an xterm sends them (`term.rs`): characters, Ctrl and Alt, arrows and Home/End in normal or application cursor mode, PgUp/PgDn, Insert/Delete, F1–F12, Shift-Tab.

**Lifetime.** Started by `tatou`; ends 10 s after it has no terminal and no dashboard (the delay covers a dashboard starting, between its first requests), or with `tatou stop`, which ends every program first.

### 8.4 Agent status
Without hooks: output in the agent's terminal within the last 10 s (or in any terminal of a session without an agent) gives "active", else "quiet". The daemon sees every byte its programs print, so this is exact.

With hooks (3.1, planned): agents report their own state, busy, input or idle, by running `tatou mark <state>` from their hooks; it tells the daemon, over its socket, which terminal is in which state (`$TATOU_TERMINAL`, set by the daemon). `TATOU=1` makes it a no-op elsewhere. Claude Code (`~/.claude/settings.json`), added by the user:
```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tatou mark busy; true" }] }],
    "Notification":     [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tatou mark input; true" }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "[ -n \"$TATOU\" ] && tatou mark idle; true" }] }]
  }
}
```
Other agents run the same one-liner from their own notify/hook mechanism. `tatou hooks` prints these snippets.

On a transition to `input` for a terminal that isn't shown: a desktop notification (see §10; skipped silently if unavailable).

## 9. Configuration

`$XDG_CONFIG_HOME/tatou/config.toml`, defaulting to `~/.config/tatou/config.toml` on every platform, macOS included, holds the settings; each project is a file of its own in `projects/` next to it, and themes of your own go in `themes/`. tatou writes only these files: `config.toml` once, the commented template when it is missing, then only its `theme` line when you choose a theme with `T`; and a project file when you add a project. Everything else is yours and left as written. A running dashboard reloads them when they change, the theme files included. A full `config.toml`:

```toml
default_agent = "claude"
theme = "monokai-pro"

[agents]
claude = "claude"
codex  = "codex"

[keys]                       # override any default
jump = "Enter"
new_worktree = "n"
edit = "e"
# custom actions in the tatou UI: key = shell command, run in a new terminal of
# the worktree's session; {path} is substituted
[keys.custom]
"g" = "lazygit"
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
- Unknown keys are errors, reported with the line (`[tmux]` included, since 2.19). `[keys]` action names are checked against the keymap.
- `[agents]` is merged with the built-in `claude` and `codex`: new names add an agent, a built-in name replaces its command. `default_agent` must name one of them. There is no built-in shell agent: shells are `t`'s job, and `s` and `n` start agents; an agent of your own may still be a shell.
- `theme` names a theme, by default `terminal`, the terminal's own palette; `monokai-pro` (Monokai Pro's dark palette) is built in too. A theme is a flat TOML file named after it, every key optional:
  ```toml
  # themes/monokai-pro.toml
  background = "#2d2a2e"  # the dashboard's, and the shown terminal's default cells'
  foreground = "#fcfcfa"  # as background, for text
  accent     = "#ffd866"  # the focused pane's border and title
  selection  = "#5b595c"  # the selected row's background; without one, the row is reversed
  error      = "#ff6188"
  input      = "#fc9867"  # ⚠ needs input
  busy       = "#a9dc76"  # ● busy
  tag        = "#ab9df2"  # the `repo` tag
  key        = "#78dce8"  # keys in the hint bar
  ```
  `T` writes the theme it chooses in the top-level `theme = …` line, else in place of the template's `# theme = …`, else in a new first line; only lines before the first table count, and every other line and comment stays as written.
  Colours are `#rrggbb`, ANSI names (`cyan`, `light-red`), indexes `0`–`255` or `reset` (the terminal's default). A key left out takes `terminal`'s value; unknown keys are errors with their line. The built-in themes are files in the repository's `themes/`, compiled in: a theme is contributed by adding one there. Yours go in `themes/` next to `config.toml`, a file with a built-in's name replacing it. An unknown theme, or a theme file in error, is a config error.
- `path` and `worktree_dir` accept `~/`; any other path must be absolute. A project whose file does not parse, or whose `path` is missing, invalid or another file's, is shown with its error, and the other projects still load.
- `base` is optional. The new-worktree form's Base follows the selected row: on a worktree, that worktree's branch; on the project (or a detached worktree), `base`, else the remote's default branch as `origin/main` (what `origin/HEAD` points to; tatou never fetches), else the branch checked out in the main worktree.

## 10. Implementation notes

- Single Rust crate. Dependencies: `ratatui`, `crossterm`, `portable-pty`, `tui-term` (and its `vt100`, used through its re-export), `serde`, `toml`, `thiserror`, `tracing` + `tracing-subscriber` (logging to `$XDG_STATE_HOME/tatou/tatou.log`, never to the terminal; rotated to `tatou.log.old` at startup once past 1 MiB; level from `$TATOU_LOG`, default `info`; `--help` and `--version` do not log). The daemon logs to the same file.
- Paths follow XDG on every platform (`~/.config/tatou`, `~/.local/state/tatou`), honouring `$XDG_*` variables when set. No `~/Library/...` on macOS: CLI users expect `~/.config`.
- All platform differences live in one `platform` module behind one function, `notify(title, body)`, using `cfg(target_os)`. Nothing else in the codebase is platform-specific; `portable-pty` hides the pseudo-terminals'.
- Shell snippets and command templates must be POSIX `sh` and must not assume GNU userland (macOS ships BSD tools).
- Plain `std::thread` + `mpsc` channels for background work: per connection and per terminal in the daemon, per request and for the shown terminal's stream in the dashboard; no async runtime.
- External commands go through `exec.rs`; the programs in terminals are the exception, started by the daemon through `portable-pty`.
- Commands' stdout and stderr are read as UTF-8, lossily: file names that are not valid UTF-8 are not supported. Commands have no timeout; they run on background threads with stdin closed and git/gh prompts disabled, so a stuck command only stalls its own refresh.
- Modules: `error`, `logging`, `config`, `theme`, `exec`, `launcher`, `daemon`, `protocol`, `client`, `term`, `platform`, `git`, `gh`, `model`, `refresh`, `actions`, `keys`, `ui`.
- Parsers for git and gh output are pure and tested against captured fixtures; the protocol is tested by round trips, the daemon end to end on a temporary socket.
- Runtime requirements: `git`. Optional: `gh`, `notify-send` (Linux; macOS uses the built-in `osascript`).
- Versioning: every build is an alpha until a stable v1. The `VERSION` file holds the version as its git tag (`v0.1.0-alpha.N`), mirrored in `Cargo.toml`; each task's pull request bumps `N`. Merging into `main` makes CI tag that version and publish it as a GitHub pre-release with GoReleaser (Rust builder). Tags are never created by hand.

## 11. Milestones

1. **Navigate:** config, launcher, list projects/worktrees from git, sessions and status, show a worktree's agent with Enter. (Built on tmux, which 2.19 replaced.)
2. **Manage:** new/remove worktree, start/restart agent, open shell, edit; native terminals in a daemon (2.19).
3. **Status:** hooks reporting to the daemon, `tatou hooks`, notifications.
4. ~~**Files**~~: dropped (2.18, #103). Editing is `e`'s, with `$EDITOR`; mouse support moved to milestone 6.
5. **PRs:** badges and open PR.
6. **Keys:** configurable keymap, custom shell-command actions, `?` help overlay, mouse support.
7. **Ship:** `cargo install`; Linux: AUR, `.deb`/`.rpm`; macOS: Homebrew tap, Apple silicon only (`darwin-arm64`; Intel Macs are not supported). All packaging goes through GoReleaser. From milestone 1, CI tests on Linux and macOS and every merged task is released as an alpha (static `linux-amd64` and `linux-arm64` binaries, a `darwin-arm64` binary).

Milestone 2 is already a useful tool. `MILESTONE.md` breaks these milestones into steps, one GitHub issue each.

## 12. Open questions

- ~~tmux, or tatou's own terminals~~ — decided in 2.19: tatou's own terminals, in its daemon (§8.3).
- Scrollback: the screens keep it, but nothing browses it yet.
- The kitty keyboard protocol inside the shown terminal: tatou does not answer programs asking for it, so they fall back to legacy keys (Shift-Enter arrives as Alt-Enter).
- ~~Mouse support and text selection~~ — decided in 6.6: the mouse is taken only when needed (§6).
