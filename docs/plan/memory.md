# Memory

The long term memory (`crates/horadric-memory`, the app's memory thread
in `horadric-ui/src/memory.rs`) is built. What was decided, measured and
found while building it is in the lore; ask `lore_recall`. This page
holds only what is still not done.

## Not done

- **The bit vector prefilter.** Search scans every int8 vector. Once the
  store is big (hundreds of thousands of chunks), scan 96 byte bit
  vectors by Hamming distance first and score only the best few thousand
  in int8.
- **A gentler pace for background embedding.** It embeds two chunks at a
  time with a 100 ms rest, using every core while it runs. Fine for
  notes; the backlog of transcript chunks waiting for vectors wants a
  pace that leaves the sessions their CPU.
- **Redaction does not know the vault's own secrets.** `redact` catches
  token shapes and secret-sounding names, but not the values kept in the
  vault (see [secrets.md](secrets.md)). It should cut those too.
- **Compaction of old code chunks.** A code chunk of a file that changed
  or went is no longer an answer, but it stays in the store and is still
  embedded. A compaction should drop them.
- **Nothing distills the day's dialogue any more.** Warriv's daily look
  back handed it each day's passages to keep as notes; it went with the
  rounds (2026-10-30). A note names passages as sources only when a
  session chooses to.
- **The yardstick has no validity questions.** Nothing measures recall
  of replaced notes, `history`, or "what is true now" against "what was
  said then".
- **A deleted worktree's dialogue keeps its own key.** A transcript's
  project is the one git names for its folder; a worktree since deleted
  falls back to its own folder, so its dialogue is not found under the
  main project.
- **The model choice may reopen.** EmbeddingGemma beat MiniLM on dialogue
  alone (81.8 % against 72.7 %); its code was removed, so a switch means
  writing it again.
