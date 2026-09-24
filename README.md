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

- `glance` opens one cluster window per project on the left edge of your
  screen: frameless, rounded, never takes focus, not in the taskbar or
  alt-tab. It stacks like a normal window: other windows can cover it, a
  click brings it forward, and "Bring tiles to front" in the tray menu
  brings them all back. Drag it anywhere: it snaps to the screen edges and to
  the other Glance windows, or hold Shift to place it freely. Click the header to
  collapse it. "Tidy up tiles" in the tray menu stacks them all back on the
  left edge.
- Each tile shows a session: an icon for its state, name, an age line like
  "needs permission 12 min", and the last line worth reading. Waiting tiles
  light up amber.
- A project in git gets a files tile at the bottom of its cluster: the file
  tree as VS Code's explorer shows it, changed files coloured with their
  letter (M, A, U, D), folders marked when something inside changed. It
  updates as files change, without polling. Click a folder to open it, a
  file to open it in VS Code, the header to fold it.
- A tray icon starts sessions: "New session..." asks for a folder, and your
  recent projects are one click away. It also quits Glance.
- `glance explorer install` adds "Open in Glance" to folders in Explorer,
  which starts Glance too if it is not running.
- A session runs `claude` in a Glance terminal: a real terminal with the
  real CLI in it, permissions, slash commands and all. The sessions share one
  terminal window, the stage, which shows one project at a time: every
  session of that project is a pane in a grid. One fills the window, two
  sit side by side, four are two by two. Click a tile and the stage switches
  to its project with that session typing; click it again or close the
  window to collapse it. Other projects keep running unseen, and the tiles
  on stage are lit. The `+` in a cluster header starts another, and
  `glance new --name fix-login` does it from a script.
- Drag a pane by its header onto another to swap the two. The order is
  remembered. "Fit terminal beside tiles" in the tray menu fills the space
  right of the clusters. The terminal snaps like clusters do, when moved and
  when an edge is dragged to resize, and Shift again places it freely.
- Plain terminals for everything that is not the agent: a dev server, a
  log, a deploy. The small button beside a cluster's bottom `+`, Ctrl+Shift+T
  in any pane, or "New terminal" in the project menu opens PowerShell in the
  project, as a pane on the stage and a tile in the cluster. The tile shows
  what the terminal's title says and how busy its output is. `exit` closes
  it. `GLANCE_SHELL` picks another shell.
- Ctrl+Alt+Space, from anywhere, shows the session that has waited on you
  longest. Press it again to move on to the next one.
- A Claude window at the top of the stack shows how much of your five hour,
  weekly and spend limits is used and when each resets, and picks the
  model, effort and permission mode for every session Glance starts or
  resumes. Each tile shows how full its session's context is. The numbers
  come from Claude Code's status line, which Glance sets for its own
  sessions only.
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
target\release\glance.exe install
```

That installs Glance for your user, no admin rights: it is in the Start
menu (search "Glance"), `glance` works in any new terminal, "Open in Glance"
is on folders in Explorer (under "Show more options" on Windows 11), it
starts with Windows, and the Claude Code hooks are in place. Glance lives in
the tray; Windows may hide a new icon behind the `^` by the clock.

Sessions survive a quit or a restart: they come back as paused tiles, and a
click resumes the conversation.

`GLANCE_AGENT=cmd.exe` runs a shell instead of `claude` in the terminals,
which is the cheap way to try them.

`GLANCE_DEV=1` runs a build beside the installed Glance without touching
it: its own port and saved state, no autostart, a red tray icon. That is how
Glance is developed from a session inside Glance.

`target\release\glance.exe reload` updates a running Glance to a new build
without the quit: once no session is mid turn it hands over, and the
sessions that were running resume by themselves. A build that fails to
start is rolled back.

`glance uninstall` takes it all back out. The hooks it removes are only
Glance's; everything else in your Claude Code settings stays as it was.

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
