# Dictation

Asked for on 2026-10-30: the human dictates prompts with Wispr Flow all
day, and Horadric could do it itself, into its own inputs, with a model
that runs on the machine.

## What is built

**Ctrl+Space** in a stage pane, a terminal session or the Folio, starts
listening. Tapped, it listens until the next Ctrl+Space. Held, it
listens until it is let go. Esc drops what was heard. The words are
typed where the caret is, never sent: the human reads them and presses
Enter. A terminal's header says "Listening" where its title would be,
the Folio says it in the composer's notice.

- `horadric-memory/src/speech.rs`: Whisper through candle-transformers,
  which the memory already depends on, so no new crate. The model is
  `distil-whisper/distil-small.en`, 332 MB, pinned by commit and hash
  and fetched on first use into `models\distil-small.en` the same way
  as the memory's model. The mel filter bank is computed, not shipped,
  and matches Whisper's own to 1e-9. Greedy decoding, no timestamps.
  Silence below an RMS of 0.004 returns nothing, since Whisper given
  silence writes "Thank you.".
- `horadric-ui/src/dictate.rs`: the microphone through WASAPI in shared
  mode, asked for 16 kHz mono floats so Windows does the resampling.
  One thread per recording, one that keeps the model loaded between
  dictations. Recordings stop by themselves after three minutes.
- `pane.rs`: the chord, the release, Esc and `WM_DICTATE`.
- `examples/dictate.rs`: transcribes a WAV without the app.
- The workspace builds candle and gemm optimised in dev builds, or a
  dev instance takes minutes per sentence.

Measured on this machine (2026-10-30), Windows text to speech, 7.7 s:
2.3 s to write it down, the words right but for "pains" and
"horridrick". The window handed to the encoder is the speech plus two
seconds, not Whisper's full 30: same words, half the time.

Why Ctrl+Space: it types nothing in a terminal that a session needs
(it is NUL, which no agent CLI binds), it is one hand, and Ctrl+Alt+Space
is already the global "next waiting session". It is fixed for now.

## Next

- **Faster.** 2.3 s for a sentence is usable, not Wispr. Options in
  order of cost: write down while the human speaks, a window at a time,
  so only the last few seconds wait; a key value cache in the decoder
  (candle's Whisper reruns every token each step); a quantized model
  (candle has `quantized_model`, needs a GGUF of the weights).
- **Feedback.** An animated rune on the pane while it listens, not only
  words in the header, and a soft tick at start and stop. Runes, not
  glows.
- **Clean up.** Wispr's edge is an LLM pass: filler words, "scratch
  that", lists. A small local model or Haiku, behind a switch.
- **Words it should know.** Whisper takes a prompt of earlier text: the
  project's name, Horadric's own words (Folio, Warriv, Tristram) and the
  session's last lines would fix "horridrick".
- **Settings.** The chord in the Settings window beside the global
  shortcuts, a microphone picker, Danish (a multilingual model, about
  twice the size).
- **Outside Horadric.** A global chord that types into whatever app has
  focus, as Wispr does. Needs a low level keyboard hook for hold to
  talk and SendInput or the clipboard to type. Bigger, later.
- **macOS.** AVAudioEngine for the microphone; the model side is shared.
