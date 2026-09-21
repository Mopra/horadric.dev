# Implementation plan

Where Glance is, what comes next, and what was decided along the way. Update
this file when a step lands or a decision changes. It is the handover
document: someone picking the project up cold should need nothing else.

Last updated 2026-09-21, after step 2.

## Shape of the thing

Four crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `glance-core` | Session, Phase, Registry, the state machine | any |
| `glance-hooks` | The localhost listener and the settings installer | any |
| `glance-ui` | Cluster windows, layout, drawing | Windows |
| `glance` | The command line and the wiring | Windows |

`glance-core` and the pure halves of `glance-ui` (`layout`, `theme`) have no
I/O and are tested. Everything else is verified on screen.

Threads: the listener thread owns the socket, a feeder thread applies events
to the registry and posts a thread message, the UI thread owns every window
and never blocks. The registry is an `Arc<Mutex<Registry>>` shared between
the feeder and the UI.

## Done

### Step 1: hook receiver and state machine

Claude Code `http` hooks post every lifecycle event to
`127.0.0.1:43117/glance/hook`. The hook config carries `X-Glance-Session:
$GLANCE_SESSION`, so an event arrives already tagged with the session it
belongs to. A `claude` started outside Glance sends an empty header and the
event is dropped, which is what keeps Glance from instrumenting the whole
machine. Superset got this wrong and wrote it up in their
`HOOKS_INVESTIGATION.md`; the guard is the lesson.

The state machine is in `glance-core/src/session.rs`. Prompt and tool events
mean working, permission and notification events mean waiting, `Stop` means
done, `SessionEnd` means ended. Subagent events and compaction restarts
change nothing. `glance run` posts a `GlanceRegister` event of its own before
starting Claude, because Claude Code sends no hook until the first prompt and
a fresh session would otherwise be invisible.

### Step 2: cluster windows

One frameless window per project, stacked down the right edge of the work
area, drawn with Direct2D and DirectWrite. The Win32 rules that make it feel
native rather than like an app window:

- `WS_EX_NOACTIVATE` so clicking a tile never takes focus from the editor.
- `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar.
- `SWP_NOACTIVATE` on every move, resize and raise.
- Dragging handled by hand, because the system move loop activates.
- DWM rounded corners and a suppressed border, so it matches Windows 11.

Measured: one process, about 45 MB with two clusters and five tiles, no CPU
between events.

## Next

### Step 3: the terminal window

The hard one. Clicking a tile expands it into a real terminal running the
real `claude`, with permissions, slash commands and everything else intact.

- ConPTY through `portable-pty`, or the `windows` crate directly if the
  dependency is not earning its place.
- `alacritty_terminal` for VT parsing and the grid. Do not write a parser.
- A second window class, `GlanceTerminal`. Unlike a cluster this one **does**
  take focus, because you type into it. Normal window, resizable, in the
  taskbar.
- Our own glyph renderer on DirectWrite: one glyph run per row, a cached text
  format, monospace metrics from the font.
- Collapse destroys the renderer and the window. The PTY keeps running.

Watch out for: IME and dead keys (Danish keyboard), clipboard paste of
multiple lines, resize mapping to `SetConsoleScreenBufferSize`, and scrollback
memory across forty sessions.

This step changes what `glance run` means. Today it spawns Claude with
inherited stdio in whatever terminal you called it from. Once Glance owns
PTYs, the normal way to start a session is from Glance itself, and `glance
run` stays as the way to tag a session in your own terminal.

### Step 4: worktrees and the git glance

- `git worktree add` per session, branch named from the session name.
- `.glance/config.json` per repo with `setup` commands, copied into each new
  worktree because gitignored files do not come along. Superset learned this
  the hard way; their worktrees break without it. Allocate a port range per
  worktree too, so dev servers do not collide.
- `git diff --numstat` for the changed file list with plus and minus counts,
  uncommitted versus committed.
- A button that opens the worktree in VS Code (`code <path>`).
- **The project key has to change.** It is the working directory today, so
  every worktree would become its own cluster. It must resolve to the parent
  repository, via `git rev-parse --git-common-dir`.
- Worktrees must be optional per project. A session that wants the shared
  working tree, or is not in a repo at all, has to keep working.

### Step 5: inbox, installer, updater

- The inbox is the sessions waiting on you, oldest first, with what each is
  waiting for. Open question whether it is a window or just sort order inside
  the clusters. Decide when forty sessions is real, not before.
- NSIS installer and a signed updater, both liftable from Purrch
  (`../purrch.fun/src-tauri`).
- A tray icon. Right now there is no way to quit except killing the process,
  and no way to get the windows back if you close them.

## Not in any step yet, but needed before daily use

- **Cluster positions are not remembered.** Restart and everything goes back
  to the right edge. Needs a settings file, probably
  `%APPDATA%\Glance\state.json`.
- **Only the primary monitor.** `arrange` reads `SPI_GETWORKAREA`, which
  ignores the other screens.
- **No way to quit.** See the tray icon above.
- **Clicking a tile does nothing.** It is wired to a hit test that discards
  the result, waiting for step 3.
- **No remote.** The repository exists on one disk. Push it somewhere.

## Open questions

Carried from the concept, with what is known now.

- **Does forty sessions hold up?** Five tiles cost 45 MB and no CPU. The
  renderer is one process, so the ceiling is likely the PTYs and the
  scrollback, not the tiles. Unknown until step 3.
- **Can a tile light up without stealing focus?** Yes, so far.
  `WS_EX_NOACTIVATE` plus `SWP_NOACTIVATE` holds through raising.
- **How does a session get named?** `--name` today, folder name as the
  fallback. Naming from the first prompt is still open.
- **What about the VS Code extension's Claude?** Still unanswered. Probably
  the answer is to stop using it.

## Name collision

`glanceapp/glance` is a self-hosted dashboard with more than twenty thousand
stars on GitHub. The crate name, the binary name and any published package
need a decision before this goes public. Undecided.
