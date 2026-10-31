# Dictation

Asked for on 2026-10-30: the human dictates prompts with Wispr Flow all
day, and Horadric could do it itself, into its own inputs, with a model
that runs on the machine.

Built: Ctrl+Space dictates with distil-small.en through candle into a
stage pane, a terminal session or the Folio, live as it is said, with a
microphone picked in Settings. How it works, what was measured and why
the prompt stays short are in the lore (ask about `dictate.rs` and
`speech.rs`). What follows is what is left.

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
