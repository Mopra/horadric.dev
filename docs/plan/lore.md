# Lore

The lore (spend measuring, context packs, shared notes, the code index,
the shorter quest log, the window measures and `horadric heavy`) is
built. What was decided and measured is in the lore itself;
ask `lore_recall`. This page holds only what is still not done.

The read guard (`horadric guard`) was removed on 2026-10-30: the lore's
wording alone moved Read's share of tool results from 71 % to about 7 %
over 125 quests, so it had no job.

## Not done

- **Compaction, tried for real.** A quest's Folio now compacts itself
  once as its first edit starts at a window of 100k or more
  (`folio::compaction`). It has only run in unit tests: watch the first
  quests that do it, that the stopped turn goes on after `/compact`, and
  compare their spend with `horadric quest spend`.
