# The hirelings: seeing subagents work

Proposed 2026-10-30. A session that sends out subagents should show
them the way Claude Code's agent panel does, and better: each one at a
glance on the tile, a row per subagent on the stage you can step into
and read live, and one view that shows the whole run over time. Named
for the hirelings of Act II, who fight beside you on your errand.

## What the others do

Researched 2026-10-30, sources at the end.

- **Claude Code CLI.** A tool row per spawn (`Explore(find hooks)`) that
  closes to `Done (12 tool uses · 86.4k tokens · 1m 13s)`. Background
  agents get rows in a panel under the prompt, nested ones as a tree
  with `(+N)`. Arrows select, Enter opens the live transcript and lets
  you type to it, Esc returns. A finished agent leaves at once, a failed
  one stays 30 s. A permission prompt from a background agent names the
  agent. Most requested missing piece: a running count you can see
  without opening anything (#27916), and tokens per agent.
- **Codex.** `/subagents` switches threads in the CLI. The app has a
  read only panel, Active and Done, with name, elapsed and a summary.
  The top complaint is that you cannot talk to a child.
- **Warp.** Pills above the parent's view, one per child, each with a
  status badge. A pill switches the pane to the child and a breadcrumb
  takes you back. It admits the parent can say success while a child
  failed.
- **Grok Build dashboard.** Rows sorted by what needs you: waiting on
  input first, then working, then idle. Children roll up under the parent.
- **Factory Mission Control.** A dashboard: elapsed and credits on top,
  workers on the left, a live log of every worker on the right.
- **Zed, Amp, OpenCode, T3 Code.** Collapsed cards inside the thread
  (Zed), only a summary back to the parent (Amp), leader key plus arrows
  between parent and children (OpenCode), a list with tokens but not the
  task it was given (T3 users ask for that).
- **Outside coding.** The GitHub Actions graph (boxes, a spinner, then a
  check and a duration). Jaeger's trace waterfall (nested bars on one
  time axis, so overlap and the slow path show at a glance). RimWorld's
  colonist bar (a row of tiny portraits, each with an icon for what it
  is doing).

What is worth taking: children belong to the parent and are never new
sessions. Show the task each one was given. One line of size
(tools, tokens, time). Step in, step back. Sort by what needs you. Roll
status up honestly. And no one has a good picture of a run over time.
That is where Horadric can be the nice one.

## What Horadric has today

- The tile shows `subagent_line` ("Explore x2, Plan") in its bottom row
  while they work, one orbiting spark per subagent, and a Subagents
  submenu that stops one.
- The mod sends `HoradricSpawn` (id, type, description, model, parent,
  workflow) and a `HoradricAgents` roster every 2 s (status, description,
  parent, start). `HookEvent` drops description, model, parent and
  workflow on parse, and the roster drops `teammate_id`.
- Every hook fired inside a subagent carries `agent_id`, but a
  subagent's `PreToolUse` overwrites the session's one `tool`, so the
  tile cannot say whose tool it is.
- Subagent transcripts (`<session>/subagents/agent-<id>.jsonl`) are only
  read afterwards, for spend and files.
- The Folio already builds a `Lane` per subagent from
  `parent_tool_use_id`, with steps, tokens, tool uses and progress.
- Pane headers show nothing about subagents.

## The proposal

Three places, from a glance to the full story.

### 1. On the tile: a rune per hireling

The orbiting sparks become runes, one per subagent, each in a glyph
chosen by its type (Explore, Plan, general purpose, a custom agent's
name hashed to one of the twelve). This follows the rule of runes, not
glows. The rune carries the state:

- **working:** orbits the lamp, its stroke drawing on and off;
- **waiting** on a permission: stops at the top of the orbit and beats;
- **done:** slows, settles into the lamp and fades, over about two seconds;
- **failed or killed:** stops where it is, cracked, in the error colour,
  and stays until the tile is clicked or 30 s pass.

The top row gains a count beside the name: `3 ⁄ 4` hirelings working.
Hovering the tile lists them, one line each:
`Explore · find hook events · Grep · 0:42 · 18k`.

The tile's own state rolls up honestly. A session whose main turn
ended but has a hireling running looks working (calmer, as
`AGENTS_ALONE` does now). One whose hireling failed shows the cracked
rune even when the main turn reports success.

### 2. On the stage: the retinue and stepping in

A **retinue strip** along the bottom of a session's pane, just above
the input, one row per hireling, like Claude Code's panel but always
visible while there is anything to show:

```
 ᚱ Explore   find hook events in horadric-hooks   Grep        0:42   18k
 ᛟ Plan      design the hirelings view            Read        1:10   31k
 ᚦ general   research Codex subagent UI           ✓ 21 tools  1:46   71k
```

Sorted by what needs you: waiting first, then failed, working, done.
Done rows fold into one line after 30 s ("2 done"), failed stay. Nested
ones indent under their parent (`parent_id`). It takes at most four
rows and scrolls. A pane header shows `3 hirelings` when the strip is
folded away.

**Click a row** (or Alt+↓, then arrows) and the pane steps into that
hireling: its transcript, read live from its `.jsonl` and drawn by the
Folio's renderer, read only. The pane header becomes a breadcrumb,
`session › Explore: find hook events`, the parent's terminal keeps
running underneath. **Esc** or a click on the breadcrumb goes back. Stop
is a button in the breadcrumb, as the tile menu does now.

For a Folio session the same row opens the `Lane` it already has, full
height, instead of reading a file.

A permission prompt from a hireling, in either kind of session, names
it: `Explore wants to run Bash`.

### 3. The war table: the run over time

The view nobody else has. Press `W` on a pane (or click the count on
the tile) and the pane turns into a waterfall of the turn, Jaeger style,
on one time axis:

```
 main      ████▌·················································████
 Explore      ├──▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮──┤ ✓
 Plan         ├──▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮──┤ ✓
 general      ├──▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮░░░  ᚦ
```

Each bar is made of its tool calls as ticks, coloured by kind (read,
search, edit, shell, web). A running bar's front edge is a rune drawing
itself, the runes drifting off it as the Folio's backdrop does. The
gaps show where the main turn sat waiting. Hover a tick for the tool
and its input. Click a bar to step into that hireling as above. It
answers the questions a list cannot: which ran in parallel, which one
everybody waited for, where the time went.

The war table works for any turn, with or without hirelings, so it also
becomes the way to see what a long solo turn did.

## Data work underneath

1. **Keep what the mod sends.** Add `description`, `model`, `parent_id`,
   `workflow_run` to `HookEvent` and `teammate_id` to `ModAgent`.
2. **A tool per agent.** Keep `tool` per `agent_id` in the session, not
   one shared slot, so the main loop's tool and each hireling's are
   separate. The hook events already say whose they are.
3. **Register `SubagentStart` and `SubagentStop`** too. Stop brings
   `agent_transcript_path` and `last_assistant_message`, and it works
   when the mod is not loaded. The mod still wins where both speak,
   because it has status and a description.
4. **Tail hireling transcripts while they run.** One reader per running
   hireling, on the listener side, following the file as the Arcane
   Sanctuary follows writes: it gives tools used, tokens, the current
   tool and the steps for stepping in. Stops reading when the hireling
   ends. A transcript is the agent's own record, not terminal output, so
   it keeps the rule.
5. **Per hireling timing** (start, each tool's start and end) kept in
   memory for the session's last few turns, for the war table. Not saved.
6. **Folio:** pass `--forward-subagent-text`, so lanes have the
   hireling's words as well as its tools.

Codex and Grok: Codex has subagents but sends no hooks for them that
we know of. Start with Claude, and show the others' as a count only
once their hooks say anything.

## Steps

1. Data: items 1 to 3 above, and the honest roll up on the tile.
2. Runes per hireling on the tile, the count, the hover list.
3. The retinue strip on the stage, from the roster and per agent tools.
4. Transcript tailing, and stepping into a hireling with the
   breadcrumb. Folio sessions open their lane.
5. The war table.

Each step is usable alone. 1 to 3 already beat what Codex shows; 4
matches Claude Code; 5 is ours.

## Open questions

- **Typing to a hireling.** Claude Code lets you message a background
  agent from its panel. From outside, a terminal session has no way in
  but the main prompt. For Folio sessions stream-json may allow it.
  Read only until we know.
- **Agent teams.** Teammates are sessions of their own in Claude Code.
  Are they hirelings on the parent's tile or tiles of their own? Leaning
  to hirelings, with `teammate_id` to tell them apart.
- **How far to nest.** A hireling that spawns hirelings: indent in the
  strip and the war table, but only the first level gets runes on the
  tile.

## Sources

- Claude Code: code.claude.com/docs/en/sub-agents, /agent-teams, /agent-view, /hooks, /desktop; github.com/anthropics/claude-code/issues/27916
- Codex: learn.chatgpt.com/docs/agent-configuration/subagents; github.com/openai/codex/issues/34591
- Warp: docs.warp.dev/platform/managing-cloud-agents
- Grok Build: dev.to/akaranjkar08/grok-build-agent-dashboard-run-8-parallel-coding-agents-from-one-screen-32bf (one secondary source)
- Factory: docs.factory.com/missions/running-app.md
- Zed: github.com/zed-industries/zed/discussions/60099
- Amp: ampcode.com/news/agents-everywhere
- OpenCode: github.com/anomalyco/opencode/issues/15223
- T3 Code: github.com/pingdotgg/t3code/discussions/10507
- Jaeger: jaegertracing.io/docs/latest/frontend-ui
