# Signals from the agent

From the research pass of 2026-10-24: what Claude Code already sends
beside the hooks. Each adds to the hooks, never replaces them. Hooks win
where both speak.

Built on 2026-11-01, and in the lore: cost per session and per project
on the menus, OpenTelemetry's `api_request` events received as OTLP JSON
with the last turn's cost on the tile menu, OSC 9;4 progress read beside
OSC 7501 and shown on the stage's taskbar button, and `horadric run`
given the status line. Decided then, and why, in the lore too: no
`refreshInterval`, and no `statusLine` written into the user's settings.

## What is left

- **A look on screen.** The tile menu's cost lines, the project menu's
  sum, and the taskbar button while a build in a plain terminal sends
  OSC 9;4. In PowerShell 7, ``Write-Host -NoNewline "`e]9;4;1;40`a"``
  shows 40 %, `9;4;2` red, `9;4;4` yellow and `9;4;0` clears it.
- **macOS.** The Mac tile and project menus have no cost lines yet, and
  the Dock has no progress from OSC 9;4.
