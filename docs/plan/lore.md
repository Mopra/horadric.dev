# Lore

The lore (spend measuring, context packs, shared notes, the code index,
the shorter quest log, the window measures and `horadric heavy`) is
built. What was decided and measured is in the lore itself;
ask `lore_recall`. This page holds only what is still not done.

The read guard (`horadric guard`) was removed on 2026-10-30: the lore's
wording alone moved Read's share of tool results from 71 % to about 7 %
over 125 quests, so it had no job.

## Not done

- **Compaction at the right moment.** A quest that ends at a 180k window
  could go on from a 40k summary after its exploration. Quests now run
  as Folios, so the interface question is answered: Horadric can send
  `/compact` as a prompt and sees `compact_boundary` come back. Left is
  when to send it (after the pack is read, before the edits) and how to
  tell that moment from the stream.
- **Tuning the context pack.** Run `horadric quest spend` again once more
  than 30 quests have run with a pack, before changing the pack's size or
  weights.
