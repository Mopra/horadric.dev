# Dictation

Asked for on 2026-10-30: the human dictates prompts with Wispr Flow all
day, and Horadric could do it itself, into its own inputs, with a model
that runs on the machine.

## What is built

**Ctrl+Space** in a stage pane, a terminal session or the Folio, starts
listening. Tapped, it listens until the next Ctrl+Space. Held, it
listens until it is let go. Fifteen seconds without a word ends it.
Esc drops what was heard, while listening or while the last words are
written. The words are typed where the caret is, never sent: the human
reads them and presses Enter. The Folio's composer also has a
microphone key left of send, which does what a tap does.

The words show while they are said, dimmed until final, as Claude
Code's dictation does. In the Folio they stand at the caret in the
composer; a terminal cannot show text that is not typed, so its header
shows the end of them with a block letter bar for the microphone's
level. In the Folio the level is a disc that swells inside the
microphone key. A dead microphone shows before anything is said.

How it is live with a model that does not stream: the recording is cut
into phrases at each pause of 0.6 s (or at 20 s). A phrase that ended
is written down for good while the next is spoken, and the phrase being
spoken is written again about every half second for the draft, skipped
while the writer is still busy. Letting go leaves only the last phrase
to write, about a second for a few seconds of speech however long the
dictation. Speech is told from the room by its loudness over a noise
floor that drops at once and rises slowly, so a fan's hum is not
speech; the cutting is `Ear` in `dictate.rs`, tested without a
microphone.

When nothing is written it says why, by the microphone's name: "No
sound came from Line In" for a dead or muted input, "Heard no speech"
for a quiet one. **Settings, General, Microphone** picks the input,
Windows' default otherwise; one unplugged falls back to the default.

The first press on a machine without the model does not listen: it
fetches the model, says why in the notice (or the header) with the MB
counting up and, in the Folio, a thin bar under it, then loads it and
says "Dictation is ready" for eight seconds. Speech recorded before
that would wait minutes for a 332 MB download, or be lost if it failed.

- `horadric-memory/src/speech.rs`: Whisper through candle-transformers,
  which the memory already depends on, so no new crate. The model is
  `distil-whisper/distil-small.en`, 332 MB, pinned by commit and hash
  and fetched on first use into `models\distil-small.en` the same way
  as the memory's model. The mel filter bank is computed, not shipped,
  and matches Whisper's own to 1e-9. Greedy decoding, no timestamps.
  Silence below an RMS of 0.004 returns nothing, since Whisper given
  silence writes "Thank you.". A run of tokens said three times over
  (one token five times), or more tokens than ten a second of audio, is
  a loop: it is written again without the prompt, and a loop left then
  is cut after its first round.
- The prompt Whisper reads as earlier text is short: Claude, the
  project's folder and branch, the end of what is typed before the
  caret, and the phrases already said. Tried 2026-10-30 with Claude's
  last reply in it as well, the way Aqua Voice reads the screen: at 64
  tokens and more distil-small.en looped on a phrase or dropped a whole
  sentence, and short prompts changed no word. So the reply stays out
  until the model changes.
- `horadric-ui/src/dictate.rs`: the microphone through WASAPI in shared
  mode, asked for 16 kHz mono floats so Windows does the resampling.
  One thread per recording, one that keeps the model loaded between
  dictations. Recordings stop by themselves after three minutes. The
  microphone is the one picked in Settings, listed and named through
  `IMMDeviceEnumerator` and the endpoint's friendly name.
- `pane.rs`: the chord, the release, Esc and `WM_DICTATE`.
- `examples/dictate.rs`: transcribes a WAV without the app.
- The workspace builds candle and gemm optimised in dev builds, or a
  dev instance takes minutes per sentence.

Measured on this machine (2026-10-30), Windows text to speech, release
build: 4.4 s of speech in 1.0 s, 15.4 s in 4.1 s, the words right but
for "pains", "carrot" and "recizes". The window handed to the encoder
is the speech plus two seconds, not Whisper's full 30: same words, half
the time.

Why Ctrl+Space: it types nothing in a terminal that a session needs
(it is NUL, which no agent CLI binds), it is one hand, and Ctrl+Alt+Space
is already the global "next waiting session". It is fixed for now.

## Next

- **Faster drafts.** A key value cache in the decoder (candle's
  Whisper reruns every token each step) or a quantized model (candle has
  `quantized_model`, needs a GGUF of the weights). Faster also means a
  bigger model fits: distil-large-v3 hears better and may take a
  longer prompt without looping.
- **Draft that settles.** LocalAgreement (whisper_streaming): words two
  drafts agree on stop changing, so only the tail flickers.
- **Feedback.** A soft tick at start and stop.
- **Clean up.** Wispr's edge is an LLM pass: filler words, "scratch
  that", lists. A small local model or Haiku, behind a switch.
- **Words it should know.** The prompt did not fix "pains" or
  "horridrick". A personal dictionary as Wispr has, applied to the
  words after Whisper, is surer: "pains" to "panes" in this project.
- **Settings.** The chord in the Settings window beside the global
  shortcuts, and Danish (a multilingual model, about twice the size).
- **Outside Horadric.** A global chord that types into whatever app has
  focus, as Wispr does. Needs a low level keyboard hook for hold to
  talk and SendInput or the clipboard to type. Bigger, later.
- **macOS.** AVAudioEngine for the microphone; the model side is shared.
