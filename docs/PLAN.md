# Implementation plan

Where Glance is, what comes next, and what was decided along the way. Update
this file when a step lands or a decision changes. It is the handover
document: someone picking the project up cold should need nothing else.

Last updated 2026-09-24, after step 3.

## Shape of the thing

Five crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `glance-core` | Session, Phase, Registry, the state machine | any |
| `glance-hooks` | The localhost listener, its client, the settings installer | any |
| `glance-pty` | Child processes in ConPTY pseudo consoles | Windows |
| `glance-ui` | Cluster and terminal windows, layout, drawing | Windows |
| `glance` | The command line and the wiring | Windows |

`glance-core` and the pure halves of `glance-ui` (`layout`, `theme`,
`palette`, `keys`, `frame`) have no I/O and are tested. `glance-pty` has a
test that runs `cmd.exe` in a real pseudo console. Everything else is
verified on screen.

Threads: the listener thread owns the socket, a feeder thread applies events
to the registry, the UI thread owns every window and never blocks. Each
console adds three: a reader that parses output into its grid, a waiter
that notices the exit, and a writer that owns the input pipe so typing never
blocks the UI. Everything off the UI thread reaches it as a message to one
hidden message-only window, never a thread message: thread messages are
dropped while Windows runs a modal loop, and dragging a terminal's edge is
one. The registry is an `Arc<Mutex<Registry>>`, each console's grid an
`Arc<Console>` with the terminal behind a mutex.

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

### Step 3: the terminal window

Clicking a tile expands it into a real terminal running the real `claude`.
Verified end to end against Claude Code 2.1 with Haiku: the trust dialog,
arrow keys, a prompt and its answer, resize, collapse and expand, `/exit`.

- **Starting a session.** `glance new [--name] [--cwd] [-- claude args]`
  posts to `/glance/new` on the running app, which starts the agent in a
  console and opens its terminal. The `+` in a cluster header does the same
  in that project. `glance run` is unchanged: a tagged `claude` in your own
  terminal, whose tile can not expand because Glance does not own it.
- **`/glance/new` is guarded.** A web page can reach localhost, so starting a
  process needs an `X-Glance-Command: new` header, which a browser can not
  send cross origin without a preflight we never answer, and any request
  with an `Origin` header is refused.
- **ConPTY on the `windows` crate directly**, in `glance-pty`, not
  `portable-pty`. Five calls did not justify its crates. The traps, both
  handled: the output pipe never reaches end of file on its own, so the
  waiter closes the console after the exit; and a parent with redirected
  stdio leaks those handles into the child unless `STARTF_USESTDHANDLES` is
  set with invalid handles. Resize is `ResizePseudoConsole`, not
  `SetConsoleScreenBufferSize`.
- **`alacritty_terminal` 0.26** with default features off. Only `Term` and
  the vte `Processor` are used, fed by our own reader thread; its event loop
  and tty module are compiled but unused. A synchronized update the program
  never ends is flushed when its 150 ms run out, from the paint path.
- **Glyph runs with forced advances.** Every glyph in a run gets the cell
  width as its advance, so text sits on the grid whatever the font says.
  Cell sizes are snapped to device pixels or neighbouring backgrounds leave
  seams. Characters the font lacks (Claude Code's `⏺`, `⎿`, `✻`), wide
  characters and combining sequences are drawn one at a time with
  DirectWrite fallback, pinned to their cells. Colour fonts only for wide
  characters, so a one cell symbol keeps the colour the program gave it.
  Cascadia Mono, falling back to Consolas, 14 DIPs.
- **Keyboard.** Characters come from `WM_CHAR` after the layout has done its
  work, so dead keys and AltGr on a Danish keyboard need nothing special.
  Keys without characters come from `WM_KEYDOWN` in xterm encoding.
  Shift+Enter sends Meta+Enter, Claude Code's newline. Ctrl+C copies when there is
  a selection. Ctrl+V pastes text with bracketed paste and escape characters
  stripped; with no text on the clipboard it is passed on so Claude Code can
  paste an image. Alt+F4 still closes.
- **Mouse.** Drag selects and copies on release, double click selects a
  word, right click copies a selection or pastes. The wheel scrolls
  history, or sends arrows to a full screen program.
- **Lifetime.** Closing the window collapses: window and renderer go, the
  console and its grid stay, and the window comes back where it was. A
  clean exit closes the window; a failed one keeps it open so the error can
  be read. An exit Claude Code could not report (a crash, a kill) becomes a
  `SessionEnd` of our own.
- **Environment.** The child gets `GLANCE_SESSION` and `COLORTERM`, and
  loses the variables Claude Code sets to name a parent session. When Glance
  was started from inside Claude Code those made the tile's agent believe it
  was nested, and it stopped saving its transcript.
- `GLANCE_AGENT` runs something other than `claude.exe` in a terminal.
  `GLANCE_AGENT=cmd.exe` is how to test the terminal without spending
  tokens.

Measured: one process, 51 MB (debug build) with a cluster and one open
terminal. History is 2000 rows per session, at about 24 bytes a cell: under
6 MB per session when full at 120 columns, 230 MB for forty full ones.

## Next

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

- **Sessions die with Glance.** The consoles live in the Glance process, so
  quitting or crashing it ends every agent in a terminal. Surviving that
  needs the consoles in a separate process, which is a real decision.
- **`claude.cmd` is not found.** Only `claude.exe` on `PATH` (the native
  installer) is looked for. An npm install needs `cmd.exe /c` and its own
  quoting rules.
- **Terminal gaps.** The IME composition window is not placed at the cursor.
  Mouse reporting to programs, the kitty keyboard protocol and cursor blink
  are not implemented. The font and its size are fixed.
- **Expanding from a synthetic click can open behind other windows.** Windows
  only lets a process take the foreground after real input. A real click on
  a tile is real input, so this only bites scripted tests.

- **Cluster positions are not remembered.** Restart and everything goes back
  to the right edge. Needs a settings file, probably
  `%APPDATA%\Glance\state.json`.
- **Only the primary monitor.** `arrange` reads `SPI_GETWORKAREA`, which
  ignores the other screens.
- **No way to quit.** See the tray icon above.
- **No remote.** The repository exists on one disk. Push it somewhere.

## Open questions

Carried from the concept, with what is known now.

- **Does forty sessions hold up?** Five tiles cost 45 MB and no CPU. One
  open terminal adds a few MB. History is bounded at 2000 rows a session, so
  the worst case for the grids is about 230 MB. The other cost is forty
  `claude` processes, which is not ours. Not yet tried with forty.
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
