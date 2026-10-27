# tatou — Milestones

The order of work, from an empty repository to the first complete version of tatou. `DESIGN.md` says what tatou does and why; this file says in which order, and records each step. How to work on the code (definition of done, versioning, commits) is in `CLAUDE.md`.

Each step is one GitHub issue (in the **tatou** GitHub project, grouped by GitHub milestone) and one pull request. A finished step is one line here: its spec and the decisions taken are in its issue and pull request, and the design as it stands is in `DESIGN.md`. An open step keeps its spec, listed in dependency order; "Depends on" names the steps to merge first.

**Checkpoints:** after M1 tatou is dogfoodable (navigate and jump). After M2 it is already a useful tool. After M7 it is feature-complete and packaged, still an alpha; v1 is a separate decision.

---

## M1 — Navigate

Config, launcher, projects and worktrees from git, sessions and their status, `Enter` to show a worktree's session. Built on tmux, which 2.19 replaced. Done.

- 1.1 Scaffold the crate (#1)
- 1.2 CI and alpha releases with GoReleaser (#2)
- 1.3 Errors, XDG paths and file logging (#3)
- 1.4 `exec.rs`: the only way to run commands (#4)
- 1.5 `config.rs`: load `config.toml` (#5)
- 1.6 Generate tatou's `tmux.conf` (#6) — tmux; replaced by 2.19
- 1.7 `tmux.rs`: client and version check (#7) — tmux; replaced by 2.19
- 1.8 Parse tmux sessions (#8) — tmux; replaced by 2.19
- 1.9 Parse `git worktree list --porcelain` (#9)
- 1.10 Command-line entry points (#10)
- 1.11 Launcher (#11)
- 1.12 `model.rs`: merge config, git and tmux (#12) — on tmux then; the daemon since 2.19
- 1.13 Background refresh (#13)
- 1.14 UI shell and keymap skeleton (#14)
- 1.15 Left pane: projects and worktrees (#15)
- 1.16 Jump to a session (#16)
- 1.17 Reload `config.toml` in the running dashboard (#67)
- 1.18 `tatou add [path]` (#68)
- 1.19 Offer to add the current repository at launch (#69)
- 1.20 Minimal README for dogfooding (#17)

---

## M2 — Manage

New and remove worktrees, shells and the editor in them, native terminals in a daemon (2.19), terminals only (2.20). After it, tatou is a useful tool. Done.

- 2.1 Git: add and remove worktrees (#18)
- 2.2 tmux: create, restart and kill sessions (#19) — tmux; replaced by 2.19
- 2.3 UI dialogs (#20)
- 2.4 New worktree (`n`) (#21)
- 2.5 Start / restart agent (`s`) (#22) — removed in 2.20
- 2.6 Open a shell in the worktree (`t`) (#23)
- 2.7 Remove worktree (`d`) (#24)
- 2.8 Popup mode (#25) — dropped in 2.19, not planned
- 2.9 Sessions pane below Projects (#85)
- 2.11 Drop the built-in `shell` agent (#87) — removed in 2.20
- 2.12 New worktree base from the selected row (#88)
- 2.13 Sessions pane shows the program in each session's current window (#93)
- 2.14 Show which pane has the focus (#95)
- 2.15 Sessions pane as a tree of sessions and their windows (#97)
- 2.16 Back to the dashboard when a window's program exits (#99) — on tmux; the daemon does it since 2.19
- 2.17 Name windows in the status bar after their session (#101) — tmux; replaced by 2.19
- 2.18 Drop the Files pane; `e` edits the worktree (#103)
- 2.19 Native terminals in a daemon, without tmux (#109)
- 2.20 Drop agents: tatou starts terminals only (#127)

---

## M3 — Agents

Support for coding agents, one module per agent (Claude Code, Codex, …), replacing the generic `[agents]` dropped in 2.20 (#127). Each module knows its agent: how to find it on the machine, how to start and resume it, and how it reports its state. Steps 3.1–3.3 are outlines, to design and open as issues when M3 starts.

Dropped:

- `tatou status` and the tmux status line (#30, was 3.5) — dropped in 2.19, not planned

### 3.1 Agent modules and detection

Depends on: 2.20

- One module per agent, behind one small shape they share (a trait, once there are two of them): its name, the program to look for, its command, its resume command (DESIGN §12.2).
- Detection: which agents are installed, found on `$PATH` (through `exec.rs`), at startup and on demand; nothing written down.
- Decisions to make: where the modules live (`src/agents/`); what the user may override in `config.toml` (a command, a flag, turning one off).

### 3.2 Setting an agent up for tatou

Depends on: 3.1

- What an agent needs from tatou: its hooks (3.4), any other setting.
- Decision to make: applying it automatically or by asking the user. Writing an agent's own configuration conflicts with DESIGN §1 and §3 (tatou never modifies files it does not own; it prints the snippet instead), so automatic setup needs that rule changed first, or setup stays a snippet the user applies, offered when an agent is found without it.

### 3.3 Starting an agent

Depends on: 3.1

- Starting a detected agent in a worktree's session, and in a new worktree: the key, the choice when several are installed, restarting and resuming.

### 3.4 `tatou hooks` (#26)

Depends on: 3.2

- `tatou mark busy|input|idle`: tells the daemon, over its socket, that the terminal it runs in (`$TATOU_TERMINAL`, which the daemon sets for each program, with `$TATOU_SOCKET`) is in that state; a no-op, quiet, without them. The daemon keeps the state with the terminal and lists it; the model shows it before activity.
- Each agent's module gives its hooks: the Claude Code hooks JSON from DESIGN §8.4, Codex's and Gemini CLI's where their hook mechanism is known; set up as 3.2 decides.
- Check whether `input` needs clearing after the user answers a permission prompt (e.g. a `PreToolUse`/`PostToolUse` hook setting `busy`), and include it if so.

**Done when**
- A test checks that the Claude section is valid JSON (with `serde_json`, a dependency since 5.1). Snippets are POSIX `sh`.
- Daemon test: a program running `tatou mark input` in its terminal shows as input in the list.

### 3.5 Verify hook-driven status end to end (#27)

Depends on: 3.4, 1.12

- With the hooks set up, run a Claude Code session in a tatou worktree and confirm the icon cycles busy → input → idle within ~1 s.
- Confirm sessions without hooks fall back to active/quiet and that `TATOU` unset makes the hooks a no-op outside tatou.

**Done when**
- Findings recorded in the PR; DESIGN §8.4 corrected if the hooks needed changes.

### 3.6 `platform.rs`: notify (#28)

Depends on: 1.4

- `notify(title, body)`: `notify-send` on Linux; on macOS `osascript` with the text passed as `argv` (`on run argv … display notification (item 2 of argv) with title (item 1 of argv)`), never interpolated into the script. Missing tool → silently skipped, logged.
- All `cfg(target_os)` lives here and nowhere else.

**Done when**
- Unit tests on the built argument vectors (title/body with quotes and backslashes); no test actually shows a notification.

### 3.7 Notify when an agent needs input (#29)

Depends on: 3.6, 3.4

- The dashboard compares statuses between refreshes (in memory only) and, on a transition to `input`, notifies unless that terminal is the one it shows.
- Also a message in the hint bar with the worktree's name.

**Done when**
- Unit test for transition detection (input from busy/idle/none, no repeat while staying in input, suppressed when viewed).

---

## M4 — Files (dropped)

Dropped in 2.18 (#103): no file pane. Editing is `e`'s, with `$EDITOR` (2.18); reading and diffing files is the editor's job and the agent's. Its steps 4.1–4.6 (#31–#36) were closed as not planned; mouse support (4.7, #37) moved to M6 as 6.6.

---

## M5 — PRs

A PR and CI badge per worktree, and `p` to open the PR. Done.

- 5.1 `gh.rs`: parse PR list (#38)
- 5.2 Fetch PRs in the background (#39)
- 5.3 Show badges and open PR (`p`) (#40)

---

## M6 — Keys

Configurable keymap, custom shell-command actions, help overlay. Done meanwhile: mouse support, themes, selection and scrollback in the shown terminal.

Done:

- 6.3 Extra tmux bindings (`[keys.tmux]`) (#43) — dropped in 2.19, not planned
- 6.6 Mouse support (#37)
- 6.7 Themes: `theme = …` and theme files (#110)
- 6.8 Choose the theme from the dashboard (`T`) (#112)
- 6.9 Select text with the mouse in the shown terminal (#115)
- 6.10 Scrollback in the shown terminal (#117)

### 6.1 Configurable keymap (#41)

Depends on: 1.14, 1.5

- Key notation parser: single characters (`n`, `@`, `D`), `Enter`, `Esc`, `Tab`, `S-Tab`, `Space`, arrows, `F1`–`F12`, `C-x`, `M-x`.
- `[keys]` overrides defaults by action name; unknown action names and duplicate bindings are config errors with clear messages.

**Done when**
- Unit tests: every notation, override, unknown action, conflict.

### 6.2 Custom actions (`[keys.custom]`) (#42)

Depends on: 6.1

- Template substitution of `{path}` with a shell-quoted value; the command runs in a new terminal of the worktree's session (a new session when it has none), shown, as `t` does.
- On a project row, its main worktree.

**Done when**
- Unit tests for substitution and quoting, including values containing quotes and `{`.

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

---

## M7 — Ship

Documentation, packaging channels and the first complete alpha. Binaries and GitHub pre-releases already exist since 1.2; packaging is added to `.goreleaser.yaml`, not as new workflows or tools.

### 7.1 User documentation (#45)

Depends on: 6.4, 5.3, 3.7

- README: what and why, requirements (git, optional gh), install, quick start, full config reference, hooks setup (`tatou hooks`), keys (the dashboard's, and the terminal's Alt keys), the daemon and `tatou stop`, uninstall (nothing to clean except `~/.config/tatou` and `~/.local/state/tatou`).
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

- `nfpms` in `.goreleaser.yaml`: `deb` and `rpm`, depending on `git`, recommending `gh`.

**Done when**
- Packages from an alpha release install and run on a recent Debian/Ubuntu and Fedora container.

### 7.5 Homebrew tap (#49)

Depends on: 7.3

- A `raqqun/homebrew-tatou` tap repository, filled by GoReleaser (`homebrew_casks`, or `brews` if casks do not fit a CLI binary), with `git` as a dependency; a repository secret with a token that can push to the tap.
- **Decision to confirm:** GoReleaser skips publishing pre-releases to taps by default (`skip_upload: auto`); every build is an alpha until v1, so either publish alphas to the tap or leave the tap empty until v1.

**Done when**
- `brew install raqqun/tatou/tatou` works on an Apple silicon Mac.

### 7.6 AUR package (#50)

Depends on: 7.3

- `aurs` in `.goreleaser.yaml` publishing `tatou-bin` from the release archives, `depends=(git)`, `optdepends=(github-cli)`; an AUR SSH key as a repository secret.
- Same pre-release decision as 7.5.

**Done when**
- `makepkg -si` of the published `PKGBUILD` succeeds in an Arch container.

### 7.7 End-to-end check of the complete alpha (#51)

Depends on: 7.3, 7.4, 7.5, 7.6

- End-to-end manual test on Linux and macOS following the README, from every install channel: launch, create worktree, agent status with hooks, notification, custom actions, PR badge, remove worktree, uninstall.
- Issues found become new issues; the result is the alpha release produced by this step's PR. v1 is decided separately.

**Done when**
- The alpha released by this step installs and passes the check from every channel above.
