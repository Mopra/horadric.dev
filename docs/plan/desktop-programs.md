# Desktop programs

Asked for on 2026-11-09, after a debugging session on a PLC project in
Sysmac Studio where the human typed variables into its Watch window and
sent screenshots back, round after round. Sessions get desktop programs
the way the browser pane gives them the web: they look at a program's
window and operate it, within what the human allowed. Windows only.

- **The work happens in `horadric mcp`.** It runs on the human's desktop
  as a child of the agent, so UI Automation, `PrintWindow` and
  `SendInput` happen there (`desktop.rs` in `crates/horadric`), and a
  slow cross process UIA call never stalls Horadric's UI thread. Tools:
  `desktop_windows`, `desktop_snapshot` (the UIA tree with refs, as
  `browser_snapshot`), `desktop_screenshot`, `desktop_click`,
  `desktop_type` and `desktop_press`. A click is a real mouse click at
  the element's middle, where the ghost glides, after the window comes
  forward and the point is checked to be the program's and not a window
  covering it; an element out of view is scrolled into it first, and one
  with no place on screen is not clicked. UIA's invoke, toggle and select
  were dropped for clicks on 2026-11-16: a button whose handler opens a
  modal dialog held its invoke until the dialog closed, and with it every
  later UIA call of that MCP process to the program, so the agent could
  not answer the dialog. Only an element whose one action is expanding is
  still expanded through UIA. Typing sets a field's value through UIA or
  types keystrokes. The human's pointer is put back after.
- **The app only grants.** Before each action the MCP process asks the
  app over the browser channel (`op: desktop.allow`, with the window, its
  program, what it will do, and whether it is risky). Grants are per
  session and per program, so a program's dialogs come with it, in three
  levels: none, look, operate. The first request asks the human with a
  dialog; the grant lasts until Horadric restarts.
- **Risky actions are asked one by one.** Transferring to a controller,
  changing its mode, forcing, resetting, deleting, writing: the agent
  says so with `risky`, and the MCP process also finds the words itself
  in the element's name, the window title and a small dialog's text. Each
  is confirmed by the human and never remembered.
- **Programs that run as administrator.** Windows hides an elevated
  window from a session that is not, and Sysmac Studio always restarts
  itself elevated. For such a window `horadric mcp` starts `horadric
  desktop-helper` as administrator (`desktop_helper.rs`) and hands it the
  call over a named pipe that takes one local client, each end checking
  the other's process id. The program is named as running as
  administrator, so a grant for the normal program does not cover it.
  A process whose token Windows will not open for the session counts as
  elevated (a session that is not can not read an elevated token), and a
  window whose program will not even say its file is resolved by the
  helper rather than refused. `desktop_windows` marks such windows "(as
  administrator)", so the agent knows before its first call that Windows
  will ask. Before Windows asks, the app is told (a `desktop.elevating`
  body, shown as a notification naming the session and the program), so
  the human knows why the question comes. The start runs on a thread of
  its own: a call waits 90 s for the human's answer (a later call 20 s),
  then tells the agent Windows is still asking and to call again, and a
  later call picks up the helper that connected meanwhile instead of
  asking twice. A no is remembered for 2 minutes, a failed start or a
  lost helper for 2 minutes from the last time Windows asked, so the
  agent can not have the human asked over and over, and after that a
  call may ask again (`hold_left`). A helper silent for 150 s in one call
  (past the longest wait) is stopped and counted lost. The helper ends
  when the pipe is cut and also watches the `horadric mcp` that started
  it, so no elevated process outlives its session. Without a helper,
  nothing that sends input (click, type, press, and `desktop_text`, which
  would select and copy) is sent to an elevated window: Windows drops
  such input without telling `SendInput`, so a refusal is the only
  honest answer.
- **Watched between looks.** From the first time a session looks at a
  program or operates it, `horadric mcp` watches it (`watch.rs`): a
  thread of its own holds out of context WinEvent hooks on that process
  and keeps the last 500 changes, each numbered: windows and dialogs
  shown and closed, the front, menus, titles, and elements whose name,
  value or state changed, read through MSAA after a burst is folded (one
  element changing fast is one change from first to last, at most 60
  are kept a second, caret, location and reorder events are not hooked).
  `desktop_changes` tells what changed since the session last asked, one
  line each, and `desktop_wait` blocks until a window appears or is
  gone, a text appears or the program goes quiet, so an agent asks
  cheaply instead of taking snapshot after snapshot. Click, type and
  press end with what changed because of them. WinEvents from a program
  run as administrator do reach a hook that is not (tried on
  2026-11-09), but its elements then mostly will not say what they hold,
  so for such a window the helper watches, and its changes come over
  the same pipe. An unnamed `desktop_changes` adds the helper's changes,
  or says why they could not be read; an unnamed `desktop_wait` asks the
  helper every 300 ms what changed in its programs (`helper_peek`, a
  call between the two processes that no agent can make), so a text
  there ends the wait and quiet needs both sides quiet.
- **Programs allowed ahead of time.** Asked for on 2026-11-11, for a
  machine a session works on with nobody there to answer the approval
  dialog of each new session. `desktop-programs.json` beside the app's
  state (`%APPDATA%\Horadric`, `Horadric-dev` for a dev instance) lists
  programs with a level, look or operate: `{"programs": [{"program":
  "C:\\Program Files\\OMRON\\Sysmac Studio\\SysmacStudio.exe", "level":
  "operate"}]}`. A program is named by its exe's full path, which only
  that install matches, or by its exe name, which any exe of that name
  matches (`.exe` and case optional). A path entry wins over a name
  entry. The MCP process sends the window's full path beside its exe
  name, and a program reached through the elevated helper is matched on
  its plain name, so Sysmac Studio running as administrator is the same
  entry. For every session Horadric holds, a listed program is let in
  that far without the dialog; what the human answers for a session
  still adds to it, and a program not on the list asks as before. The
  app reads the file again whenever its time or size changes, so an edit
  counts from the next call, and it never copies a listed grant into the
  session's own, so taking a program off counts at once too. A file that
  does not parse counts as empty (so everything is asked), with a toast
  saying why. Horadric's own programs can not be listed, since a session
  operating Horadric could answer its own questions. Edited by hand or
  with `horadric desktop allow PROGRAM look|operate`, `deny PROGRAM` and
  `list` (pure parts in `horadric_core::desktop_programs`). A grant from
  the list is shown in a toast once per session and program, and every
  grant and answer is a line in `desktop.log` beside the list, saying
  whether it came from the list, a dialog or bypass (`horadric desktop
  log`). A listed call still glides the ghost and shows on the Thinking
  plate like any allowed call.
- **The list never covers a risky action.** There is no level above
  operate, and the risky question is asked whatever the grant, from the
  list or a dialog. This is OptiPeople's rule for customers' equipment:
  nothing on a machine is changed without a technician's yes, each time.
  Only the plate's Bypass, the human there and holding Esc, skips it,
  and since 2026-11-18 a pattern the technician pre-approved for that one
  action (below).
- **Bypass ends with the work.** Seen on 2026-11-12: a session finished
  its task and the plate still said Bypass on. Bypass is now on for the
  session the plate showed when it was turned on and for every session
  whose call comes while it is on. The hook thread passes each session's
  `Stop`, `StopFailure` and `SessionEnd` to the overlay, which also reads
  the sessions' phases: a session ends its share when its turn ends that
  way, when it is gone, or when it is seen working and then resting
  (done, idle, failed). One already resting when bypass was turned on
  has a minute to start working. Once every such session has ended its
  share and no action is gliding or queued, bypass turns itself off and
  the plate's history says so.
- **From a video of a Sysmac test, 2026-11-12.** Two sessions of the same
  name ran at once and nobody could tell which drove the program, the
  plate stood about 50 seconds on "opening the File menu" while the
  agent copied files in a shell, about 40 seconds went in a file dialog
  the typing had missed, and the Output pane could only be read off
  screenshots. So:
  - One session drives a program at a time. Another session's operate
    call in a program is refused, with who holds it, while the session
    that last operated it is still at work, or while another session's
    question about operating it waits. Looking stays open. Sessions are
    named by id and start beside their name in the questions and on the
    plate.
  - NOW on the plate turns to what the session does instead, from its
    tile's tool and phase: working in the shell, on files, in the
    browser, finished its turn, ended, with the last desktop action
    under it.
  - `desktop_type` reads the field's value back before Enter and says
    what it holds; when that is not what was typed, Enter is not
    pressed and the call fails. `desktop_wait` gives up after 10 s
    unless `timeout_ms` or `HORADRIC_DESKTOP_WAIT_MS` say otherwise, and
    a wait that times out is an error telling the agent to look first.
  - `desktop_text` reads lists, grids, output panes and editors: UI
    Automation's text pattern first, else select all and copy (an
    operate action), putting the clipboard's text back after.
- **From the unattended servicetjek machine, 2026-11-18.** Sessions there
  run with nobody at the screen, so every risky question ended "refused
  by the human", and sessions said the question covered Sysmac Studio's
  own dialogs. The question then was topmost, centred under the mouse
  (which the agent's real clicks had just put on the program), and took
  the focus and the mouse; any click or focus change elsewhere put it
  away, which counted as a refusal. A session's own next action in the
  program refused its question for it. So:
  - A question about a program is put aside (`dialog::show_aside`): in
    the corner of the program's screen that covers least of its window
    and its last dialog, bottom right by the tray when they tie, not
    topmost, shown without the focus and without capturing the mouse.
    Losing the focus or the mouse no longer answers it; only a button,
    Esc in it, or its time does.
  - It waits `answer_within` seconds from the file (60 when not set, 10
    to 80, since the agent's call gives up at 90; a question queued
    behind others waits only the rest). Then the session is told no
    technician answered, nothing was done, and to ask in the chat, and
    `desktop.log` says "no answer, nobody answered within N s", apart
    from "refused by the human". A queued call whose agent already gave
    up is dropped, not asked late.
  - Enter in a standard Open or Save dialog (one with an edit field or
    combo box named "File name:", "Filnavn:" or "Dateiname:") and typing
    a path into that box with Enter are no longer risky by themselves:
    they pick a file on this PC. The dialog's title and the element's
    name are still checked for the risky words, and Delete there is
    still risky.
  - Pre-approved actions. This changes the rule above, at the human's
    ask: a listed program at operate can carry `pre_approved` patterns,
    each the technician's yes ahead of time for one named action, so the
    rule still holds that nothing changes without a technician's yes.
    A pattern has an `action` (click, type, press) and must name the
    element (`name`, for click and type) or the key (`key`, for press),
    not by wildcards alone; it may narrow by `window` (the title of the
    window acted in, the dialog's own for a dialog), `control` (the UI
    Automation type), `text` (a line of a small dialog's text, its
    question) and `session` (the session's name, so a pattern can be
    for one task). Text is matched whole, ignoring case and runs of
    spaces, with `*` for any run. Unknown fields fail the file, since a
    misspelt one would match more than meant. The MCP process sends the
    action in parts (`act`) beside its sentence, and the app matches
    them; every action a pattern lets through is logged as "operate from
    the list, pre-approved: PATTERN". The log names the window and a
    small dialog's question beside the action, so the human can write a
    pattern from a line of it. `horadric desktop approve|unapprove`.
  - Sysmac Studio's "Transfer..." stays risky: one menu item for both
    directions. A direction is pre-approved for one task by a pattern
    with `session`, on the direction's own button, and with the menu
    item too when that task should not be asked at all.
- **Seen on the stage.** Every operate action shows a notification naming
  the session and the program, so the human always knows an agent has
  their program in hand.

## Open questions

- A frame drawn around the element acted on, like the agent's cursor's
  arrow, so the human sees where in the program it is.
- Whether answered grants should outlive Horadric, per project and
  program. The list above is the ahead of time answer for every project.
- A page in the app's settings for the list, beside the command.
- The elevated helper still needs Windows' UAC yes when it starts, unless
  the machine elevates without asking, which an unattended machine has
  to settle on its own. A helper started once per Horadric rather than
  once per `horadric mcp` would ask the human once a day instead of once
  a session, at the cost of one elevated process serving every session.
- Whether a stopped helper (silent past 150 s) can always be stopped:
  the handle Windows gives back for an elevated start may not allow
  `TerminateProcess`, in which case cutting the pipe ends it only once it
  is out of what it hung in.
