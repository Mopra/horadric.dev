# Implementation plan

Where Glance is, what comes next, and what was decided along the way. Update
this file when a step lands or a decision changes. It is the handover
document: someone picking the project up cold should need nothing else.

Last updated 2026-09-24, after step 3, the launchers, persistence, install,
the stage, reload, the project grid, browser windows, the look and plain
terminals.

## Shape of the thing

Five crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `glance-core` | Session, Phase, Registry, the state machine | any |
| `glance-hooks` | The localhost listener, its client, the settings installer | any |
| `glance-pty` | Child processes in ConPTY pseudo consoles | Windows |
| `glance-ui` | Cluster and terminal windows, layout, drawing | Windows |
| `glance` | The command line, `glancew` for Explorer, the wiring | Windows |

`glance-core` and the pure halves of `glance-ui` (`layout`, `theme`,
`palette`, `keys`, `frame`, `files`, `viewer`, `highlight`, `shell`) have no I/O and
are tested. `glance-pty` has a
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

One frameless window per project, stacked down the left edge of the work
area, drawn with Direct2D and DirectWrite. The Win32 rules that make it feel
native rather than like an app window:

- `WS_EX_NOACTIVATE` so clicking a tile never takes focus from the editor.
- `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar.
- `SWP_NOACTIVATE` on every move, resize and raise.
- Dragging handled by hand, because the system move loop activates.
- A dragged cluster snaps to the work area edges and to the other Glance
  windows, at the same margin and gap `arrange` uses (`layout::snap`). Shift held
  places it freely. Not Alt: a lone Alt press opens the menu bar of the app
  with the focus. "Tidy up tiles" in the tray menu unpins them all.
- DWM rounded corners and a suppressed border, so it matches Windows 11.
- Not topmost. It stacks like a normal window, so an editor can cover it.
  A click raises it by going topmost and straight back, since a plain
  `HWND_TOP` from a background process stays below the foreground window.
  "Bring tiles to front" in the tray menu raises them all. It used to be
  topmost and raised on every phase change, which put it over everything.

Measured: one process, about 45 MB with two clusters and five tiles, no CPU
between events.

### Step 3: the terminal window

Clicking a tile expands it into a real terminal running the real `claude`.
Verified end to end against Claude Code 2.1 with Haiku: the trust dialog,
arrow keys, a prompt and its answer, resize, collapse and expand, `/exit`.

- **Starting a session.** `glance new [--name] [--cwd] [-- claude args]`
  posts to `/glance/new` on the running app, which starts the agent in a
  console and opens its terminal. `glance run` is unchanged: a tagged
  `claude` in your own terminal, whose tile can not expand because Glance
  does not own it. The friendlier ways in are under Launchers below.
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
  stripped. Alt+F4 still closes.
- **Images.** Passing Ctrl+V on for Claude Code to read the clipboard did
  not work, so Glance does it: a clipboard image is saved as a PNG in
  `%TEMP%\Glance` and its path is pasted, which Claude Code turns into an
  attachment. The clipboard's own PNG is used when a browser or the Snipping
  Tool offers one, otherwise the Windows Imaging Component encodes the
  bitmap. Text beats an image, because Excel puts a picture beside copied
  cells, unless the text is only the image's URL. Files copied in Explorer
  paste as their paths. Files dropped on the window paste as their paths,
  quoted when they hold a space, as in Windows Terminal. Pasted images are
  deleted after a day.
- **Mouse.** Drag selects and copies on release, double click selects a
  word, right click copies a selection or pastes. Dropped files paste as
  paths. The wheel scrolls history, or sends arrows to a full screen
  program.
- **Lifetime.** Closing the window collapses: window and renderer go, the
  console and its grid stay, and the window comes back where it was. A
  clean exit closes the window; a failed one keeps it open so the error can
  be read. An exit Claude Code could not report (a crash, a kill) becomes a
  `SessionEnd` of our own.
- **Environment.** The child gets `GLANCE_SESSION`, `GLANCE_OWNER_PORT` and `COLORTERM`, and
  loses the variables Claude Code sets to name a parent session. When Glance
  was started from inside Claude Code those made the tile's agent believe it
  was nested, and it stopped saving its transcript.
- `GLANCE_AGENT` runs something other than `claude.exe` in a terminal.
  `GLANCE_AGENT=cmd.exe` is how to test the terminal without spending
  tokens.

Measured: one process, 51 MB (debug build) with a cluster and one open
terminal. History is 2000 rows per session, at about 24 bytes a cell: under
6 MB per session when full at 120 columns, 230 MB for forty full ones.

### Launchers

Typing `glance new --cwd` was too much friction for starting a session in a
new project, so three ways in that need no terminal:

- **Tray icon.** Always there, even with no tiles. Its menu has "New
  session..." (the folder picker, opening in the last project), up to eight
  recent projects one click away, and Quit, which warns when sessions in
  Glance terminals would end. Recent projects live in
  `%APPDATA%\Glance\recent.json`. The tooltip counts sessions and waiting
  ones. The icon is drawn in code (`icon.rs`), so there is no file to ship.
  Pulled forward from step 5.
- **The full width `+` below a cluster's last tile** starts another session
  in that project at once, no picker. It is outlined, not filled, so it
  reads as the slot where the next tile goes rather than as a session.
- **The `+` in a cluster header** is for a new project. It opens the folder
  picker in the folder that holds this project, since projects tend to sit
  side by side. It used to open in the project itself, which made another
  session there the easy path; the bottom button is that path now.
- **Explorer.** `glance explorer install` adds "Open in Glance" when right
  clicking a folder or the empty space inside one, under
  `HKEY_CURRENT_USER`, so no admin rights. On Windows 11 it is under "Show
  more options"; the short menu only takes packaged apps. It runs
  `glancew.exe`, a second, windowless binary, so a right click never flashes
  a console. `glancew` starts the app when it is not running (hidden, no
  console window) and then asks it for the session. With no folder it only
  starts the app.

Two Win32 details that decided the shape. The app's hidden window is now a
top level tool window, not message-only, because only top level windows
hear Explorer's `TaskbarCreated` broadcast, after which the tray icon must be
added again. And menus and dialogs run modal loops that dispatch our own
messages, so they run with the app state not borrowed, from the window
procedure itself.

### Persistence

Glance picks up where it left off, after a quit, a crash or a restart. The
processes can not survive that, but the conversations can: Claude Code
resumes one with `claude --resume <id>`, and every hook carries the id.

- `%APPDATA%\Glance\state.json` (`glance_core::saved`) holds the sessions
  Glance owned, with name, folder, original arguments, Claude's session id,
  whether a prompt was ever sent, the last line and the window position;
  the cluster positions; the recent projects; whether autostart was offered.
  Written from the one second tick when it changed, beside and renamed.
- On start every saved session is a **paused** tile, a new phase. Clicking
  one runs `claude --resume <id>` with its original arguments in the same
  folder and window. Nothing starts until clicked, so forty saved sessions
  cost nothing at boot. A session never prompted starts fresh, since Claude
  wrote no transcript to resume.
- A clean exit (`/exit`) ends a session for good. Any other exit, a crash or
  a kill, pauses it instead. Right click a tile for End session.
- Right click a project's header or its bottom plus for the whole project:
  New session, Start 4 sessions, Start over with 4 sessions (the new ones
  start before the old ones end, so the cluster keeps its place), and End
  all sessions. The tray has End all sessions for every project. Ending
  asks first when a session is running and says how many are mid turn.
- On `WM_QUERYENDSESSION` and on Quit the state is written once more and
  then frozen, so sessions dying on the way out are not saved as gone.
- Claude's session id is the latest non empty one from any hook. Glance's
  own events carry none; the first version stored that empty id and never
  learned the real one.
- **The runaway, and the guard.** The first resume cleared the paused mark
  after launching, and launching opens the window through `expand`, which
  saw a paused tile and resumed again: 167 `claude` processes in a minute.
  Now the mark is cleared first, and `launch` refuses any session that
  already has a live console, whatever path led there.
- A second Glance refuses to start when the port answers. Autostart plus a
  manual start would otherwise show every saved session twice.

### Install

`glance install` makes Glance a normal per user app, no admin rights:

- Copies `glance.exe` and `glancew.exe` to `%LOCALAPPDATA%\Programs\Glance`.
- Adds that folder to the user `PATH` (registry, then `WM_SETTINGCHANGE`).
- A Start menu shortcut to `glancew.exe`, so Windows search finds Glance.
- Points Explorer's "Open in Glance" and Start with Windows at the copy.
- Installs the Claude Code hooks, then starts Glance.

`glance uninstall` reverses it and leaves the saved state. The executables
carry the icon and version information, baked in by `crates/glance/build.rs`,
which writes the `.res` file itself from the tray icon's drawing code, so
Explorer, Start, the taskbar and Task Manager all say Glance.

`glance` with no arguments now starts the app hidden and returns. Running
the app inside a terminal meant closing the terminal ended every session.
`glance app` is the old behaviour, for the log.

### Reload

Shipping a build used to be four steps from a terminal outside Glance and a
click on every tile. Now it is `target\release\glance.exe reload`, run by
the agent when the human says ship.

- **The request.** `reload` posts `/glance/reload` with the path of the
  binary it was run from, guarded like `/glance/new` (its own
  `X-Glance-Command` value, no `Origin`). `--now` skips the wait.
- **The wait.** The app hands over once no session in a Glance terminal is
  mid turn (`Phase::mid_turn`, only `Working`). A session waiting on you
  is not mid turn; it resumes to the same question. The agent that ran
  `reload` finishes its turn first, which is why the command returns at
  once instead of waiting. The tray tooltip says a reload is pending.
- **The handover.** The app saves with `running` marked on every session
  whose process is alive and `on_stage` naming the session with the
  keyboard on the stage,
  starts `<new build> swap --pid <its pid>` with no window, and quits.
- **`swap`** waits for that process to exit, moves each installed binary
  aside as `glance.old.exe` and `glancew.old.exe` (a running binary can be
  renamed, not overwritten), copies the build in, rewrites the hooks as
  `install` would, and starts `glance.exe app --reload`. It counts as up
  when the port answers and the process is still alive 3 seconds later,
  since the app listens before it builds its windows.
- **Rollback.** A build that is not up within 20 seconds is killed, the
  moved binaries go back, and the old build is started the same way. Only
  what was moved goes back, so a copy failing half way restores exactly
  that. `%APPDATA%\Glance\reload.log` has the last reload.
- **`app --reload`** resumes the sessions saved as `running`, once each,
  without opening a window for each, then puts the stage back. An ordinary
  start still leaves every session paused until clicked.
- **A dev instance** restarts from its own build and copies nothing, which
  is how reloading itself gets tested.

Tested with a dev instance and `cmd.exe` sessions: two running sessions
came back as two, in about three seconds, with the stage showing the same
one; a faked working session held the reload until its `Stop`. The install
path and the rollback were tested against a fake install folder with its
own `APPDATA`, `LOCALAPPDATA`, home and port, the rollback by squatting the
port so the new build could not listen.

The installed Glance has to know `/glance/reload`, so the first build with
it is installed by hand. Sessions still end with the process; reload only
makes the restart cheap. Keeping them alive across it is the separate
process question below.

### Dev instances

Glance is developed from a session inside the installed Glance, so a test
build must never disturb the one hosting the agent. `GLANCE_DEV=1` makes
a dev instance:

- Port 43118 unless `GLANCE_PORT` says otherwise, so both can listen.
- State in `%APPDATA%\Glance-dev`. Sharing `state.json` would load the real
  sessions as paused tiles, and a click would resume a conversation that is
  already live in the installed Glance.
- No autostart offer and no switch for it in the tray. The first dev run
  used to point the `Run` key at `target\debug\glancew.exe`.
- A red tray icon and "Glance dev" in the tooltip.
- `install`, `uninstall` and the hook and Explorer installers refuse to run.

The hook URL is fixed at install time, so a `claude` in a dev terminal
still posts to the installed Glance. Every Glance-started `claude` now gets
`GLANCE_OWNER_PORT`, the hook sends it as `X-Glance-Port`, and a listener
that is not the owner passes the event on to the one that is. Without the
header the installed Glance would adopt dev sessions as ghost tiles.

Tested on screen: a dev instance beside no installed one, a `cmd.exe` tile,
state written to `Glance-dev` only. The hand off between two running
instances is covered by a listener test, not yet seen with a real `claude`.

### The stage

Ten sessions open used to mean ten terminal windows piled on each other.
Now they share one, the stage, which shows one project at a time.

The first version showed one session and swapped it on a click, with a
"pop out" for sessions wanted side by side. Replaced, see The project
grid below: switching sessions one at a time hid the rest of a project,
and popped windows were the freedom that made the desktop a pile again.

- **The project on stage is lit**: its cluster has an edge in the project's
  colour, the same colour as the stage's border (see The look).
- **Where the stage goes.** A new session docks it against the tiles as a
  square filling top to bottom: its top and bottom at the margin, its edge
  a gap from the box around every cluster on the primary screen, on the
  side with more room, narrower only when that side is (`layout::beside`,
  `layout::square`). The first time, it opens the same way. A resume, a
  tile click or the hotkey leaves it wherever it was left (`stage` in
  `state.json`).
- **Next waiting session.** Ctrl+Alt+Space, from anywhere, shows the session
  that has waited on you longest. Pressed again while that one is in front,
  it moves on to the next (`inbox::next`), so one you are not ready for does
  not block the rest. A dev instance uses Ctrl+Alt+Shift+Space so both can
  hold one. Also in the tray menu, with the shortcut beside it.
- **Fit terminal beside tiles** in the tray menu fills that same space
  beside the clusters with the stage, by its visible edges: a Windows 11 window
  has an invisible resize border that would otherwise double every gap.
- **Terminals snap** like clusters, with the same lines, margin and gap:
  when moved (`WM_MOVING`, `layout::snap`) and when an edge is dragged
  (`WM_SIZING`, `layout::snap_edges`). Both measure visible edges. Clusters
  and terminals snap to each other (`snapping::others`), and Shift places
  either freely. The system loops build each rect from the one returned
  last, so a snapped rect snapped again on every small step and the window
  never let go. The free rect comes from the cursor instead, at the
  distance from each edge it had when the drag began.
- The windows to snap to are found by class name, so a dev instance also
  snaps to the installed Glance's windows. Harmless, and useful: that is
  where they are on screen.

Found while testing it: a click on a tile made the cluster the foreground
window, `WS_EX_NOACTIVATE` or not. So "is this terminal in front?" was
always no at click time, and the second click that should collapse a
terminal only brought it forward, before the stage too. The cluster now
answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` itself.

A test trap, not a bug: an app started with `Start-Process -WindowStyle
Hidden` has its first `ShowWindow(SW_SHOWNORMAL)` turned into a hide by
Windows, so the first terminal never appears. Start dev builds with
`-NoNewWindow` instead. `glancew` and the `Run` key are not affected.

### The project grid

The stage shows every session of one project at once, as panes in a grid
(`layout::grid`): one fills the window, two sit side by side, three are two
above one wide, four are two by two. Asked for so a project reads as one
piece of work, with less freedom and more structure.

- **A pane is a child window** (`pane.rs`) with its own render target,
  keyboard and mouse, the old terminal window's insides. The stage
  (`terminal.rs`) is the frame: title, snapping, layout, and the drag.
  Child windows rather than one window drawing every grid, because focus,
  capture, dropped files and the caret then belong to the right session by
  themselves.
- **A tile click switches the project** and gives that session the
  keyboard. A click on the tile of the session typing, with the stage in
  front, collapses it. The hotkey for the next waiting session does the
  same switch.
- **Which sessions are panes**: every console of the project except a
  clean exit. A crash keeps its pane, showing what went wrong, until the
  tile resumes it into the same place. Paused sessions have no pane.
- **The stage follows the registry.** `reconcile` ends in `sync_stage`, so
  a session started in the project on stage joins it, an ended one leaves,
  and an empty stage closes. `TerminalWindow::show` keeps the panes it
  already has, with their selection and scroll, and lays out again only
  when the set changed.
- **Headers.** With more than one pane, each has a header: the phase dot,
  the session name and what the agent says it is doing, with a line of the
  phase's colour along its top. The one with the keyboard is underlined in
  the project's colour and the others step back. One pane alone has no
  header; the title bar says it all.
- **Drag to swap.** Pressing a header gives the stage the mouse
  (`WM_PANE_GRAB`, then `SetCapture` on the stage). Past 4 pixels the pane
  lifts, blue, and the pane under the cursor is outlined; letting go swaps
  the two. The swap goes through the app (`Input::Swap`), which owns the
  order.
- **The order is kept** per project in `grids` in `state.json`. A paused
  session keeps its place, so a restart puts each back where it was as it
  resumes (`layout::grid_order`). A reload brought a swapped grid back
  swapped.
- **Pop out is gone**, with `popped` and `placement` in `state.json`. An
  older file still reads; those fields are ignored.
- The tray's "Arrange terminals" became "Fit terminal beside tiles".

Tested on screen with a dev instance and `cmd.exe` sessions: three in one
project laid out two above one; typing reached the pane clicked; `exit` in
one left the other two side by side with the keyboard moved over; a tile in
another project switched the stage, and a second click collapsed it;
Alt+F4 inside a pane closed the stage; the drag swapped two panes and the
reload kept them swapped.

### The files tile

Every cluster whose project is in git ends in a files tile: the project's
tree as VS Code's explorer shows it, with the git changes coloured in.
Asked for so the changes in a project can be seen at a glance, without an
editor.

- **What it shows.** Every file git knows (tracked and untracked, ignored
  ones hidden) plus deleted ones, folders first, names sorted without
  regard to case. Names take VS Code's dark theme colours and letters: M
  modified, A added, U untracked, D deleted, R renamed, C conflict. A
  folder takes the most important change inside it and a dot. A chain of
  folders with one child each shares a row (`crates/glance-ui/src`). The
  header counts changed files.
- **Folders.** One holding a change opens by itself, so a new change is in
  view without a click. A click opens or closes any folder and is
  remembered by path, so it survives the tree being rebuilt.
- **Files.** A click shows the file on the stage (see The file viewer).
  Ctrl+click opens it in VS Code instead, in the window with the project
  (`Code.exe <project> <file>`, found beside `bin\code.cmd` on `PATH`, so
  no console flashes). Without VS Code, whatever Windows opens it with.
- **Size.** At most 14 rows, then it scrolls with the wheel and shows a
  thin bar. Dragging its bottom edge (a small handle, the cursor turns to
  a resize arrow) sets its height, from 3 rows to the bottom of the
  monitor, kept as `files_height` per cluster. The height is free, not
  whole rows, and the edge snaps like a dragged window, so it lines up
  with the bottom of the stage; room left under the last row stays empty.
  It never grows past the monitor's height, whatever was saved. The header
  folds it to one row, kept in `state.json` as `files_collapsed` per
  cluster. It sits below the `+`, which stays the
  slot where the next session tile goes.
- **Where the data comes from** (`watch.rs`). One thread per project runs
  `git ls-files` and `git status` and hands the tree (`files.rs`, pure and
  tested) to the cluster with a window message. Then it sleeps in
  `ReadDirectoryChangesW` until a file changes: nothing polls, so a quiet
  project costs no CPU. Changes in ignored folders are dropped, from `git
  ls-files --ignored --directory`, or every `cargo build` would rerun git;
  inside `.git` only `index` and `HEAD` count. A burst is waited out (300
  ms quiet, at least 1 s between scans, at most 3 s of waiting).
- `git --no-optional-locks` is required: a plain `git status` may rewrite
  the index, which the watcher would see, and scan again forever.
- A folder outside git gets no tile and no thread.

Tested on screen with a dev instance on this repo: 43 changes shown
coloured, a new file at the root appeared as U within a second and went
when deleted, five writes to `target\` caused no scan, folding, the wheel
and the header fold worked, and a click opened `main.rs` in VS Code. A scan
takes about 100 ms in a debug build, most of it starting git.

Not done yet: a folder with a `.git` above the project folder (a session
started in a subfolder of a repository) does not hear commits, since the
index is outside what is watched. Changed files still rescan it.

### The file viewer

A click on a file in the files tile shows it on the stage, read only, beside
the project's sessions: line numbers, VS Code's Dark+ colours, long lines
wrapped at a space and indented under their text. Asked for so a file an
agent is working on can be read without leaving Glance. A viewer, not an
editor, on purpose.

- **A pane with no program.** A file view is a `Console` without a pseudo
  console, its grid fed escape sequences that paint the file
  (`viewer.rs`, pure and tested). The pane draws it like a session, so
  selection, the wheel, glyph fallback and wide characters cost nothing.
  Control characters in the file become their Unicode pictures, so a file
  can never send the terminal a command.
- **One per project.** A click on another file replaces it, like VS Code's
  preview tab, so browsing never piles up panes. It comes last in the
  grid and is never saved in the order. Closing the stage drops it.
- **Colours** (`highlight.rs`). `syntect` with the Sublime grammars it
  bundles, which are TextMate grammars like VS Code's, and Dark+ written
  out as scope rules, so no theme file ships. Pure Rust regexes
  (`regex-fancy`), no C build. Highlighting runs on a thread; the file shows
  plain first. The bundle has no TypeScript or TOML: `.ts` and `.tsx` borrow
  JavaScript, TOML stays plain.
- **Keys.** Arrows, Page Up and Down, Ctrl+Home and Ctrl+End scroll.
  Ctrl+C copies, and a drag copies on release, as in a session. Copying
  leaves out the line numbers and gives a wrapped line back whole
  (`viewer::copy` maps grid cells to file bytes). Esc or the cross at the
  end of its header closes it. Nothing typed reaches anything.
- **Live.** When the project's watcher rescans, the view reads its file
  again if its size or time changed, keeping the same line at the top. So
  an agent's edit shows within a second or so. A resize lays the file out
  again for the new width, also keeping the top line.
- **Limits.** Files over 4 MB are not read, binary files (a NUL in the
  first 8000 bytes) are not shown, and past 10,000 lines or 20,000 grid
  rows the rest is left out and counted. Each row is a full width of
  cells, so the rows limit is what bounds memory, about 60 MB at 120
  columns.

Tested on screen with a dev instance on this repo: `main.rs` opened beside
a `cmd.exe` pane with the colours right, the wheel scrolled it, a click on
another file replaced it, an edit to the shown file appeared within a few
seconds, a drag across the gutter copied only the text, and Esc and the
cross both closed it with the keyboard back in the session.

Not done yet: a diff view (the changed lines of a modified file marked in
the gutter), a search, and horizontal scrolling instead of wrapping.

### Browser windows

An agent testing a web page starts a browser of its own, through Playwright
or the Chrome DevTools MCP. Its window used to land anywhere, with nothing
saying which session opened it. Embedding a browser in Glance was
considered and dropped: the agent drives its browser over a debug protocol
and never needs a window Glance owns, and it would be the web view the
settled decisions rule out. So Glance manages the browser's window instead.

- **Which session.** Every agent starts inside a job object of its own
  (`glance-pty`, `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it is in the job from
  its first instruction), and everything it starts joins the job. A window
  belongs to the session whose job holds its process (`IsProcessInJob`).
  The job limits nothing and closing it ends nothing.
- **Which windows** (`browsers.rs`). A WinEvent hook, out of context, hears
  every top level window shown and every tracked one destroyed, and posts
  them to the app window. Only main windows count (no owner, a title bar,
  not a tool window) and only browsers, by executable name: Chrome, Edge,
  Firefox, Brave, Chromium, Vivaldi, Opera and Playwright's WebKit. An editor
  or an app under test is left alone, and so is a dev Glance started inside
  a session, whose windows are in that session's job too.
- **Where it goes.** When it first appears, against the stage in the space
  beside the tiles and the stage together (`layout::beside_stage`), its own
  size where that fits and shrunk where it does not. Narrower than 360 DIPs,
  or opened minimised or maximised, it stays where it opened. After that it
  is the user's.
- **It follows its project.** When the stage switches project, that
  project's browsers come back just below the stage in the stacking order,
  and every other project's are minimised. One the user minimised stays
  minimised.
- **The tile mark.** A tile whose session has a browser open shows a small
  window glyph at the end of its second line. It is a button: a click
  restores that session's browsers and gives them the foreground.
- **Never block.** Every call on a browser window is asynchronous
  (`SWP_ASYNCWINDOWPOS`, `ShowWindowAsync`). The window belongs to another
  process, and a hung browser would otherwise freeze the UI thread.
- A URL opened in the user's own browser (`start https://...`) goes to the
  browser that is already running, outside every job, so it is not caught.
- Shrinking a browser changes the page's layout when it has no fixed
  viewport. With a fixed one, the Playwright library's default, the window
  size changes only what you see, not what the agent tests. Not yet checked
  against the MCP servers' own defaults.

Tested on screen with a dev instance on its own port and `cmd.exe` sessions
in two projects, each running `start msedge` with a profile of its own: each
Edge landed beside the stage, starting the second project minimised the
first one's, a tile click switched them back, the tile's button brought its
Edge to the front, and closing an Edge took the mark off its tile. The user's
own Chrome was never touched. The hook costs no CPU to speak of: 16 ms in 5
idle seconds.

### Plain terminals

A project needs terminals that are not an agent: a dev server, a log, a
deploy, a quick `git log`. Asked for so they are at hand beside the
sessions instead of in another terminal app.

- **A shell is a session** with `shell` set (`Session::shell`, saved). So
  it gets a tile, a pane in the project's grid, a place in the saved order,
  the tile menu, and a cluster when it is the project's only session, all
  from the code sessions already had. The alternative, panes with no tile
  like the file viewer, left a dev server with nothing to click once the
  stage showed another project.
- **Ways in.** A small button with a terminal glyph beside the bottom `+`
  (`Hit::Shell`), Ctrl+Shift+T in any pane (`CharAction::NewShell`, as a new
  tab in Windows Terminal; plain Ctrl+T still reaches the program), and
  "New terminal" in the project menu. It opens in the project folder, joins
  the stage without moving it, and gets the keyboard. Named Terminal,
  Terminal 2 and so on.
- **Which shell** (`shell::program`): `GLANCE_SHELL` by path or name,
  otherwise `pwsh`, otherwise Windows PowerShell, otherwise `COMSPEC`.
- **Not a session to the hooks.** The shell gets no `GLANCE_SESSION` or
  `GLANCE_OWNER_PORT`, and loses them when Glance itself inherited them, so
  a `claude` typed into it is nobody's and stays off the tiles, as it
  would outside Glance. Found on screen: a dev instance started from a
  session handed its tag down.
- **The tile.** No hook reports on a shell, so its phase stays idle and the
  tile reads the terminal instead: the terminal glyph, the title the program
  set as its second line (`shell::title` drops a title that only names the
  executable, and takes the command from `cmd.exe - npm run dev`), and its
  output as the activity trace, at most one mark a second
  (`Session::touch`). The pane header and the stage's title use the same
  cleaned title.
- **Lifetime.** Any exit closes it, tile and pane, whatever the code: `exit`
  means done, and there is nothing to resume. Closing the stage leaves it
  running. "Start over" leaves a project's terminals alone. Quitting Glance
  says terminals close, apart from the sessions that resume. After a restart
  a terminal comes back as a paused tile in its place, and a click opens a
  fresh shell there; a reload reopens the ones that were open. What ran in
  it is gone either way: the process question below covers shells too.

Tested on screen with a dev instance: the button opened Windows PowerShell
(no `pwsh` on this machine) as a second pane beside a `cmd.exe` session
with the keyboard, setting the window title from inside it showed on the
tile, the pane header and the stage, `exit` removed tile and pane, a
restart brought it back paused and a click reopened it, one process each
time and no `claude`. Not tested on screen: Ctrl+Shift+T, since a scripted
key would go to whatever window has the focus; the mapping is unit tested.

### The usage window

A window of its own at the top of the stack: how much of the account's
Claude limits is used, and the model, effort and permission mode every
session Glance starts gets. Asked for so the limits are in view without
typing `/usage`, and so a model can be picked once instead of per session.

- **Where the numbers come from.** No hook carries usage, and there is no
  public API for a subscription's limits. Claude Code gives them only to the
  status line command, as JSON on stdin after each reply: the model, how
  full the context is, and the five hour, weekly and spend limits with when
  each resets. So Glance hands every `claude` it starts `--settings
  %APPDATA%\Glance\claude-settings.json`, written at each start, which makes
  `glance status` its status line. That passes the JSON to
  `/glance/status` on the owner's port, tagged like a hook, and prints
  `Haiku 4.5 · context 21% · session 28%` for the terminal. A `claude`
  started anywhere else is not touched, the same rule the hooks keep. The
  path goes without quotes unless it has a space, since PowerShell takes a
  quoted first word as a string.
- **What it shows.** One row per limit Claude Code sent, each with a bar,
  the percent and how long until it resets, blue, amber from 75 %, red
  from 90 %. A limit whose reset has passed reads as empty. The header says
  how long ago the numbers came. They are the account's, so the latest from
  any session wins, and they are saved, so the window is not empty after a
  restart. Before the first reply it says so.
- **Settings.** Model, Effort and Permissions, each a menu with Default
  first, which passes nothing and leaves it to Claude Code. The values are
  what `claude --help` lists. Bypass permissions sits behind a separator.
  They are passed as `--model`, `--effort` and `--permission-mode` when a
  session starts or resumes (`Defaults::flags`), unless its own arguments
  already say, and never saved in its arguments, so a resume takes the
  defaults as they are then. A running session keeps what it started with;
  the menu says that. Saved as `defaults` in `state.json`. Nothing goes to
  a shell put in with `GLANCE_AGENT`.
- **Context on tiles.** Each session keeps the last status it heard
  (`Session::status`, not saved), which the tile draws, see The look.
- **The window** (`usage.rs`) behaves like a cluster: no focus, dragged and
  snapped the same way, folded by its header, pinned once dragged, raised
  and tidied from the tray with the rest. It counts as a tile when the
  stage docks. Kept as `usage_window` in `state.json`.
- **It looks like a cluster too** (see The look): acrylic behind it, the
  limits and settings as panes of glass (`Painter::glass_pane`, shared with
  tiles and the files tile), the name in the display face beside a mark
  with no colour, since an accent would claim a project, and Fluent
  chevrons for folding and for each setting's menu. It was built before
  the look and drew flat and opaque until then.

Tested with a dev instance: the empty window, fake limits at 41 % and 82 %
(blue and amber bars, the window growing to fit), a tile's context at
85 %, and a real `claude` started with Haiku and low effort as defaults. It
ran as `claude --model haiku --effort low --settings ...`, one process,
and after its reply the window showed 28 % and 21 % with reset times and
the terminal the status line. Not tested: picking from a setting menu by
hand, since a scripted click lands on the installed Glance's windows.

### The look

The first look was a default dev tool: flat boxes, one type size, a phase
as a tinted fill, nothing moving. Asked for something with its own
identity. The rule behind it: colour has two jobs and they never share a
place. A phase is light, on a tile's edge and in its icon. A project is an
accent, on its mark and on the stage's edge. So a project's colour can never
be read as a session needing you.

- **Deep dark, then glass.** The look is near black first. Clusters have
  acrylic behind them (`backdrop.rs`, `DWMWA_SYSTEMBACKDROP_TYPE` with the
  frame extended over the client area), under a tint of 92 %, so the
  desktop is only a hint of depth. The cluster's render target is
  premultiplied, so what it leaves clear shows the acrylic. Text there is
  greyscale: ClearType needs to know the colour behind it. Tiles are a
  breath of white (3 %) with a faint lit top edge. Every edge is soft light,
  never a bright line: only a waiting tile's edge is plainly visible, since
  it is the one thing meant to catch the eye. The stage had Mica at first
  and it lifted the whole window toward grey, so it has none: its title bar
  is the terminal's near black (`DWMWA_CAPTION_COLOR`), its gaps are black
  and the panes float a shade lighter. A Windows without system backdrops
  refuses the attribute and gets opaque clusters.
  `DwmExtendFrameIntoClientArea` is declared by hand: the windows crate has
  it behind `Win32_UI_Controls`, a large feature for one call.
- **Phase as light.** A working tile has a light going round its edge, a
  dash on the outline with a radial fade for its tail. A waiting tile
  breathes, and sends a ring off itself the moment it starts waiting. A
  finished turn flashes green once and settles to a soft edge. Idle, paused
  and ended tiles have no edge and fade back. `theme::edge_strength` and
  `theme::presence` hold the numbers.
- **Project accents.** Eight colours chosen away from every phase colour,
  picked by an FNV hash of the project key (`theme::accent`), so a project
  keeps its colour across runs. It marks the cluster's name, washes faintly
  down from its top edge, rings the cluster whose project is on the stage,
  tints the stage window's border (`DWMWA_BORDER_COLOR`, the accent sunk
  two thirds into the dark) and underlines the pane with the keyboard. All
  of them muted: an accent says which project, it does not outline one.
- **A live tile.** The icon is the tool the turn is in (Segoe Fluent Icons,
  `theme::tool_icon`), or the phase when there is none. The session keeps
  the tool and when it last did something (`Session::tool`,
  `Session::activity`, neither saved), and the tile draws the last ten
  minutes as a trace of twenty bars. How full the context is, from the
  status line, is a ring round the icon; from 75 % the number shows as
  well, amber then red, in the trace's place. The top line says only how
  long, except for waiting, where the verb decides what you do.
- **Motion** (`motion.rs`, `anim.rs`, pure and tested). A new tile rises
  into place, tiles slide when the list changes, hover fades in 120 ms.
  On the stage the panes without the keyboard step back behind a veil of
  the background, and panes fade in when the stage switches project.
  Windows' animation setting (`SPI_GETCLIENTAREAANIMATION`) off holds all of
  it still.
- **Type.** Project names in Segoe UI Variable Display, session names
  semibold, ages with tabular digits so they do not shuffle each second,
  FILES letter spaced, the header counts as small chips.

What it costs, measured on a release build with two clusters of fake
sessions: 1.1 % of one core while two tiles work and two wait, 0.8 % with
only waiting ones breathing, 0.2 % with nothing moving. The first cut cost
11 %. Two changes got it down. Everything that holds still is drawn once
into a compatible render target and copied each frame, with only the light
drawn fresh (`Painter::still` and `Painter::light`); the layer is redrawn
when anything else changes. And gradient brushes are kept by their stops
and moved or faded by their properties, since each new one is a texture on
the GPU. A cluster asks for frames by timer only while something moves: 40
ms while a light goes round, 66 ms while only a breath does, none at rest.

Tested on screen with a dev instance on its own port and state folder: the
acrylic behind clusters with Chrome underneath, the orbit, the breath and
the ring leaving a tile that starts waiting, the done flash, three `cmd.exe`
panes on the stage with the unfocused two dimmed, the stage's border
matching its cluster's mark, context rings at 38 % and 82 %. After the first
look proved too light and its borders too bright, the darker pass was
checked the same way beside the installed build.

Not done: the tray menu is still light, and the terminal font is still
fixed. Panes are square: a child window can only be rounded with a window
region, whose corners are jagged.

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
  waiting for. The hotkey (see The stage) walks it already. Still open
  whether it also needs a window or a sort order inside the clusters. Decide
  when forty sessions is real, not before.
- A signed updater, liftable from Purrch (`../purrch.fun/src-tauri`).
  `glance install` covers installing for now; NSIS only if a download for
  other people needs it.
- The tray icon exists (see Launchers). What is left for it here: start
  with Windows, and a way to show collapsed terminals.

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
  to the left edge. Needs a settings file, probably
  `%APPDATA%\Glance\state.json`.
- **Only the primary monitor.** `arrange` reads `SPI_GETWORKAREA`, which
  ignores the other screens. Snapping already uses the monitor under the
  cursor.
- **Snapped clusters do not stay together.** Nothing remembers that two
  clusters were snapped, so a cluster that grows can overlap the one below
  it, and moving one leaves the other behind.
- **The tray menu is light in dark mode.** Win32 popup menus only follow
  the dark theme through undocumented `uxtheme` calls.
- **New tray icons start hidden.** Windows 11 puts them behind the `^`
  overflow until the user drags them out or turns them on in Settings.
- **No remote.** The repository exists on one disk. Push it somewhere.

## Open questions

Carried from the concept, with what is known now.

- **Does forty sessions hold up?** Five tiles cost 45 MB and no CPU. One
  open terminal adds a few MB. History is bounded at 2000 rows a session, so
  the worst case for the grids is about 230 MB. The other cost is forty
  `claude` processes, which is not ours. Not yet tried with forty.
- **Can a tile light up without stealing focus?** Yes, so far.
  `WS_EX_NOACTIVATE` plus `SWP_NOACTIVATE` holds through raising.
- **How does a session get named?** Claude Code writes its own title for
  the conversation into the transcript (`ai-title`, or `custom-title` after
  `/rename`). The listener reads the transcript's tail on `SessionStart`,
  `UserPromptSubmit` and `Stop` and the tile shows that title. A name given
  with `--name` beats Claude's title but not a `/rename`. The folder name is
  the fallback until the first turn ends.
- **What about the VS Code extension's Claude?** Still unanswered. Probably
  the answer is to stop using it.

## Name collision

`glanceapp/glance` is a self-hosted dashboard with more than twenty thousand
stars on GitHub. The crate name, the binary name and any published package
need a decision before this goes public. Undecided.
