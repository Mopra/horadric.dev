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
  `desktop_type` and `desktop_press`. A ref acts through UIA patterns
  first (invoke, toggle, select, expand, set value) and falls back to a
  real click or keystrokes, after which the human's pointer is put back.
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
  the same pipe.
- **Seen on the stage.** Every operate action shows a notification naming
  the session and the program, so the human always knows an agent has
  their program in hand.

## Open questions

- A frame drawn around the element acted on, like the agent's cursor's
  arrow, so the human sees where in the program it is.
- Whether grants should outlive Horadric, per project and program.
