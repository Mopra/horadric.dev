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
(conversation and files) and steering or queueing mid turn are built.

- **Edit and resend** any earlier prompt: a fork at that message
  (`--fork-session`, `--resume-session-at`), the old branch kept, both
  browsable. Files rewound with `rewind_files`, so a rewind can go to
  any earlier point, not only the last turn.
- **Review before it lands:** an edit mode where each Edit or Write
  comes as a diff card to accept, change or reject, through
  `can_use_tool` and `updatedInput`. Line comments on a diff belong here.
- **The work as a map:** the steps of a turn as a timeline, the files it
  touched lit in the Arcane Sanctuary as they happen.
- **Many sessions, one view:** a prompt sent to several sessions at
  once, answers side by side.

## Smaller features still missing

Surveyed 2026-10-29 from Claude Code's desktop app, its VS Code
extension, the CLI's interactive mode and T3 Code. The rule: simple,
usable, fast. T3 Code's feel comes from rules more than tricks: a pure
model, small payloads, nothing that repaints continuously, no spinner
that lies.

- **API errors as notices of their own.** A login failure shows the
  sign-in card and a retry shows as a notice, but a rate limit and other
  API errors still show as the CLI's reply text (the tile does show the
  error).
- **Soon:** `!` shell mode run by Horadric; a context breakdown (the
  categories `get_context_usage` answers are parsed into `ContextUsage`
  but not drawn); prompt suggestions; session titles; timestamps and a
  verbose view; images in tool results (only a prompt's images are
  drawn); `/btw`.
- **Later:** per-turn git checkpoints (T3 Code's way), worktree per
  session with PR status, fast mode and output style as controls (the
  styles are parsed from `initialize`), an external editor, LaTeX.
- **Terminal only, on purpose:** voice.

## Open question

- **The protocol moving.** An unknown message is kept and shown raw,
  and features are detected by `capabilities`. Not done: a stream that
  fails to parse at all falls back to a terminal.
