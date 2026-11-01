# Dictation

Asked for on 2026-10-30: the human dictates prompts with Wispr Flow all
day, and Horadric could do it itself, into its own inputs, with a model
that runs on the machine.

Built: Ctrl+Space dictates with distil-small.en through candle into a
stage pane, a terminal session or the Folio, live as it is said, with a
microphone picked in Settings. Since 2026-11-01 also: a cached, eight bit
decoder (twice as fast on long phrases), drafts that settle
(LocalAgreement), a tick at start and stop, a personal word list, an
opt in Haiku tidy, both chords in Settings, Keys, and Ctrl+Alt+D typing
into any program. How it works, what was measured and why are in the
lore (ask about `dictate.rs`, `anywhere.rs` and `speech.rs`). What
follows is what is left.

## Next

- **A bigger model.** The decoder is no longer the slow half; the
  encoder is (1.3 s of 2.3 s on 16.7 s of speech). distil-large-v3
  hears better and may take a longer prompt without looping, but its
  encoder is about three times the work: try it once quantized, or with
  the encoder in eight bits.
- **Danish.** A multilingual model, about twice the size, picked in the
  Dictation section.
- **Seen outside Horadric.** Dictating into another program shows
  nothing but the ticks: a small plate near the caret with the level and
  the draft, as Wispr has.
- **macOS.** The microphone is written (`mac/microphone.rs`, not yet
  built on a Mac); the stage still needs the key, the draft and the
  ticks, and the tidy needs `claude` found the Mac's way.
