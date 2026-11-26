# Takeover

A session operates a program on the human's own desktop, with real
clicks and keys, within what the human allowed. Windows only.

It is the second of the two ways a session uses the PC. The first, and
the default, is the agent's desk: see [computer use](computer-use.md)
for both side by side and the rule that picks one. Takeover is for what
only works on the human's own desktop:

- a program that is already open there, with the state the work is
  about (a project loaded, a device connected, a debug session running),
- a program that always runs as administrator,
- a program bound to hardware or to a licence that one session holds.

Why things already built are the way they are is in the lore: ask
`lore_recall` about takeover or about a file named below. The tools
were first named `desktop_*`, and a few names that two builds speak to
each other keep that word: the ops between `horadric mcp` and the app
(`desktop.allow`, `desktop.done`, `desktop.elevating`, `desktop.mode`),
the helper's pipes (`horadric-desktop-...`), the saved setting
`desktop_glide_ms` and the variable `HORADRIC_DESKTOP_WAIT_MS`.
Everything else is named takeover.

## The shape

Five parts. Only the first touches the program.

### The engine (`horadric mcp`, `crates/horadric/src/takeover.rs`)

The work happens in the MCP process, which runs on the human's desktop
as a child of the agent. UI Automation, `PrintWindow` and `SendInput`
happen there, so a slow call into another program never stalls
Horadric's UI thread.

The tools:

- `takeover_windows`: the windows on the desktop. Titles are cut short
  until the human lets the session into the program. A window that runs
  as administrator is marked, so the agent knows Windows will ask.
- `takeover_snapshot`: a window's controls as text, each with a ref.
- `takeover_screenshot`: a picture of a window, for what a snapshot can
  not tell.
- `takeover_click`: a real mouse click at the element's middle, after
  the window comes forward. The human's pointer is put back after.
- `takeover_type`: text into a field. What the field holds is read back
  before Enter, and Enter is not pressed when it is not what was typed.
- `takeover_press`: a key, with modifiers.
- `takeover_text`: the text of a list, grid, output pane or editor.
- `takeover_changes` and `takeover_wait`: see the watcher.

The old `desktop_` names are still answered and no longer listed.

What the engine holds itself to:

- It asks before it reads. Nothing of a program is read until the app
  allowed the session in, so names in a program nobody allowed can not
  be sounded out.
- Reading never presses anything.
- A click reads its target again after the question, and refuses one
  that is gone, off screen or covered by another program. It also
  refuses while the human holds a mouse button.
- Typing stops when another window takes the front. A line break or a
  tab typed as keys is refused outside a document.
- Space and access keys in a dialog count as risky: they press buttons
  nobody named.
- The select all and copy path of `takeover_text` reads the clipboard
  only when the target program owns it, and is refused when the human's
  clipboard holds anything but plain text.
- UI Automation reads have time limits, so a hung program in front does
  not hold every later call.

Whether an action is risky is decided by pure, tested functions (`risk`,
`key_risk`, `typed_refusal`): the agent's own `risky` flag only adds to
them.

### The app, which only grants (`crates/horadric-ui/src/takeover.rs`)

Before each action the engine asks the app (`op: desktop.allow`, with
the window, its program, what it will do and whether it is risky). The
app keeps what the human allowed and asks them when it does not know.

- Grants are per session and per program, in two levels: look and
  operate. A program's dialogs come with it. A grant lasts until
  Horadric restarts.
- A grant is keyed by the program's full path, whether it runs as
  administrator, and, for a program that only hosts other things, what
  it shows.
- A risky action is asked every time and never remembered.
- The question is put in a corner clear of the program and its dialogs,
  without taking the focus. Its default button refuses. It waits
  `answer_within` seconds (60 when not set, 10 to 80), then the session
  is told nobody answered, apart from a refusal.
- A refused or unanswered question is held for 2 minutes before it can
  be asked again, and at most four wait per session.
- One session drives a program at a time. Another session's operate
  call is refused with who holds it. Looking stays open.

### The elevated helper (`takeover_helper.rs`)

Windows hides an elevated window from a session that is not elevated,
and drops the input sent there. For such a window the engine starts the
helper as administrator, which Windows asks the human about, and hands
it the call over two named pipes.

- It is bound at its start to one program, by full path. A second
  elevated program means a second helper and a second question from
  Windows.
- It checks every window itself, asks in a window of its own, judges
  risk itself and keeps its own log. The next section says why.
- For such a program the helper also does the watching.
- It ends with the session that started it. A no from Windows, or a
  helper that failed or was lost, is remembered for 2 minutes, so the
  agent can not have the human asked over and over.

### The watcher (`takeover_watch.rs`)

From the first time a session looks at a program or operates it, the
engine watches it with WinEvent hooks and keeps the last 500 changes:
windows and dialogs shown and closed, the front, menus, titles, and
elements whose name, value or state changed.

- `takeover_changes` tells what changed since the session last asked,
  one line each. Click, type and press end with what changed because of
  them.
- `takeover_wait` blocks until a window appears or is gone, a text
  appears or the program goes quiet (10 seconds unless `timeout_ms` or
  `HORADRIC_DESKTOP_WAIT_MS` says otherwise, 60 at most). It tells what
  came before the wait apart from what came during it.
- Watching ends with the grant: the app is asked again before changes
  are told, and a no unhooks the program and drops what was kept.
- A process handle pins what is hooked, so a reused process id can not
  bring in a program nobody approved.
- A stuck watcher is said. It never answers "nothing changed".
- Protected fields are never read.
- The budget of 60 changes a second applies before anything is read.

### The overlay (`crates/horadric-ui/src/ghost/`)

What the human sees while a session has their desktop. Three things,
each with one purpose:

- A mark that travels to where the next click will land, before it is
  made: the session's rune, with the session's name small beside it.
  The click waits for it (2 seconds, set by a slider on the plate), so
  the human sees the aim before the act and has time to say no. A rune
  and not an arrow, since the one pointer on screen is the human's.
- A frame round every screen, which says control is on: a thin, soft
  light along the screen's edges, with nothing sharp in it. The human
  rejected a rune outline here and asked for a subtle glow, the one
  exception to the app's rule that activity is shown by runes and not
  by glows. The graphics card draws it once and the compositor moves
  it, so it costs the app nothing per frame.
- The Thinking plate, which says which session it is (by name, id and
  start), what it is about to do and why, and what it did. It is a flat
  plate like the app's others, and runes are cut on it while the
  session works: three by the title, and one by each shell command
  still running. It also lists the shell commands that session runs
  beside the control.

Holding Esc (0.6 seconds) stops the session's action. A tap of Esc
passes through to the program in front, and only the human's own keys
count, never ones a session sends.

The plate has one switch, Bypass, turned on by holding the mouse on it,
for a human who trusts the work and watches it. It is on for one
session, the one the plate showed when it was turned on: another
session working meanwhile is asked as ever, and its calls do not keep
Bypass on. It only skips the risky question for programs that session
may already operate. It never grants a program, never applies to an
elevated one, is never kept, and ends with that session's work (30
minutes at most). A risky question of that session already on screen
when it is turned on is put away and its call goes through Bypass; an
approval question, another session's question, or any question about
an elevated program, stays up.

To look at the overlay without a session, start a dev instance with
`HORADRIC_GHOST_SHOW=1` (`bypass` for Bypass's colours, `idle` for
control merely on; then optionally `,` seconds before it starts `,`
seconds it lasts). Nothing is clicked and no call is answered.

## Who decides, and what the lists can promise

A session's agent runs as the same Windows user as Horadric, with a
shell. So nothing kept where that user can write is a wall:
`takeover-programs.json` and `takeover.log` in `%APPDATA%\Horadric` can
be written by any program of the user, the agent among them, and
`horadric takeover` refusing to change a list from inside a session is
a speed bump, not a guard.

That list therefore only answers what the user could be asked anyway:
the approval question of a program that does not run as administrator.
It covers no elevated program and no risky action.

Programs that run as administrator, and every pre-approved risky action
of any program, are on the administrators' list in
`%ProgramData%\Horadric`. It is written only from an elevated shell, by
`horadric takeover allow` and `deny` with `--elevated` or `--admin`,
and by `approve` and `unapprove`. It is refused on reading unless its
folder and file are owned by, and writable only by, administrators.

The elevated helper trusts neither the session nor the app:

- It is bound at its start to one program, by the full path Windows
  shows before the yes.
- It acts only in windows whose process it finds to run that file.
- It asks the human in a window of its own before it first operates the
  program and before every risky action, which it judges from what it
  reads itself.
- Its log, `takeover-elevated.log` beside the administrators' list, is
  the one record the session can not rewrite.

What this does not promise:

- Operating a program that runs as administrator can not be contained
  to it. Its Open and Save dialogs can start other programs with its
  rights, so the helper asks about every action while one is open and
  says so in its first question.
- Looking at such a program is not asked by the helper. After Windows'
  yes it rests on the app's word.
- A pattern can no longer be narrowed to a session, since a session
  names itself.
- The helper's own file is in a folder the user can write, and releases
  carry no Authenticode signature, so whatever replaces `horadric.exe`
  runs elevated on the next yes. Only moving the install closes that.

## Using it

**The tray.** The tray menu has an entry "Takeover" with two lines,
"Off" and "Ask before each program". While it is off the tools are not
listed to any session and every call is refused. "Ask before each
program" is what the rest of this page describes. The choice is kept
across restarts.

**The command.** `horadric takeover` (`horadric desktop` is kept as an
alias). The human runs it in a terminal of their own: it changes no
list from inside a session.

```
horadric takeover list
horadric takeover allow notepad.exe look
horadric takeover allow "C:\Program Files\Globex\Viewer\viewer.exe" operate
horadric takeover deny notepad.exe
horadric takeover log 50
```

`log` shows the last 20 lines when no number is given.

A program is named by its exe's full path, which only that install
matches, or by its exe name, which any program of that name matches. A
listed program is let in that far without the approval question, for
every session. An edit counts from the next call, with no restart.
Horadric's own programs can not be listed.

**The administrators' list.** From a terminal started with "Run as
administrator":

```
horadric takeover allow --elevated "C:\Program Files\Acme\Acme Studio\AcmeStudio.exe" operate
horadric takeover approve --elevated "C:\Program Files\Acme\Acme Studio\AcmeStudio.exe" click "_Delete" --window "Open Project" --control Button
horadric takeover approve --elevated "C:\Program Files\Acme\Acme Studio\AcmeStudio.exe" press Enter --window "Import solution"
horadric takeover unapprove --elevated "C:\Program Files\Acme\Acme Studio\AcmeStudio.exe" 2
horadric takeover log --elevated
```

- `allow --elevated PROGRAM look|operate` is for a program that runs as
  administrator, such as an engineering tool that always restarts
  itself elevated. `deny --elevated PROGRAM` takes it off again.
- `allow --admin PROGRAM operate` puts a program that does not run
  elevated on the administrators' list, so risky actions in it can be
  pre-approved.
- `approve` is the yes ahead of time for one named risky action: a
  click or type on an element by name, or a key. It can be narrowed
  with `--window`, `--control` (not for a key) and `--text` (a line of
  a small dialog's text). Text is matched whole, ignoring case, with
  `*` for any run.
  The log shows the window and the question beside each action, so a
  pattern can be written from a line of it.
- A pre-approved action is done without asking and logged with the
  pattern that let it through. `list` numbers the patterns for
  `unapprove`.

Both files have the same form:

```json
{
  "answer_within": 60,
  "programs": [
    {
      "program": "C:\\Program Files\\Acme\\Acme Studio\\AcmeStudio.exe",
      "level": "operate",
      "elevated": true,
      "pre_approved": [
        { "action": "click", "control": "Button", "name": "_Delete", "window": "Open Project" },
        { "action": "press", "key": "Enter", "window": "Import solution" }
      ]
    }
  ]
}
```

In the user's file, `elevated` entries and `pre_approved` patterns are
not used. A file that does not parse counts as empty, so everything is
asked, and a toast says why. An unknown field fails the file, since a
misspelt one would match more than meant.

**The logs.** Every grant and answer is a line in `takeover.log` in
`%APPDATA%\Horadric`, saying whether it came from a list, a dialog or
bypass, with the shell commands the session ran beside the control. It
is kept under a size. The helper's own decisions are in
`takeover-elevated.log` in `%ProgramData%\Horadric`, which
`horadric takeover log --elevated` shows.

**The old names.** The files were `desktop-programs.json`,
`desktop.log` and `desktop-elevated.log`. In `%APPDATA%\Horadric` an
old file is moved to its new name the first time it is asked for, and
never over a new one. In `%ProgramData%\Horadric` it is read under its
old name while no new one is there, and renamed when an elevated
process next writes. The commands `horadric desktop` and
`horadric desktop-helper` and the `desktop_` tool names are still
answered.

## Not yet tried on screen

Takeover as first built was used on real programs: sessions drove an
engineering tool that runs as administrator on a customer's machine,
attended and unattended. What was changed on 2026-11-26 only compiles,
with its pure parts tested, apart from two things that ran: the check
of who may write the administrators' file, against real files in a
test, and the overlay's show (`HORADRIC_GHOST_SHOW`) on a dev instance,
where the rune outline was seen and turned down and the light's cost
was measured. No session has driven a program with this build. None of
this has been seen working on a real program:

- The engine's new rules: asking before reading, the click that reads
  its target again, typing that stops when the front changes, the
  clipboard checks, the time limits on reads. The clipboard check will
  refuse often, since browsers and editors put more than plain text on
  the clipboard.
- The watcher's new rules: ending with the grant, the process handle,
  the stuck watcher's answer, the read budget.
- The elevated helper deciding for itself: its binding to one program,
  its own question window, its own risk judgement, the switch to a
  second helper, its log.
- The administrators' list end to end: written from an elevated shell,
  read by the app and the helper. Only the check of who may write a
  file was run against real files, in a test.
- Grants keyed by full path, the 2 minute hold on a refused question,
  the limit of four waiting questions, Bypass's narrower reach.
- The overlay with a session behind it. The light round the screens
  as the compositor now moves it has not been looked at by the human,
  nor the mark as the session's rune with its name, nor the plate's
  runes while a real session works. Esc told apart as held or tapped by
  a low level hook, and Bypass turned on by a held mouse button, only
  compile.
- Bypass and the question on screen: the risky question put away when
  Bypass comes on and its call decided again, and the app telling the
  overlay which calls operate. The rule for which question goes is
  tested, the rest only compiles.
- The shell commands on the plate and in the log.
- The renamed tools, command and files, with the old names answered.
  When a file is moved is a tested rule; no real `desktop-programs.json`
  has been carried over.
- The tray's "Off" and "Ask before each program", and the tools leaving
  a session's list while it is off.

## Left

- Try everything above against a real program, an elevated one among
  them, with the human watching.
- Host this engine in the desk's hand, so the desk gets snapshots with
  refs, changes and wait (see [computer use](computer-use.md)).
- An idea, not decided: pause the moment the human touches the mouse or
  keyboard, in place of today's held Esc and the refused click while a
  mouse button is down.
- A frame drawn round the element acted on, so the human sees where in
  the program the agent is.
- Whether answered grants should outlive Horadric, per project and
  program. The lists are the ahead of time answer for every project.
- A page in the app's settings for the lists, beside the command.
- The helper still needs Windows' yes when it starts, once per session
  and elevated program. One helper per Horadric would ask once a day,
  at the cost of one elevated process serving every session.
- Whether a helper that went silent can always be stopped: the handle
  Windows gives back for an elevated start may not allow ending it.
- Moving the install to a folder the user can not write, and signing
  releases, which is what closes the last gap in the section above.
