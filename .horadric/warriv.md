# Warriv's memory

## Rules
- A landing that fails only with "Diverging branches can't be fast-forwarded" lost a race to a commit on main (often my memory commit): file a small haiku quest to cherry-pick the branch and land it, no human needed.
- A landing red on checkpoint's git init in %TEMP% (template hook copy) is a test flake: tell the fix-up to rerun, never to change the quest's code.
- Do not wait for main's checks before casting a ship stone: nothing runs them between landings. Ship Public runs all three first (RELEASING.md step 2) and Ship Local does too since 5170f91; both stop on red (lore #30).
- Keep a chain of dependent steps in one quest; split into quests only where parts can run side by side. The human finds handovers slow (lore #34).
- A quest session that says it waits on its own background work (judge, review agent, checks) is not stuck: no tell, no hand on (lore #31).
- A quest session silent for hours with an empty worktree after two nudges is dead and I can not restart it: hand it to the human with "restart it", who does so (settled on "OSC 7501: plain terminals", which then landed).
- A quest I file gets an After: line for every quest whose code it calls, not only the nearest: "Backup: the Settings section" named part 2 but needed part 1, started early and blocked itself (2026-10-16).
- A quest blocked only to ask leave to rerun checks (killed, out of memory) gets "go" from me: reruns never need the human.
- Quests that change the landing machinery (crates/horadric-ui/src/runner.rs, worktree.rs, horadric-core merge.rs) get an After: line on each other: four ran side by side on 2026-10-28/29 and cost two conflict fix-ups and four extra fix-up quests.
- A [/] quest whose session a dev instance started (state in %APPDATA%\Horadric-dev) died with it: no tell reaches it, hand it to the human with "restart it" at once (lore #51).

## Lately
- 2026-10-29 Round (hourly, main at 377cf51, only my memory since the ddc6c34 ship): no aim, log done. Nothing to do.
- 2026-10-29 Round (human left, main at d102033, only my memory since the ddc6c34 ship): no aim, log done. Nothing to do.
- 2026-10-29 Round (hourly, main at 72999b3, only my memory since the ddc6c34 ship): no aim, log done. Nothing to do.
- 2026-10-29 Round (human left, main at 6a02ab0): the ddc6c34 Ship Local reloaded at 15:27, so main is shipped. No aim, log done. Nothing to do.
- 2026-10-29 Round (hourly, main at ddc6c34): the Folio files are committed now (359c397, ddc6c34), tree clean apart from tasks files, no cargo, installed build from 11:19, so cast "Ship Local". No aim, log done.
- 2026-10-29 Round (14:18, human left, main at 8482f58): the human's session did the headless spike itself (97a9e62 to 8482f58), so noted that on the spike quest. Code since the v0.35.0 ship (Tristram header, quieter typing, Folio) is unshipped, but six Folio files sit uncommitted from that live session, so held "Ship Local". No aim.
- 2026-10-29 Round (hourly, main at c50feab, only my memory since the v0.35.0 ship): app.rs and window.rs sit uncommitted, a live session's. Spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (hourly, main at 39aff70, only my memory since the v0.35.0 ship): spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (human left, main at 804bd7b): the human cut v0.35.0 (11:19), no code since cb21098, so nothing to ship. Spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (hourly, main at c19b79b, only my memory since the cb21098 ship): spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (hourly, main at 342f893, only my memory since the cb21098 ship): spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (08:28, human left, main at cb21098): the human's two ship commits (6d1ba48, cb21098) were built and reloaded at 08:13, so main is shipped. Spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (hourly, main at 0a0c1bf, only my memory since the ae3d444 ship): spike waits on the human, no aim. Nothing to do.
- 2026-10-29 Round (06:36, human left, main at 21cc39b): the ae3d444 Ship Local built 06:35 and reloaded fine. Only the spike left, waiting on the human. Nothing to do.
- 2026-10-29 Round (landing, main at ae3d444): the stray session.rs edit landed and the main checkout is clean, so cast "Ship Local". Spike waits on the human.
- 2026-10-29 Round (06:11, human left, main at b5c7e03): the 06:01 Ship Local reloaded fine at 06:11. The unowned session.rs edit (Stop with background work stays Working, tested) still lingered after 5 hours, so filed "Land the stray session.rs edit..." (sonnet, Assumed) instead of asking. Spike waits on the human.
- 2026-10-29 Round (06:01, landing, main at b1693f1): the context panel landed. The 9b90bd5 Ship Local never reloaded (release exe still 05:21, no cargo, heavy idle), so cast "Ship Local" for the panel and the checkpoint fix. session.rs holds a stale uncommitted edit from 01:18 (Stop with background work stays Working), shipped before too. Spike waits on the human.
- 2026-10-29 Round (landing, main at 9b90bd5): the checkpoint git-init fix landed and the 0b12636 ship reloaded, so cast "Ship Local". Context panel still runs, spike waits on the human.
- 2026-10-29 Round (landing, main at 0b12636): "Sessions waiting on their own checks..." landed (edd8483), so cast "Ship Local" as Open said. Context panel and checkpoint fix still run, spike waits on the human.
- 2026-10-29 Round (human left, main at c6c1007, only my memory since 8f6e4c3): the 8f6e4c3 Ship Local reloaded fine at 05:21. Three quests still run, spike waits on the human. Nothing to do.
- 2026-10-29 "Sessions waiting on their own checks..." stopped undone a third time: landing part green, its heavy agent still runs. Own work, no tell, noted it.
- 2026-10-29 Round (landing, main at 8f6e4c3, "Blocked quests say why" landed): cast "Ship Local" as Open said (7 quests since the last ship). Told the blocked context panel go to rerun its memory-killed checks and ff-merge. "Sessions waiting on their own checks" stopped undone again, still on its own agents: no tell.
- 2026-10-29 "Sessions waiting on their own checks are not nudged..." stopped undone while two of its own agents run (the bug it fixes). Own work, no tell, noted it.
- 2026-10-29 Look back (10 done, 5 merged, 5 not, 17 Warriv wakes): conflicts were landing quests in runner.rs/worktree.rs side by side (new rule, the drift notice has landed); red tests were the checkpoint temp flake (quest running, told it of a second form); the 8 silent stops were sessions waiting on their own heavy checks, so filed "Sessions waiting on their own checks are not nudged or counted as stopped" (opus). The day gave 4 lore notes (#558 to #561, #561 replaces #30).
- 2026-10-29 Early (to 05:00): landing machinery quests (worktree from main, land while alive, tell running quests, merge hotspots) ran side by side and cost conflict fix-ups in runner.rs and worktree.rs (new rule); three red landings were the checkpoint git-init temp flake (fix filed and landed). Many stops were sessions waiting on their own checks (no tell). Cast Ship Local after each green landing.
- 2026-10-28 "Headless sessions: the spike" blocked on the human's own gate; gave it Model: opus.
- 2026-10-24 Brotherhood and Linux node work by a live session all day: held Ship Local while its files sat uncommitted, cast it at 05:42, 13:16 and 14:04 once clean. "Hand a picked thing to the agent" could not fast-forward past uncommitted files, so filed "Land the picked thing branch", which landed and shipped. v0.32.0 published by the human. Idle hourly rounds after.
- 2026-10-23 "Browser pane draws pages offset" landed, cast Ship Local.
- 2026-10-17 to 2026-10-21 Mostly idle rounds, task notification quest with the human. Ship Local after the bench, Lore parts 6 and 7, Tristram browser tiles, Accounts and the dev instance quest; v0.30.x and v0.31.0 shipped public by the human. A dev instance had taken a quest and died with it (rule).
- 2026-10-16 Look back (29 done, 26 merged, 51 Warriv sessions): the day gave 5 notes (#30 to #34). The 14 unreported stops were sessions waiting on their own background work, so filed "Quest sessions waiting on their own background work are not nudged" (sonnet). The 2 conflicts were both crates/horadric/src/task.rs on the spend quest's fix-ups, already landed; warned the Grep/Glob quest it shares hooks with "recall on every prompt". Found ship local sessions skip the checks, filed "Ship Local runs the three checks first and stops on red" (haiku) and fixed my rule.
- 2026-10-16 Earlier: many wakes for Memory and Backup quests waiting on their own checks, review agents or benchmarks (not stuck, no tell). Ship Local cast after lore_recall by meaning, transcripts and code by meaning, across projects, Warriv distills, the look back; Ship Public cast for all of Memory and landed as v0.28.0. Gave models to the Memory and Backup quests. Look back filed the "not nudged" and "Ship Local runs the checks" quests.
- 2026-10-16 Early: embedding model, store and model fetch landed as crates the app did not use yet, so no ship until lore_recall by meaning. Ship Public cast for the Arcane Sanctuary after the sky quest landed (v0.27.0); held earlier ships while sky files sat uncommitted in the shared tree.
- 2026-10-15 Lore and Tristram landed through the evening with a Ship Local after each step and one Ship Public for both (v0.26.0). Hand-marked spend needed two merge fix-ups in task.rs (sonnet). Several rounds held a second cast while a ship already building would carry the landing.
- 2026-10-13 Only docs and memory landed after 0.24.0, no ship. The human settled cloud tiles: wait for `claude agents --json`. The plain terminals answer became a rule.
- 2026-10-12 Town Portal's second round landed quest by quest, a Ship Local after each (each reloaded fine); iOS keyboard landed in horadric.portal. Cloud tiles waited on the human.
- 2026-10-11 Claude Code 2.1.287 to 2.1.296 series: held ships while quests were half done in the shared tree, shipped once it was clean. "OSC 7501: plain terminals" went silent and was handed to the human to restart. Many idle hourly rounds before that.
- 2026-10-06 to 2026-10-10 Mostly idle rounds (no quests, no aim). Settings window series landed 10-07 and shipped public as v0.16.0; Mac quests shipped as v0.17.0; v0.18.0 and v0.19.0 were cut by hand.

## Open
- The checkpoint git-init fix landed (9b90bd5): if the flake shows again, look at %TEMP% itself (antivirus, locks).
- "Headless sessions: the spike" was done by the human's session outside the log (noted on the quest); it stays [!] until they close it.
- Once quests run with a pack, the pack comparison resumes by itself.
- "Sessions waiting on their own checks..." landed: the next look back's silent stops count should drop.
