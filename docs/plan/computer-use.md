# Computer use: the agent's desk

A session can drive a browser page today, and nothing else on the PC.
This gives it the whole PC: mouse, keyboard and screen, for any app.
And it does so beside the human, not in their place: the agent works on
one monitor while the human keeps working on the other.

## Why a second Windows session

One Windows login has one cursor and one keyboard focus. An agent that
clicks with `SendInput` on the human's desktop moves their mouse and
takes focus from what they type, on whichever monitor. Driving apps
without the pointer (UI Automation, posted messages) was looked at and
turned down as the base: many apps come to the front when invoked, and
posted input fails silently in Chromium and Electron apps.

So the agent gets a Windows session of its own, for the same user: a
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
  with Windows' own credential prompt. A refused sign-in says why in
  plain words, the Windows Hello setting among the reasons.
- Child sessions are switched on by `horadric desk enable`, which the
  window starts elevated when they are off.
- Before connecting it sets the RunOnce value `desk::RUN_ONCE` to
  `"<this exe>" desk hand`, so the hand starts at the desk's sign-in.
  When a hand already answers, the session is still signed in and
  nothing is set.
- Once connected it writes the child session's id to
  `desk::SESSION_FILE` in the state folder, and removes the file when
  the desk is closed.
- Closing the window disconnects and leaves the session signed in, so
  the agent's apps stay open. "End the desk" signs it out.
- The human can click into the window and use the desk themselves.

### The hand (`horadric_ui::desk::hand`, `horadric desk hand`)

Horadric's helper inside the desk. It has no window.

- It runs only in a session that is not the console's and that
  `desk::SESSION_FILE` names (it waits a few seconds for the file).
- It serves the screen share's protocol for its own session on
  `desk::port`, with a token in `desk::TOKEN_FILE`: frames and input.
- `GET /apps`: the windows open on the desk, and the apps the human has
  open in their own session.
- `POST /launch`: starts an app on the desk. The twin rule
  (`horadric_core::desk_apps`) answers for an app that can not run
  twice: Chrome open at the human's becomes Edge, classic Outlook
  becomes the new Outlook, and so on. An app with no twin is refused
  with the reason.
- At start it stops the autostart programs that came up in its session
  (the Run keys and the Startup folder), since the desk is a workplace
  and not a second copy of the human's tray.
- A full Horadric that autostarts inside the desk exits quietly.

### The tools (`desk_` in `horadric mcp`)

For sessions Horadric started, beside `browser_` and `remote_`:

- `desk_screenshot`: the desk as a picture, with the scale to multiply
  image coordinates by. `zoom` for a region at full resolution.
- `desk_click`, `desk_type`, `desk_press`, `desk_scroll`: as the
  `remote_` tools, whose code they share.
- `desk_apps`, `desk_open`: what is open, and starting an app.

They work only while the desk is open: the human opening it is the
permission. With it closed a tool says so and does nothing.

## Later

- Takeover: the same tools on the human's own desktop, on one monitor,
  for an app that only works there. It pauses the moment the human
  touches mouse or keyboard on that monitor.
- A snapshot of the desk's UI Automation tree with refs, like
  `browser_snapshot`, for accuracy and fewer screenshots.
- Lock, sleep and reboot: whether the session survives, and reconnecting
  without the password.
- Other editions and a work account (Entra) were not tested.
