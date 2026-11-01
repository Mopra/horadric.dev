# Memory

The long term memory (`crates/horadric-memory`, the app's memory thread
in its `service.rs`) is built. What was decided, measured and
found while building it is in the lore; ask `lore_recall`. This page
holds only what is still not done.

## Not done

- **Nothing distills the day's dialogue any more.** Warriv's daily look
  back handed it each day's passages to keep as notes; it went with the
  rounds (2026-10-30). A note names passages as sources only when a
  session chooses to.
- **The model choice may reopen.** EmbeddingGemma beat MiniLM on dialogue
  alone (81.8 % against 72.7 %); its code was removed, so a switch means
  writing it again.
