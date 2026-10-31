# Headless sessions and the Folio

Claude sessions run headless (the installed `claude.exe` on stream-json
pipes, no SDK) and Horadric draws them as the Folio. That is built and
is the default for every new Claude session, automation included: the
engine, the host, the model, the rich Folio, open in terminal and back,
sign-in, MCP, the native slash commands, and every Horadric feature
reaching a Folio. Why headless and not the SDK, the grey zone the human
accepted, the confirmed protocol and the traps are in the lore; ask
`lore_recall` about the Folio, headless or a file under `folio/`.

What still holds for the work below: terminal sessions are never
deleted, and the setting "New Claude sessions open in a terminal"
switches everything back in one place, in case Anthropic objects. Hooks
still drive the phase and win where they and the stream both speak.

## What it opens

What a terminal could never do, the reason for all this. Undo last turn
(conversation and files), steering or queueing mid turn, a turn's map
(its footer opens a timeline of its steps and the files it touched),
`/all` (one prompt to every Folio of the project), edit and resend or
rewind to any earlier prompt as a fork, and reviewing each edit before
it lands are built.

- **Line comments on a diff** under review: today a reviewed edit is
  allowed, changed as a whole, or denied with one reason.

## Smaller features still missing

Surveyed 2026-10-29 from Claude Code's desktop app, its VS Code
extension, the CLI's interactive mode and T3 Code. The rule: simple,
usable, fast. T3 Code's feel comes from rules more than tricks: a pure
model, small payloads, nothing that repaints continuously, no spinner
that lies.

- **Later:** per-turn git checkpoints (T3 Code's way), worktree per
  session with PR status, fast mode and output style as controls (the
  styles are parsed from `initialize`), an external editor, LaTeX.
- **Terminal only, on purpose:** voice.

## Open question

- **The protocol moving.** An unknown message is kept and shown raw,
  features are detected by `capabilities`, and a stream that fails to
  parse at all goes on in a terminal. Still open: a stream that parses
  but means something new.
