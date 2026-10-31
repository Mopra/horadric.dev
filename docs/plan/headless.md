# Headless sessions and the Folio

Decided on 2026-10-24. A first slice landed 2026-10-29: the engine,
the model and a plain Folio behind a button on each project cluster
(see Steps). Since 2026-10-31 new Claude sessions open as a Folio by
default, those automation starts too (step 8). A Claude session's pane
shows Claude Code's terminal today: PowerShell output, tool calls and
diffs scroll past in full, take most of the room, and are rarely what
the human wants to read. The human wants Horadric to own the whole
experience, "the freedom to truly transform how we develop with AI".

**The decision.** Claude sessions run headless: Horadric starts the
installed, unmodified `claude.exe` with its stream-json protocol on
pipes, no terminal, and draws the conversation itself in a view called
the Folio. Not the Agent SDK: it is TypeScript or Python, so a runtime
beside the Rust app, and its terms forbid offering subscription login
in a product built on it. Driving the CLI directly is the same agent
loop, in Rust, on the human's own login.

**The grey zone, accepted.** Anthropic's legal page says developers
building products that interact with Claude, including on the Agent
SDK, should use API keys, and it explicitly allows a user signing in to
the unmodified Claude Code binary with their own subscription. A
headless CLI on the user's own login sits between those two. Horadric is
free, built for the human, friends and colleagues, and the human accepts
the risk: if Anthropic objects, we revert to terminal sessions and say
sorry. So terminal sessions stay working beside headless ones, never
deleted, and switching back is one setting. Sources:
[legal and compliance](https://code.claude.com/docs/en/legal-and-compliance),
[Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview).

**The settled rule still holds.** State comes from hook events, never
from parsing terminal output. The stream is not terminal output: it is
structured messages the program sends on purpose, like OSC 7501. Hooks
still fire headless (the project's and user's settings load unless
`--bare`, which we never pass) and still drive the phase. Where hooks
and the stream both speak, hooks win, as today.

## The protocol, as far as it is known

From the official docs and the Python Agent SDK's source, read
2026-10-24 against Claude Code 2.1.310. The stdout message types and
user message fields are documented. The control protocol
(`control_request`, `control_response`, `initialize`, `can_use_tool`)
is not formally specified: it is what the SDK speaks, and can change
without notice. Hence step 1 records real streams and every later step
tests against them.

- **Start.** `claude.exe --input-format stream-json --output-format
  stream-json --verbose --include-partial-messages
  --permission-prompt-tool stdio`, plus `--resume=<id>`,
  `--session-id=<uuid>`, `--model`, `--permission-mode`,
  `--append-system-prompt`, `--mcp-config` as today's terminal launch
  passes them. Always the `=` form for values. No `-p`: confirmed
  against 2.1.312, the flags alone keep a session up across turns.
  `claude.exe` directly, never a `.cmd` shim. The environment drops
  `CLAUDECODE`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and the
  `CLAUDE_CODE_*` variables naming a parent session, so the subscription
  login is what it uses (init says `apiKeySource: "none"`, confirmed).
  No `initialize` is needed for `can_use_tool` to arrive. No
  `--permission-mode` either: a session follows the human's own
  settings (the human's is `auto`, which never asks; decided
  2026-10-29), so the ask cards show only when settings do ask.
- **In, one JSON object a line.** A prompt is
  `{"type":"user","message":{"role":"user","content":...}}`, content a
  string or blocks (images as base64 blocks), with `uuid`, `priority`
  (`now`, `next`, `later`) and `client_composed`. Control requests:
  `initialize`, `interrupt`, `set_permission_mode`, `set_model`,
  `get_context_usage`, `rewind_files`, `stop_task`, `mcp_status`,
  `mcp_reconnect`, `mcp_toggle`, `mcp_authenticate` (confirmed
  2026-10-31 against 2.1.314; see `folio/mcp.rs` in core).
- **Out.** `system`/`init` first (session id, model, tools, slash
  commands, skills, agents, MCP servers, `capabilities`), then
  `assistant` messages (text, thinking, `tool_use` blocks), `user`
  messages carrying `tool_result`, `stream_event` token deltas, and
  one `result` per turn (cost, usage, duration, errors,
  `terminal_reason`). Also `compact_boundary`, `status`,
  `conversation_reset` after `/clear`, task events for background work,
  and `control_request`s from the CLI: `can_use_tool` and
  `hook_callback`, answered with a `control_response`, and
  `elicitation` for an MCP server's form or page.
- **Asks.** Every permission prompt is a `can_use_tool` request.
  AskUserQuestion is one too: allow it with `updatedInput` carrying
  `answers`. ExitPlanMode most likely is, to be confirmed. Always echo
  `updatedInput` on allow.
- **Slash commands** go as plain user text: custom commands, skills,
  `/compact`, `/clear`, `/model sonnet`, `/effort`. Pickers and
  `/login`, `/theme` do not exist headless.
- **Login** (2026-10-31, 2.1.314). `claude auth login` works with no
  console: on pipes it opens the browser itself, listens on localhost,
  and prints `If the browser didn't open, visit: <url>` and `Paste code
  here if prompted > ` (no line end). The browser coming back finishes
  it; the printed page instead shows a `code#state` to write on stdin.
  `Login successful.` and exit 0, or `Login failed: …` on stderr. It
  writes the same files as any login, so account switching is
  untouched. `claude auth status --json` says `loggedIn`, `authMethod`,
  `email`, `subscriptionType`. The Folio runs both hidden; see
  `folio/sign_in.rs` in core and ui.
- **One process, many turns.** Stdin stays open for the session;
  closing it ends it. A prompt sent mid turn with `next` is folded
  into the running turn after its next tool step (or starts the next
  turn if none comes); `now` stops the turn and starts one with it.
  Stdout is read on its own thread so the pipe never fills; lines can be large (tool results, images), so no 1 MB
  cap.

The full research brief, with every source, is in the lore (note left
on 2026-10-24).

## The shape

- **The host speaks for the session.** Each session already runs in a
  `horadric-host` process that outlives the UI. A headless session's
  host starts `claude.exe` on pipes instead of a ConPTY, owns the
  protocol, and keeps the session's messages since it started. A UI
  that attaches (after a reload or a crash) gets the transcript from
  disk plus the host's live tail, so nothing is lost, as a terminal's
  screen replay works today. The pipe protocol between UI and host gains
  message frames beside the byte stream.
- **A pure model in `horadric-core`.** `folio.rs`: messages in, a list
  of blocks out (prompt, reply, thinking, a run of tool steps, a diff,
  an ask, a turn's footer). Every rule of what folds into what lives
  here and is tested against recorded streams, with no window.
- **The view in `horadric-ui`.** The Folio draws in the stage pane where
  the terminal is, Direct2D and DirectWrite as everything else. It
  scrolls, selects and copies, finds text, and opens files and diffs in
  the file viewer.
- **The phase** still comes from hooks. The stream adds what hooks never
  carried: tokens as they come, context used, the plan.

## The Folio

The conversation as pages, not a log.

- **Your prompt** as a heading-like block, with any images.
- **Claude's reply** rendered: paragraphs, headings, lists, tables,
  code blocks with the file viewer's highlighting, links. Streams in as
  it is written.
- **Thinking** folded to one line, open on a click.
- **Tool work** folded into one line per run of steps: "Read 4 files ·
  Searched twice · Ran tests ✓ · Edited pane.rs +12 −3". A click opens
  the steps; a failed one opens by itself. A shell command shows its
  command and the last lines of output, never all of it.
- **Diffs** as a list of files changed with their counts. A click shows
  the diff inline; another opens the file.
- **Subagents** as lanes of their own under the step that started them,
  folded the same way, from `parent_tool_use_id`.
- **Asks** as cards with buttons: allow once, always, deny with a
  reason; questions as their options; a plan as a page to approve or
  send back. The phone's Town Portal answers the same pending request.
- **Each turn's footer:** time, tokens, context left. No dollar cost:
  on a subscription the API-price figure is not what the human pays,
  so it was dropped.
- **The composer** at the bottom: multi line, slash commands and skills
  from init in a list as you type `/`, `@` for files, images pasted,
  Esc interrupts, Enter while it works steers (sent at once, `next`),
  Ctrl+Enter stops the turn for it (`now`).
  Model and permission mode as controls on it, sent as control requests.

The rune ring, the burning letters and the rest of the glass's light
carry over: the composer is where typing burns in.

## What the terminal gave for free, rebuilt

- `/model`, effort and permission mode pickers: controls on the
  composer.
- `/login` and `/logout`: run by the Folio itself, see Login above.
- `/config`, `/theme` and other terminal only commands: a
  "Open in terminal" action ends the headless process once idle and
  resumes the same session in a terminal pane (`--resume`); "Back to the
  Folio" does the reverse. One session, two ways in, never both at
  once.
- Ctrl+O's transcript and verbose mode: every block opens in place.
- The folder trust dialog: Horadric asks before the first session in a
  folder, since headless runs a project's hooks and MCP servers without
  asking.
- The status line: the footer and the tile get model, cost and context
  from the stream.
- The mod's band (2026-10-31, against 2.1.314). Headless the mod loads
  and all of it runs but `ui.render`: `session.start` says `surface:
  null, isInteractive: false`, its clock ticks, the roster and spawns
  post, `/horadric/know` is asked every 3 s and the overlap note reaches
  Claude beside a prompt. Only the band above the prompt is never drawn,
  so the Folio draws it above its composer (`folio/band.rs`), from what
  the app would answer the mod, every second.

## What it opens

What a terminal could never do, the reason for all this.

- **Edit and resend** any earlier prompt: a fork at that message
  (`--fork-session`, `--resume-session-at`), the old branch kept, both
  browsable. Files rewound with `rewind_files`.
- **Review before it lands:** an edit mode where each Edit or Write
  comes as a diff card to accept, change or reject, through
  `can_use_tool` and `updatedInput`.
- **Steer mid turn** without stopping it, and queue the next prompts
  while it works.
- **The work as a map:** the steps of a turn as a timeline, the files it
  touched lit in the Arcane Sanctuary as they happen.
- **Many sessions, one view:** a prompt sent to several sessions at
  once, answers side by side.

These come after the Folio works; each gets its own line here when it is
next.

**Markdown is our own** (decided 2026-10-25): no `pulldown-cmark`. The
human wants as few dependencies as possible, and a reply needs only a
subset: paragraphs, headings, lists, code blocks and inline code,
emphasis, links, tables, quotes. A pure parser in `horadric-core`,
tested against replies from the fixtures; anything it does not know
shows as plain text.

## Steps

Done so far (2026-10-29): step 1 except the resume checks and
ExitPlanMode (`Headless` in `horadric-pty`, `horadric headless-debug`,
five fixtures), step 2, step 3, step 4 and a first step 6. A Folio is
a session kind: a tile of its own (its hooks find it by
`HORADRIC_SESSION`, as a terminal's do), a pane in the stage's grid,
several per project, its host reattached after a reload or a crash.
The setting "New Claude sessions open as a chat" (inverted since, see
step 7) made the new session button, a resumed tile and the History
menu start Folios. "Terminal"
on the Folio and "Open as chat" on a Claude terminal's tile hand one
session between the two in the same tile. The confirmed protocol is in
the lore.

1. **Spike.** A host that starts headless `claude.exe`, a hidden debug
   command that sends a prompt and prints the stream. Confirm: no `-p`
   needed or the flag it does need, subscription login with
   `apiKeySource: "none"`, hooks fire and the tile's phase follows,
   `can_use_tool` for a Bash ask, AskUserQuestion, ExitPlanMode,
   interrupt, resume of a session started in a terminal and the reverse.
   Record each stream as a fixture.
2. **The model.** `folio.rs` and its tests against the fixtures.
3. **The host.** Headless sessions in hosts that outlive the UI, with
   replay on attach. Verify a reload mid turn loses nothing, and count
   `claude.exe` after.
4. **The Folio, plain.** Prompt, reply as plain text, tool runs folded,
   the composer, interrupt, asks as cards. A setting, off by default,
   makes new Claude sessions headless.
5. **The Folio, rich.** Markdown, code highlighting, diffs, subagent
   lanes, images, slash command list, model and mode controls, footer.
6. **Open in terminal and back.**
7. **Default on** for new Claude sessions. Done 2026-10-31: the
   setting is now "New Claude sessions open in a terminal", off. Every
   way the human starts a Claude session follows it (the cluster's
   button, the project menu, the tray, the folder picker, a bare
   `horadric new`, Explorer). "New CLI session" in the project menu is
   always a terminal. Since 2026-10-31 a Folio starts with everything a
   terminal session gets (`App::claude_start` builds both, and
   `App::start_session` takes a spec for either): own worktrees, with
   their ports and setup, and the town open as Folios, and a first
   prompt goes in as the first message.
8. **Automation on Folios.** Done 2026-10-31: quests (taken by hand or
   by the runner, in their own worktree when the project runs several),
   the Quest Giver, Warriv and its reviewer, a runeword's reviewer, the
   Runesmith, errands, stones cast on a fresh session and a quest log's
   Carry on all start through `start_session` as Folios, unless the
   setting says terminal: the one switch back covers automation too.
   They are driven through `steering.rs` (a prompt, a command, a stop,
   a halt), so nothing types bytes. A paused one resumes as a Folio, its
   role rebuilt from its id and the list by `claude_start`. What stays a
   terminal: a runeword whose `keys` include one a Folio cannot take
   (an arrow, Tab, text no Enter ends), see `runeword::needs_terminal`;
   cast on a Folio such a step stops the runeword and says why. A
   `run` step shown in a pane, the bare `--resume` picker and `horadric
   new` with a name or arguments stay terminals too. Account switching
   winds a Folio down at rest (or at once, or when a limit refused it)
   and starts it again on the same conversation with the new login,
   the go on prompt as its first message. A worktree Horadric just made
   of a trusted project is trusted, so an unattended quest never waits
   on the Folio's trust ask. An automated start that fails says so in a
   toast or the log, never a dialog.

Then "What it opens", one by one. The full feature list toward
replacing the CLI, in tiers, is [folio-features.md](folio-features.md).

## Open questions

- **The Brotherhood.** A headless session handed to another machine
  goes by its transcript, as today, and resumes headless there.
- **The protocol moving.** Feature detection by `capabilities`, and a
  check at start: an unknown message type is kept and shown raw, never
  dropped, and a stream that fails to parse falls back to a terminal.
