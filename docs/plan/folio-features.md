# The Folio's features, toward replacing the CLI

Surveyed 2026-10-29 from Claude Code's desktop app, its VS Code
extension, the CLI's interactive mode and T3 Code. Part of
[headless.md](headless.md). The rule: simple, usable, fast. T3 Code's
feel comes from rules more than tricks: a pure model, small payloads,
nothing that repaints continuously, no spinner that lies.

**[T]** is terminal only, served by "Open in terminal". **verify**
means a message the SDK names but our fixtures have not confirmed.

## A. Must have to replace the CLI

- A1 Sessions survive reload and crash: the host owns claude.exe and replays on attach.
- A2 Markdown replies (our own parser), code highlighting, copy a code block or a reply.
- A3 Resume and history: past sessions of the project, opened with their transcript drawn.
- A4 Permission card: allow once, allow always (`permission_suggestions`), deny with a reason.
- A5 AskUserQuestion card: options, multi select, free text.
- A6 Plan mode: the plan as a page to approve or send back (ExitPlanMode, verify).
- A7 Composer controls: permission mode (Shift+Tab cycles), model, effort.
- A8 Diffs for Edit, Write, MultiEdit: files with counts, a click opens the diff.
- A9 Shell steps: command, last lines, expandable, exit state.
- A10 Composer basics: multi line, Up/Down history, image paste, file drop, big paste folded.
- A11 Slash menu on `/`: commands, skills, agents from init.
- A12 `@` file completion.
- A13 Steer: Enter while busy goes to the running turn at its next step, Ctrl+Enter stops the turn for it.
- A14 Todo checklist above the composer.
- A15 Context meter, compaction marker, `/clear` reset.
- A16 Subagent lanes under the step that started them.
- A17 Errors plainly: API retry, rate limit, auth, failed turn. Auth
  done (2026-10-31): a turn refused for want of a login (assistant
  `error: authentication_failed`, or an `api_error` with a 401) shows a
  sign-in card instead of the CLI's "run /login" reply. `/login` and
  `/logout` run in the Folio: `claude auth login` hidden on pipes, the
  card with "Open sign-in page" (the default browser) and a field for
  the code the page shows, then the session starts again on its
  conversation with the new login. Rate limit and other API errors
  still show as the CLI's reply text.
- A18 Find, select and copy across blocks, jump between prompts.
- A19 Open in terminal and back.
- A20 Needs-you markers on tile and tab.

## B. Soon

Rewind (`rewind_files`, `--resume-session-at`), edit and resend as a
fork, background tasks panel with stop, `!` shell mode run by Horadric,
context breakdown, local command output as cards, MCP status card, line
comments on a diff, review each edit before it lands, prompt
suggestions, drafts kept per session, session titles, timestamps and a
verbose view, images in tool results, usage limits meter, `/btw`.

## C. Later

Turn timeline and files lit in the Arcane Sanctuary, one prompt to
several sessions, per-turn git checkpoints (T3 Code's way), worktree
per session with PR status, fast mode and output style, external
editor, LaTeX.

## Terminal only

MCP OAuth and voice. Every `/` command the CLI answers with "isn't
available in this environment" over stream-json is native in the Folio
(`folio/slash.rs`): `/memory` and `/agents` open their files in the
viewer, `/hooks` lists them, `/permissions` adds and takes out rules in
`.claude/settings.local.json`, `/plugin` runs `claude plugin` hidden,
`/theme` opens Settings, `/resume` lists History, `/export` writes
markdown, `/add-dir` starts the Folio again with the folder, `/exit`
ends it. `/config` and `/doctor` work in the CLI itself; `/ide` and
`/terminal-setup` are not offered.
