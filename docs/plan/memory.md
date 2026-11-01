# Memory

The long term memory (`crates/horadric-memory`, the app's memory thread
in its `service.rs`) is built. What was decided, measured and
found while building it is in the lore; ask `lore_recall`. This page
holds only what is still not done.

## Not done

- **Distilling the day waits to be armed.** `horadric distill` hands a
  session the project's dialogue not yet distilled, and the "Distill the
  day" errand in `.horadric/config.json` runs it at 04:00. It does
  nothing until the human arms it in the tome: it starts a session a day,
  and the rounds went because sessions popping up on their own were not
  wanted. Not tried end to end, since that starts an agent.
- **The model choice may reopen.** EmbeddingGemma beat MiniLM on dialogue
  alone (81.8 % against 72.7 %); its code was removed, so a switch means
  writing it again.
