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

Since 2026-11-08: four models to pick from in Settings, Dictation, with
the language (English or Danish) told to a multilingual one, and a plate
by the caret when Ctrl+Alt+D dictates into another program.

## Measured 2026-11-08

Release build, i7-11700K, the example (`examples/dictate.rs`) on Windows
TTS recordings, the second run timed. A 15 second sentence:

| Model | Fetch | Time | Heard |
|---|---|---|---|
| distil-small.en (default) | 332 MB | 1.9 s | "pain", "Maine", "Warwick" for pane, main, Warriv |
| distil-large-v3 | 1.5 GB | 6.5 s | "main", "folio's quest log"; still "pain" |
| whisper-small | 970 MB | 2.6 s | "main", "warriv", "anywhere.rs": best on names |
| whisper-large-v3-turbo | 1.6 GB | 7.0 s | about as distil-large-v3 |

On 25 s of Danish read aloud, whisper-small took 5.1 s with a slip in
most lines, turbo 12.6 s and nearly every word right.

distil-large-v3 is not the default: three and a half times the wait for
no clear gain on these. The encoder in eight bits was tried and is far
slower on candle's CPU path (nine times, and f16 a fifth slower), so the
encoder stays in full floats; the large ones hold about 3 GB while
loaded.

## Next

- **Hear it from a real microphone.** Every number above is from TTS.
  Try whisper-small for English as well: it was the best on project
  names, at 1.4 times the wait.
- **Quicker large models.** The encoder is the time. Ways left: a
  shorter window still (it is the speech plus two seconds), or the GPU.
- **macOS.** The microphone is written (`mac/microphone.rs`, not yet
  built on a Mac); the stage still needs the key, the draft and the
  ticks, and the tidy needs `claude` found the Mac's way.
