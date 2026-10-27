# tatou

A minimal terminal UI for running coding agents (Claude Code, Codex, Gemini CLI, …) in parallel across several repositories, one git worktree per task. tatou lists your projects and their worktrees with each one's activity, opens a shell or your editor in a worktree, where you start any agent you like, shows the terminal you pick in a pane beside them, and keeps every terminal running in a small daemon of its own when you quit. See [DESIGN.md](DESIGN.md) for the full picture.

**Status: early alpha.** tatou lists projects, worktrees and sessions, creates and removes worktrees, opens shells and your editor in them, and shows them in the dashboard. It starts no agent itself: you run yours in a shell. With [gh](https://cli.github.com) logged in, each worktree shows its branch's pull request and checks. Built-in agent support (finding the agents installed and setting them up) and notifications come next ([MILESTONE.md](MILESTONE.md)).

## Requirements

- Linux, or macOS on Apple silicon
- [git](https://git-scm.com)
- Optional: [gh](https://cli.github.com), logged in (`gh auth login`), for pull request badges
- To build it: [Rust](https://rustup.rs) 1.98 or later

## Install

```sh
git clone https://github.com/raqqun/tatou
cd tatou
cargo install --path .
```

This puts `tatou` in `~/.cargo/bin`; make sure it is in your `$PATH`. Prebuilt binaries of each alpha (`linux-amd64`, `linux-arm64`, `darwin-arm64`) are on the [releases page](https://github.com/raqqun/tatou/releases).

## Add your projects

A project is a git repository. Any of these adds one:

- `tatou add` inside the repository (any folder of it, or one of its worktrees), or `tatou add ~/code/my-app`.
- Run `tatou` inside a repository that is not a project yet: it asks `Add ~/code/my-app to tatou? [Y/n]`.
- Write a file yourself, `~/.config/tatou/projects/my-app.toml`:

  ```toml
  path = "~/code/my-app"
  ```

Each project is one file in `~/.config/tatou/projects/`; delete the file to remove the project. Settings live in `~/.config/tatou/config.toml`, written with every option commented out on the first run (`$XDG_CONFIG_HOME` is honoured, on macOS too). A running dashboard picks up changes to both within a few seconds.

## Run it

Open a terminal tab of its own and type:

```sh
tatou
```

The dashboard opens in that terminal: Projects and Sessions on the left, the shown shell or editor on the right. tatou starts its daemon the first time; it keeps every terminal running when you quit the dashboard, and `tatou` shows them again, from this terminal or another.

In the dashboard:

| Key | Does |
|---|---|
| `j` / `k`, `↓` / `↑` | move |
| `h` / `l`, `←` / `→` | fold / unfold a project |
| `Tab` | Projects ↔ Sessions |
| `Enter` | show the selected worktree's terminal, or session, or window, on the right (on a project: fold / unfold) |
| `n` | new worktree, with a shell in it |
| `t` | open a shell in the selected worktree |
| `p` | open the selected worktree's pull request in the browser (with `gh`) |
| `e` | open `$EDITOR` in the selected worktree (on a project: its repository) |
| `d` | in Projects, remove the selected worktree (its branch stays); in Sessions, end the selected window, or session with all its windows |
| `q` | quit the dashboard; shells, editors and what runs in them keep running |

What you open shows on the right and takes the keys: everything you type goes to it, but these, which work from anywhere:

| Key | Does |
|---|---|
| `Alt-d` | back to the dashboard |
| `Alt-l` / `Alt-h` | next / previous session |
| `Alt-j` / `Alt-k` | next / previous window of the shown session |

On macOS, the terminal must send Option as Meta for these (Terminal: *Settings → Profiles → Keyboard → Use Option as Meta key*; iTerm2: *Profiles → Keys → Left Option key → Esc+*). Shift-Enter reaches agents as a new line in terminals that support the kitty keyboard protocol (kitty, Ghostty, WezTerm, iTerm2); elsewhere, use Alt-Enter. When the shown program ends, the next window or session shows. Text is selected with the mouse as in any terminal.

Each worktree shows its session's activity: a dim `●` means output in any of its terminals in the last 10 seconds, a dim `○` none, `·` no session. Busy, needs-input and idle (`●`, `⚠`, `○`) come with agent support ([DESIGN.md §8.4](DESIGN.md#84-status)).

With `gh` installed and logged in, a worktree whose branch has a pull request shows it on the right, refreshed every minute: `#42 ✓` while open, its checks `✓` passed, `●` running, `✗` failed; `#45 draft ✓`, `#40 merged` and `#41 closed` dim.

## Troubleshooting

- tatou and its daemon log to `~/.local/state/tatou/tatou.log`; `TATOU_LOG=debug tatou` logs every command it runs.
- `tatou stop` ends every terminal, and the daemon.

## Uninstall

`tatou stop`, `cargo uninstall tatou`, then remove `~/.config/tatou` and `~/.local/state/tatou`. Your repositories and worktrees are left as they are.

## License

[GPL-3.0](LICENSE)
