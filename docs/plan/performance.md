# Performance

Three passes (2026-09-26, 2026-10-01, 2026-10-13) took the UI thread,
the stage panes and the browser's memory down; what they fixed and the
numbers are in the lore (`lore_recall performance`). What is left:

- **The tile shows a session waiting on heavy.** `horadric heavy` lets
  one test suite or full build run at a time on the machine, and tells
  the waiter whose command it waits for. The tile says nothing, so a
  session queued for the slot looks stuck.
- **More than one slot.** The slot is always one. A machine with cores
  to spare could allow two, chosen by core count or a setting.
