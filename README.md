# Glance

Every coding agent session you have running becomes a small tile on your
Windows desktop, grouped by project, that lights up when it needs you and
grows into a full terminal when you click it. Each session works in its own
git worktree, and the tile shows you what it changed.

Glance is a window manager for agent sessions that already exist. It never
puts its own chat UI in front of the agent. The terminal is the UI.

Status: early. Today Glance is a console tool that proves the state stream.
The tiles come next.

## What works now

- `glance hooks install` adds Claude Code hooks to `~/.claude/settings.json`.
  They are `http` hooks: Claude Code posts each lifecycle event to Glance on
  localhost. No script runs, no process is spawned per event.
- `glance serve` listens for those events and shows a live table: session,
  state (working, waiting, done), how long it has been in that state, and the
  last line worth reading.
- `glance run` starts a `claude` tagged so `serve` can see it. A `claude`
  started anywhere else is ignored.

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
target\release\glance.exe serve
```

In another terminal, inside a project:

```
glance run
```

`glance hooks uninstall` removes the hooks again and leaves everything else in
your settings untouched.

## Plan

1. Hook receiver and state machine. Done.
2. Cluster window: one frameless always-on-top window per project, tiles
   inside it, no focus stealing when a tile lights up.
3. Terminal window: ConPTY on the Rust side, our own renderer on DirectWrite,
   `alacritty_terminal` for the VT grid.
4. Worktree per session, changed files with plus and minus counts, open in
   VS Code.
5. Inbox, installer, updater.

Pure Rust, Win32 and Direct2D directly. No web view, no toolkit.

## Not doing

- A chat UI of its own.
- Progress bars or time estimates.
- An embedded editor or diff viewer. VS Code does that.
- Merging worktrees back. Git and the human do that.
- Mac or Linux, for now.

## License

MIT.
