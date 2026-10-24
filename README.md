# tatou

A minimal terminal UI for running coding agents (Claude Code, Codex, Gemini CLI, …) in parallel across several repositories, one git worktree per task. tatou lists your projects and their worktrees with each agent's status, and jumps to an agent's tmux session with one key. It never shows agent output itself: the agents run in plain tmux sessions on tatou's own tmux server, and everything tatou creates works without it. See [DESIGN.md](DESIGN.md) for the full picture.

**Status: early alpha.** Today tatou lists projects, worktrees and sessions, and jumps between them. Creating worktrees and starting agents from tatou come next ([MILESTONE.md](MILESTONE.md)); until then, start them by hand as shown below.

## Requirements

- Linux, or macOS on Apple silicon
- [git](https://git-scm.com) and [tmux](https://github.com/tmux/tmux) 3.2 or later
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

This starts tatou's tmux server and attaches to its dashboard. Running `tatou` again from another terminal re-attaches. Started from inside your own tmux, it warns first: tatou's tmux would run nested inside yours.

In the dashboard:

| Key | Does |
|---|---|
| `j` / `k`, `↓` / `↑` | move |
| `h` / `l`, `←` / `→` | fold / unfold a project |
| `Enter` | jump to the selected worktree's session (on a project: fold / unfold) |
| `q` | leave tatou: back to your terminal; agents and shells keep running, `tatou` comes back to them |

Each worktree shows its agent's status: `●` busy, `⚠` needs input, `○` idle, `·` no session. Without the agent hooks from [DESIGN.md §8.4](DESIGN.md#84-agent-status), a dim `●` means output in the last 10 seconds and a dim `○` none.

**Back to the dashboard:** `Alt-d` from any session on tatou's server (it starts the dashboard again if it was closed), or leave a window's agent or shell that tatou started: when it exits, you are back on the dashboard. On macOS, the terminal must send Option as Meta (Terminal: *Settings → Profiles → Keyboard → Use Option as Meta key*; iTerm2: *Profiles → Keys → Left Option key → Esc+*). The key can be changed with `back` under `[tmux]` in `config.toml`; run `tatou` again to apply it.

## Start an agent by hand (for now)

Until tatou starts agents itself, create a worktree and start a session in it on tatou's server, from any terminal while tatou runs, then jump to it with `Enter`:

```sh
git -C ~/code/my-app worktree add -b fix-login ../my-app.worktrees/fix-login
tmux -L tatou new-session -d -s my-app-fix-login -c ~/code/my-app.worktrees/fix-login -e TATOU=1 claude
```

tatou matches sessions to worktrees by their directory, so the session's name is up to you. `TATOU=1` lets the agent hooks report its status; without it they do nothing.

## Troubleshooting

- tatou logs to `~/.local/state/tatou/tatou.log`; `TATOU_LOG=debug tatou` logs every command it runs.
- `tmux -L tatou attach` reaches tatou's sessions without tatou; `tmux -L tatou kill-server` stops them all.

## Uninstall

`cargo uninstall tatou`, then remove `~/.config/tatou` and `~/.local/state/tatou`. Your repositories and worktrees are left as they are.

## License

[GPL-3.0](LICENSE)
