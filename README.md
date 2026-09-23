# Glance

Every coding agent session you have running becomes a small tile on your
Windows desktop, grouped by project, that lights up when it needs you and
grows into a full terminal when you click it. Each session works in its own
git worktree, and the tile shows you what it changed.

Glance is a window manager for agent sessions that already exist. It never
puts its own chat UI in front of the agent. The terminal is the UI.

Status: early. The tiles, the state stream behind them and the terminals
work. Worktrees are next.

## What works now

- `glance` opens one cluster window per project on the right edge of your
  screen: frameless, rounded, always on top, never takes focus, not in the
  taskbar or alt-tab. Drag it anywhere. Click the header to collapse it.
- Each tile shows a session: state dot, name, an age line like
  "needs permission 12 min", and the last line worth reading. Waiting tiles
  light up amber.
- `glance new --name fix-login` starts `claude` in a Glance terminal: a real
  terminal window with the real CLI in it, permissions, slash commands and
  all. The `+` in a cluster header starts one in that project. Click the
  tile to bring the terminal forward, close the window to collapse it back
  into the tile. The session keeps running either way.
- `glance hooks install` adds Claude Code hooks to `~/.claude/settings.json`.
  They are `http` hooks: Claude Code posts each lifecycle event to Glance on
  localhost. No script runs, no process is spawned per event.
- `glance run --name fix-login` starts a tagged `claude` in the terminal you
  are in instead. Its tile shows state but can not expand. A `claude`
  started any other way is ignored.
- `glance serve` shows the same state as a table in the terminal.

The whole app is one process. With two projects and four sessions on screen
it uses about 45 MB and no CPU between events. The terminals are ConPTY,
`alacritty_terminal` for the grid, and our own DirectWrite glyph renderer.

## How it knows

Glance sets `GLANCE_SESSION` in the environment of every `claude` it starts.
The hook is configured to send that variable as a header, so every event
arrives already tagged with the tile it belongs to. Outside Glance the header
is empty and the event is dropped.

State comes from hook events, never from reading terminal output:

| Event | State |
|---|---|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | working |
| `PermissionRequest`, `Notification` (permission, idle, dialog) | waiting |
| `StopFailure` | waiting, with the error |
| `Stop` | done |
| `SessionEnd` | ended |

Subagent events and compaction restarts do not change state.

## Build

Rust stable on Windows.

```
cargo build --release
target\release\glance.exe hooks install
target\release\glance.exe
```

In another terminal, inside a project:

```
glance new --name what-this-session-does
```

`GLANCE_AGENT=cmd.exe` runs a shell instead of `claude` in the terminals,
which is the cheap way to try them.

`glance hooks uninstall` removes the hooks again and leaves everything else in
your settings untouched.

## Plan

1. Hook receiver and state machine. Done.
2. Cluster windows on Win32 and Direct2D. Done.
3. Terminal window: ConPTY, the alacritty grid, our own glyph renderer. Done.
4. Worktree per session, changed files, open in VS Code.
5. Inbox, installer, updater.

Pure Rust, Win32 and Direct2D directly. No web view, no toolkit.
[docs/PLAN.md](docs/PLAN.md) has the detail, the open questions and the
known gaps.

## Not doing

- A chat UI of its own.
- Progress bars or time estimates.
- An embedded editor or diff viewer. VS Code does that.
- Merging worktrees back. Git and the human do that.
- Mac or Linux, for now.

## License

MIT.
