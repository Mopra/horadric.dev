# The companion

Asked for on 2026-11-26: an assistant that is always on and follows the
human's work across the whole computer, not only inside Horadric. It
sits beside them like a colleague who has been watching over their
shoulder. The human can say "take this over" or "do that" at any moment
and it already knows the context. Later it also helps unasked: it fills
in text, suggests changes and catches what the human missed.

Nothing of it is built.

## Why Horadric

Others are building the same thing. Google's Gemini Proactive Assistance
and Magic Cue read the screen and notifications on a phone. Adsideo does
it on a Mac. Screenpipe and Microsoft Recall record the screen on the PC
but mostly look back, they do not step in. ProAgentBench (arXiv
2602.04482) and PASK (arXiv 2604.08000) are the research on when such an
agent should speak.

Horadric already has most of the pieces:

- Dictation types into any program and draws a plate by the caret
  (`dictate/anywhere.rs`, `dictate/plate.rs`). Suggested text can be
  shown the same way.
- Memory finds things by meaning (`horadric-memory`), and the lore shows
  how sessions ask for what was found before.
- The desk is a Windows desktop of the agent's own (see
  [computer use](computer-use.md)). Work the human hands over can be
  done there without taking their mouse.
- Horadric already knows what every Claude session is doing.

What is missing is what the human does outside Horadric.

## Principles

- **Local first.** What is captured stays on this machine, in the state
  folder. Nothing is sent anywhere until a step below says so, and then
  only the part a call needs.
- **Cheap capture, rare calls.** Sending the screen to Claude all the
  time costs too much. Capture uses what Windows gives for free, and
  Claude is called only when something happens.
- **Quiet.** Interruptions decide whether this is a help or a nuisance.
  It never takes focus and never pops a window up unasked. A suggestion
  is grey text the human can ignore by typing on.
- **Never watched.** Password fields, private browser windows, the lock
  screen and a list of programs and sites the human picks. A pause key
  and a mark in the tray show at a glance whether it watches.
- **This is not session state.** The rule that a session's state comes
  from hooks, never from parsing terminal output, still holds. The
  companion reads the human's screen, a new kind of input, and nothing
  it reads changes a tile.

## Steps

Each step is useful on its own, and each is tried before the next is
started.

### 1. The journal

A rolling local log of what the human works on:

- The foreground program, its window title, and when it changed
  (`SetWinEventHook` for EVENT_SYSTEM_FOREGROUND and name changes).
- The text the focused window exposes through UI Automation: the
  document, the mail being written, the page in the browser. Read when
  focus or content settles, not on every key.
- Windows' own OCR (`Windows.Media.Ocr`) as the fallback for programs
  that expose no text, run only on a settled window, never on a timer.
- Kept for a set time (seven days to start), then deleted. Summaries
  older than that may be kept by the memory store.

Both UI Automation and the OCR come through the `windows` crate already
in use, with more features, so no new dependency.

### 2. Ask it

Sessions can ask the journal. An MCP tool like the lore's
(`journal_recall`: the last N minutes, or a question found by meaning)
answers with what the human was doing. Any session, a quest included,
can then start from "what was I doing in Outlook ten minutes ago" or
"the error on the page I had open".

### 3. Take over

A key (and a dictated sentence) hands the current thing to the
companion: "answer this mail", "finish this table", "fix what this page
says". A session starts with the last minutes of the journal already in
its prompt. Work in other programs is done on the desk; the human is
shown what will change before it reaches their own windows.

### 4. Quiet suggestions

When the human stops typing in a text field, the companion may offer to
finish the sentence or the field as grey text by the caret, taken with
Tab. Nothing else unasked at this step.

This step needs a filter that decides, many times a minute, whether a
call is worth making. See the first open decision.

### 5. Catching misses

Only once step 4 has proved not to annoy: a mail that mentions an
attachment but has none, a date that does not match the calendar, a
form sent with a field left empty. Shown as one small mark the human
can open, never a popup.

## Decided

On 2026-11-26, by the human:

- **A small local model is welcome.** The filter in step 4 runs on a
  local model, so the many small "is this worth a call?" checks cost
  nothing. Which model is chosen when step 4 starts, with numbers from
  steps 1 to 3. This reverses the 2026-11-05 drop of small local
  models, for the companion.
- **All work is watched, OptiPeople's included.** Nothing is left out by
  default except what is never watched (above).
- **No daily cost cap.** Instead the companion pauses itself when the
  subscription's usage reaches 80 percent, and tells the human with a
  toast. It starts again when the usage window resets, or when the human
  unpauses it.

## Open decisions

- **Audio.** Meetings and calls are out of scope until the human asks.
- **How long the journal is kept**, and whether it is ever kept in the
  Drive backup (default: never).
