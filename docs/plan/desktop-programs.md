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
- **Seen on the stage.** Every operate action shows a notification naming
  the session and the program, so the human always knows an agent has
  their program in hand.

## Open questions

- A frame drawn around the element acted on, like the agent's cursor's
  arrow, so the human sees where in the program it is.
- Whether grants should outlive Horadric, per project and program.
