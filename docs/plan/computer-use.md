# Computer use

A session can use the PC beyond a browser page: mouse, keyboard and
screen, for any app. Windows only. There are two ways, and both stay.

| | The agent's desk | Takeover |
|---|---|---|
| Where the agent works | A second Windows session of the same user, shown in a Horadric window | The human's own desktop, in the program's real window |
| What the human gives up meanwhile | The monitor the desk is on. Their cursor, focus, keyboard and clipboard stay theirs | The program in hand, and the pointer and keyboard for the moment of each click or key |
| The permission | The human opens the desk from the tray. While it is open every session may use it | The tray's "Takeover" is "Off" or "Ask before each program". Then per session and program, look or operate, asked in a dialog or listed ahead of time. Risky actions are asked each time |
| What the human sees | The desk in its window, like a screen beside their own | The session's rune travelling to each click before it is made, a soft light along the screen's edges, and a plate saying what the agent does and why |
| How the human stops it | Close the desk's window | Hold Esc |
| The tools | `desk_*` | `takeover_*` |
| The plan | This page | [takeover.md](takeover.md) |

The rule that joins them:

- **The desk is the default.** It is for working beside the human, who
  loses nothing while the agent works.
- **Takeover is for what only works on the human's own desktop**: a
  program that is already open there, one that runs as administrator,
  one bound to hardware or a licence.
- **A closed desk is never a reason to switch.** When the desk is
  closed, the agent says so and asks the human to open it. It does not
  reach for the human's own desktop in its place.

The rest of this page is the desk.

## Why a second Windows session

One Windows login has one cursor and one keyboard focus. An agent that
clicks with `SendInput` on the human's desktop moves their mouse and
takes focus from what they type, on whichever monitor. Driving apps
without the pointer (UI Automation, posted messages) was looked at and
turned down as the base: many apps come to the front when invoked, and
posted input fails silently in Chromium and Electron apps. Takeover
does click on the human's desktop, and pays for it: it borrows the
pointer for each click and asks before it acts.

So for work beside the human the agent gets a Windows session of its own, for the same user: a
child session (`WTSEnableChildSessions`). Windows gives it its own
cursor, focus, keyboard and clipboard, and it has the human's files,
apps and logins. Horadric shows it in a window on the monitor the human
gives it. This is the agent's desk.

Tested on Windows 11 Home, build 26200, on 2026-11-26 (lore #799 and
#800 have the detail):

- Child sessions can be switched on, once, with admin rights.
- `mstsc /child` is not accepted on this build. Hosting the Remote
  Desktop control (CLSID `A0C63C30-F08D-4AB4-907C-34905D770C7D`) with
  the extended property `ConnectToChildSession` works.
- The session has its own explorer, dwm and GPU driver processes.
- Clicks and keys sent inside the child session reach its apps, and the
  human's cursor and focus do not move.
- Edge, the new Outlook, Teams, Spotify and VS Code run in both
  sessions at once. Chrome hands the start to the copy in the human's
  session, classic Outlook hangs on its locked data file.
- A background process can not take focus there: click a window before
  typing into it.
- The user's autostart programs all start in the child session too.
- A scheduled task starts in the human's session, not the child's. An
  HKCU RunOnce value runs at the child's sign-in.
- With "only allow Windows Hello sign-in" on, every password is refused
  (reason 2055). The user name that works for a Microsoft account is
  the plain email.

## The three parts

What they agree on (ports, file names, the credential's name) is in
`horadric_core::desk`.

### The window (`horadric_ui::desk::window`)

A window of Horadric's that hosts the Remote Desktop control and
connects it to the child session.

- It fills the monitor the human gave the desk, at that monitor's
  resolution, and remembers the monitor. With one monitor it is an
  ordinary window.
- The sign-in is kept in Credential Manager under
  `desk::credential_target`. With none kept the human is asked once,
  with Windows' own credential prompt, on a thread of its own so the
  rest of Horadric goes on. What was typed is kept when the control
  says the login is complete, not when it is connected, so a wrong
  password is never stored. A refused sign-in says why in plain words,
  the Windows Hello setting among the reasons.
- Child sessions are switched on by `horadric desk enable`, which the
  window starts elevated when they are off.
- Before connecting it sets the RunOnce value `desk::RUN_ONCE` to
  `"<this exe>" desk hand`, so the hand starts at the desk's sign-in.
  When a hand already answers, the session is still signed in and
  nothing is set. A value nobody ran is taken back, and one left by a
  Horadric that died is removed at the next start.
- A desk that shows but has no hand is told to the human, with "End
  the desk" as the way to a new one: 10 seconds after coming back to a
  session that was signed in, 2 minutes after a fresh sign-in. The
  window never signs the session out by itself, the agent's apps may
  be open there.
- Once connected it writes the child session's id to
  `desk::SESSION_FILE` in the state folder, and removes the file when
  the desk is closed.
- Closing the window disconnects and leaves the session signed in, so
  the agent's apps stay open. "End the desk" signs it out. A desk asked
  for while it closes opens again a second after the disconnect: a
  connection made in the same moment is ended by the session.
- The human can click into the window and use the desk themselves.

### The hand (`horadric_ui::desk::hand`, `horadric desk hand`)

Horadric's helper inside the desk. It has no window.

- It runs only in a session that is not the console's and that
  `desk::SESSION_FILE` names (it waits a few seconds for the file). The
  file counts only when it was written since Windows started, so one
  left by a crash does not make an ordinary Remote Desktop login the
  desk.
- It serves the screen share's protocol for its own session on
  `desk::port`, with a token in `desk::TOKEN_FILE`: frames and input.
- `GET /apps`: the windows open on the desk, and the apps the human has
  open in their own session.
- `POST /launch`: starts an app on the desk. The twin rule
  (`horadric_core::desk_apps`) answers for an app that can not run
  twice: Chrome open at the human's becomes Edge, classic Outlook
  becomes the new Outlook, and so on. An app with no twin is refused
  with the reason. The rule holds for a path in every form that names
  a known app (no `.exe`, quotes, a shortcut, its `shell:AppsFolder`
  id), and a page (a web or `file:` address, an `.html`, a `.pdf`)
  always opens in Edge, never in the default browser. An app that
  shares something else with the human's session comes with a note:
  Notepad opens with their unsaved tabs. Horadric itself is refused, by
  name or by path (`desk_apps::own_refusal`): a Horadric on the desk
  could be driven to answer its own questions.
- While the window is closed `/apps` and `/launch` answer that the desk
  is closed and do nothing.
- At start it stops the autostart programs that came up in its session
  (the Run keys and the Startup folder), since the desk is a workplace
  and not a second copy of the human's tray. It stops for good at the
  first start asked of it and at the first key or mouse input on the
  desk, and never stops an app of the twin table.
- A full Horadric that autostarts inside the desk exits quietly.

### The tools (`desk_` in `horadric mcp`)

For sessions Horadric started, beside `browser_` and `remote_`:

- `desk_screenshot`: the desk as a picture, with the scale to multiply
  image coordinates by. `zoom` for a region at full resolution.
- `desk_click`, `desk_type`, `desk_press`, `desk_scroll`: as the
  `remote_` tools, whose code they share.
- `desk_apps`, `desk_open`: what is open, and starting an app.

They work only while the desk is open: the human opening it is the
permission. With it closed a tool says so and does nothing, also while
the session is still signed in and its hand still answers: a tool asks
for `desk::SESSION_FILE` before it reaches for the hand.

## Where it stands

The desk ran end to end on a dev instance on 2026-11-26 (lore #805):
it opened, the hand answered, and every `desk_` tool did its work
through `horadric mcp`. Not yet seen: the hand without its terminal
window on the desk, and a frame asked for while the desk closes, which
once answered an error without saying why (lore #803).

## Later

- Takeover is built, as a mode of its own: see [takeover.md](takeover.md).
  This plan once said it would pause the moment the human touches mouse
  or keyboard. What is built is different: a held Esc stops the agent's
  action, and a click is refused while the human holds a mouse button.
  Pausing on the human's touch is still an open idea, listed there.
- Host takeover's engine in the desk's hand, so the desk gets snapshots
  with refs, changes and wait, for accuracy and fewer screenshots.
- Lock, sleep and reboot: whether the session survives, and reconnecting
  without the password.
- Other editions and a work account (Entra) were not tested.
