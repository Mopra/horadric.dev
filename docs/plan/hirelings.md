# The hirelings: seeing subagents work

Built 2026-10-30: the band in `horadric-core/src/hireling.rs`, the runes
on the tile in `render.rs`, the retinue and the war table in
`horadric-ui/src/retinue.rs`, a hireling's pane in `folio/watch.rs`. The
lore has how and why; the research behind it is in this file's git
history. What is left:

- **Keys into the retinue.** Alt+Down to the first portrait, arrows
  between them, Enter to open. Only the mouse opens one now.
- **The count on the tile opens the war table.** Now only Ctrl+Shift+W in
  the pane or the glyph atop the retinue's column do.
- **Hovering a tick** on the war table for its tool and what it was
  given. A track's hover says only the hireling's summary.
- **Checked on screen.** The runes' size on the lamp, the portraits in
  a short pane, the table with ten tracks.

## Open questions

- **Typing to a hireling.** Claude Code lets you message a background
  agent from its panel. From outside, a terminal session has no way in
  but the main prompt. For Folio sessions stream-json may allow it.
  Read only until we know.
- **Agent teams.** Teammates are sessions of their own in Claude Code.
  Are they hirelings on the parent's tile or tiles of their own? Leaning
  to hirelings, with `teammate_id` to tell them apart.
- **How far to nest.** A hireling that spawns hirelings: indent in the
  war table, a `+N` on the parent's portrait, and only the first level
  gets runes on the tile.
