# The companion

Asked for on 2026-11-26: an assistant that is always on and follows the
human's work across the whole computer, not only inside Horadric. It
sits beside them like a colleague who has been watching over their
shoulder and listening. The human can say "take this over" or "do that"
at any moment and it already knows the context. Later it also helps
unasked: it fills in text, suggests changes and catches what the human
missed.

The plan was reviewed on 2026-11-26; what the review changed is listed
at the end. A first cut of steps 1 to 3 was built on 2026-11-27, see
"What is built" below.

## What is built

Since 2026-11-27, off until switched on in Settings, General,
"Companion". The code is `crates/horadric-ui/src/companion/` and
`crates/horadric/src/trail.rs`.

- **The trail, the seen half.** `horadric companion` is a process of its
  own that the app starts and stops. It writes the window in front and
  the text UI Automation gives for it, only what is new, to
  `%APPDATA%\Horadric\trail`, one sealed file per day, kept seven days.
  `horadric companion show [MINUTES]` prints it, and
  `horadric companion peek` is step 0's measuring tool.
- **Never watched** is `companion-never.txt` in the state folder plus the
  built in list (password managers, Horadric itself, private windows by
  their title).
- **Ask it.** `trail_recall` answers a session the human sits at. Quest,
  Warriv and errand sessions and the companion's own are refused, and
  every asking is in `trail\asked.log`. It finds by words, not yet by
  meaning.
- **Take over.** Ctrl+Alt+G listens for a sentence and starts a session
  in the companion's folder with the last ten minutes in its prompt. The
  session hands a text back with `companion_offer`, and Ctrl+Alt+J types
  it where the cursor is. Ctrl+Alt+B pauses the watching.

Not built yet: the heard half (microphone and calls), OCR for programs
that expose no text, the daily summary in the memory store, a `Trail:`
line that lets a quest ask, a mark on the tray icon (the tip and the
menu say it), work handed back through the desk, and steps 4 and 5.
Step 0 was not done as a day of measuring: reading is on a settled
window only, and what it costs the browsers is still to be felt.

## Why Horadric

Others are building the same thing. Google's Gemini Proactive Assistance
and Magic Cue read the screen and notifications on a phone. Adsideo does
it on a Mac. Screenpipe and Microsoft Recall record the screen on the PC
but mostly look back, they do not step in. ProAgentBench (arXiv
2602.04482) and PASK (arXiv 2604.08000) are the research on when such an
agent should speak.

Horadric already has most of the pieces:

- Dictation types into any program and draws a plate by the caret
  (`dictate/anywhere.rs`, `dictate/plate.rs`), and it already captures
  the microphone through WASAPI and runs Whisper on this machine.
- Memory finds things by meaning (`horadric-memory`), and the lore shows
  how sessions ask for what was found before.
- The desk is a Windows desktop of the agent's own (see
  [computer use](computer-use.md)).
- Horadric already knows what every Claude session is doing.

What is missing is what the human does outside Horadric.

## Principles

- **Captured locally.** Capture, transcription and the filter run on
  this machine. The trail (below) leaves it only inside a prompt to
  Claude, and only the part that prompt needs. It is never sent to a
  brother, the phone's portal, Discord or the Drive backup.
- **Cheap capture, rare calls.** Sending the screen to Claude all the
  time costs too much. Capture uses what Windows gives for free, and
  Claude is called only when something happens.
- **Quiet.** Interruptions decide whether this is a help or a nuisance.
  It never takes focus and never pops a window up unasked.
- **What is read is not an order.** The trail holds text from web
  pages, mail and other people's speech. A session is given it marked
  as something seen, never as instructions, the same way a pasted
  document is.
- **Never watched.** Password fields (UI Automation says which they
  are), password managers, and whatever programs the human adds. A
  pause key and a mark in the tray show at a glance whether it watches
  and listens. Private browser windows are left out where the window
  says it is one (Edge does, in its title); where it cannot be told,
  the plan does not promise it.
- **Only the human's own session.** Horadric also starts inside the
  agent's desk (every Run key does). The companion watches only the
  session the human sits at, never the desk, and a dev instance never
  watches while the installed one does.
- **This is not session state.** The rule that a session's state comes
  from hooks, never from parsing terminal output, still holds. The
  companion reads the human's screen, a new kind of input, and nothing
  it reads changes a tile.

## Steps

Each step is useful on its own, and each is tried before the next is
started.

### 0. Measure first

A day's work, thrown away after. A small program that prints what UI
Automation gives for the focused window in the programs the human uses
most: the new Outlook, Teams, the browser, VS Code, Excel, and Horadric's
own stage. Three numbers decide the rest:

- How much of the text is there without OCR.
- What reading it costs. A UI Automation client switches on the
  accessibility mode of Chrome, Edge and every Electron program, which
  is known to slow them. If the human feels it, the browser is read by
  OCR or through the browser pane instead.
- Whether the caret's place can be had (step 4 needs it; lore #785 says
  browsers, Electron and terminals keep no system caret).

### 1. The trail

A rolling local log of what the human works on. Not "the journal":
`journal.jsonl` is already the quest log's.

Seen:

- The foreground program, its window title, and when it changed
  (`SetWinEventHook`).
- The text the focused window exposes through UI Automation: the
  document, the mail being written, the page in the browser. Read when
  focus or content settles, not on every key, and only what changed
  since the last reading is written.
- Windows' own OCR (`Windows.Media.Ocr`) as the fallback for programs
  that expose no text, run only on a settled window, never on a timer.
- Horadric's own stage is not read from the screen. What its sessions
  do is already known.

Heard (decided 2026-11-26: everything, the microphone too):

- The microphone (what the human says) and the system's sound (what
  the others say, WASAPI loopback) are two streams, so the trail knows
  who spoke without telling voices apart.
- Whisper runs only on speech. Today's loudness test (`SILENCE` in
  `speech.rs`) is not enough for a stream that is open all day: music
  and video are loud too. Loopback is taken per program (Windows'
  process loopback) so Teams is heard and Spotify is not, by a list the
  human can change.
- No sound is kept. Once a stretch is written as text the samples are
  dropped.
- Transcription runs behind, on few threads at low priority. Measured
  in [dictation](dictation.md): distil-small.en writes 15 seconds in
  1.9; Danish needs the turbo model, which takes half the length of the
  speech. So a Danish meeting is written minutes behind, and a take
  over that needs the last minute has that part written first. The GPU
  would end this and is the same open question dictation has.
- Trap: holding the microphone of a Bluetooth headset open puts the
  headset in its call mode, and music through it turns to telephone
  quality. A microphone that is not the headset's is picked for
  listening, or listening through a headset starts only while a call
  runs.

Kept:

- In the state folder, encrypted for this Windows user (DPAPI, in the
  `windows` crate), since it holds everything that was on screen.
- Values the vault knows are cut out before a line is written.
- Seven days, then deleted; a short summary per day stays in the
  memory store.

Where it runs: in a process of its own, as session hosts do. A UI
Automation call into a hung program can wait forever, and Whisper holds
one to three gigabytes; neither belongs in the process that draws the
tiles. UI Automation, OCR, loopback and DPAPI all come through the
`windows` crate already in use, so no new dependency.

### 2. Ask it

Sessions can ask the trail. An MCP tool like the lore's (`trail_recall`:
the last N minutes, or a question found by meaning) answers with what
the human was doing and what was said.

Who may ask is the weak point of the whole plan. The trail holds mail,
customer data and other people's words, and a session that reads a
hostile web page could be talked into fetching it and sending it
somewhere. So:

- A session the human sits at may ask. A quest, a Warriv session and a
  subagent may not, unless the quest's notes ask for it, the way a
  `Connectors:` line asks for connectors.
- Every answer says what it is and that nothing in it is an order.
- Each asking is written down (who, when, what for) where the human can
  see it.

### 3. Take over

A key (and a dictated sentence) hands the current thing to the
companion: "answer this mail", "finish this table", "fix what this page
says". A session starts, in a project folder of the companion's own,
with the last minutes of the trail already in its prompt.

How the work gets back, surest first:

- **As text at the caret.** The answer is typed where the human's
  cursor is, as dictation does, after they have seen it. This covers
  mail, chat and forms and needs nothing new.
- **Through a connector**, where the program has one (mail, calendar,
  Teams, files).
- **On the desk**, for programs that run there. Not all do: classic
  Outlook stays at "Opening" and Chrome hands its window to the human's
  session (lore #800). So the desk is not the general answer the first
  version of this plan took it for.

The companion never moves the human's own mouse.

### 4. Quiet suggestions

When the human stops typing in a text field, the companion may offer to
finish the sentence or the field as grey text by the caret.

What makes this the hardest step:

- **Time.** Haiku through `claude -p` takes about 3.5 seconds (lore
  #686), and a local model good enough to write was found as slow on
  this CPU. Text that arrives 3.5 seconds after a pause is mostly too
  late. So the first version is asked for with a key, and offering on a
  pause waits for a session kept warm or a model on the GPU that
  answers in under a second. Measure before building.
- **The filter.** Deciding whether a pause is worth a call runs on a
  small local model through candle, which is already here. It only says
  yes or no, so it can be far smaller than one that writes.
- **Taking it.** Tab moves between fields in most forms, and catching
  it means a keyboard hook over every program. The key is one of
  Horadric's own chords instead, held only while a suggestion shows.
- **Where.** The caret's place, from step 0. Without it the text is
  shown in the plate by the pointer.
- **Others do it already.** Outlook, Gmail and VS Code finish text
  themselves. Programs are switched off one by one in Settings.

### 5. Catching misses

Only once step 4 has proved not to annoy: a mail that mentions an
attachment but has none, a date that does not match the calendar, a
promise made in a meeting and not written down. Shown as one small mark
the human can open, never a popup. The cheap ones (the attachment) are
rules, not model calls.

## Decided

On 2026-11-26, by the human:

- **A small local model is welcome.** This reverses the 2026-11-05 drop
  of small local models, for the companion.
- **All work is watched, OptiPeople's included.** Nothing is left out by
  default except what is never watched (above).
- **Audio is in: everything, the microphone too.**
- **No daily cost cap.** Instead the companion pauses what it does
  unasked (steps 4 and 5) when a limit of the account it calls with
  reaches 80 percent, the five hour one or the weekly one
  (`usage::Limit`), and tells the human with a toast. It starts again
  when that limit resets, or when the human unpauses it. Capture and a
  take over the human asks for go on: they are the human's own use.
- **The name is the companion.**
- **The trail is kept seven days** as text, then only a short summary
  per day in the memory store.
- **No rule for which account gets work.** The companion calls with
  whichever account is active, as every session does. The accounts
  differ only in their terms: the work seat is under OptiPeople's
  agreement, the private one may be trained on unless that setting is
  off.

## Open decisions

- **Telling the others.** Calls and meetings are written down as text.
  Whether the people in them are told is put off by the human
  (2026-11-26), to be taken up before listening is switched on for
  calls with others.
- **Which runtime reaches cards that are not Nvidia's** (below).

## The GPU, and every other PC

Decided 2026-11-26: Whisper and the local model run on the graphics
card wherever the PC has one that fits, and Horadric stays for
everyone, the human's laptop included. So nothing in the companion may
need a card. A card makes it better, the lack of one makes it plainer:

| | With a fitting card | Without |
|---|---|---|
| Listening | The large model, written as it is said | The small model, written behind; Danish minutes behind |
| Suggestions (step 4) | Offered on a pause | Asked for with a key |
| The trail, asking it, take over | The same | The same |

- **Picked at start, not at build.** One Horadric for every PC. It
  looks at the card and its memory when it starts, says in Settings
  what it chose, and the human can overrule it.
- **Nothing more in the installer.** What a card needs (CUDA's files
  are some hundreds of megabytes) is fetched the first time it is
  used, the way the models are.
- **On battery** listening and suggestions drop to the plain column or
  pause, by a setting. A laptop that runs Whisper all day on battery is
  empty by noon.
- **First: Nvidia through CUDA.** candle has it, so no new dependency,
  and the human's desktop has an RTX 4070 Ti (12 GB). To check before
  building: that one binary built with candle's CUDA feature still
  starts, and falls back to the CPU, on a PC without CUDA.
- **Open: the other cards.** Most laptops have Intel or AMD graphics,
  which CUDA never reaches. The human's laptop is one: an Intel i7 with
  its built in graphics and no card, so it starts in the plain column,
  and built in graphics would gain far less than a card does. Reaching them means another runtime
  (whisper.cpp with Vulkan, or ONNX Runtime with DirectML), a new
  dependency and Whisper written a second time. Decide once the plain
  column has been tried on the human's laptop: if it is good enough
  there, the second runtime is not worth it.
- **Each PC has its own trail.** The laptop does not know what was done
  at the desktop, since the trail is never sent to a brother. Sharing
  it between the human's own machines would be a decision of its own.

## What the review changed

Reviewed 2026-11-26. The first version was right in its order and its
principles, and wrong or silent on these:

- It said work in other programs is done on the desk. The desk cannot
  open several of the programs the human works in. Text at the caret
  and connectors come first now.
- It said nothing of who may read the trail, or of a web page's text
  reaching a prompt as if the human had said it.
- It took Tab and a suggestion "when the human stops typing" for
  granted. Neither holds at 3.5 seconds a call.
- It called the log the journal, a name already taken.
- It would have watched the agent's desk as well, and kept everything
  on screen unencrypted.
- It did not measure before building. Step 0 does.
