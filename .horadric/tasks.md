# Horadric tasks

The order is the work order. Each item points at its section of
docs/PLAN.md, which has the reasoning; read it before starting.

## Animations

The small niceties that make the app feel alive, worked by one session in
order. Each one respects Windows' animation setting, costs nothing at rest,
and is written up under "The look", Motion, in docs/PLAN.md.

## Ideas

Everything in docs/IDEAS.md not yet built, asked for by the human. For each one: write its design into docs/PLAN.md first (a section under Next, with the reasoning), change its entry in IDEAS.md to say it is planned, then build it. The rule in IDEAS.md holds: the theme earns its place by making something clearer or quicker.

## Codex and Grok Build

ChatGPT and xAI subscriptions used from Horadric like a Claude one. See "Codex and Grok Build beside Claude Code" under Next in docs/PLAN.md, which has the table of what each agent gives and the reasoning; read it before starting. Claude sessions must behave exactly as before at every step.

## Discord Activity

Rust was installed on this machine after Horadric started, so a session's shell may not find `cargo`: put `%USERPROFILE%\.cargo\bin` first on `PATH` (Git Bash: `export PATH="$HOME/.cargo/bin:$PATH"`). Commits stay local on `main`; this account cannot push, and the work goes up as one PR when the section is done.

Horadric on the human's Discord profile through Rich Presence. See "Discord Activity" under Next in docs/PLAN.md, which has the protocol, the rules and the reasoning; read it before starting. The first four run side by side in the shared tree, so commit small, stage only your own files, and keep to your own new files where you can. Only the tray quest runs a dev instance on 43118; the others test with `cargo test` or a small check of their own, never a second dev instance.

## The Runetome

Runewords as rune stones in a tile of their own. See "The Runetome" under Next in docs/PLAN.md, which has the reasoning; read it and "Runewords" under Done before starting. The two quests at the top build in parallel; the tile needs both on main.

## Runetome feedback

From the human on 2026-10-01, after using the tome.

## Orchestration

An orchestrator so quests run by the dozen without the human. See "Orchestration" in docs/PLAN.md, and read its "Decided on 2026-10-05" part before starting. The first four quests build in parallel, each in its own worktree; they touch the same files (tasks.rs, runner.rs), so merge main into your branch before you merge it back. Every quest that changes how sessions start counts claude.exe after its dev instance test.

## The caravan moves on its own

Make the app run itself when nothing is wrong and nobody watches. See "The caravan moves on its own" in docs/PLAN.md and read it whole before starting. Several of these touch crates/horadric-ui/src/warriv.rs and crates/horadric-core/src/warriv.rs, so merge main into your branch before you merge back. Every quest that starts agents counts claude.exe after its dev instance test.

## Warriv drives

Warriv runs the project alone while the human is away, and ships. See "Warriv drives" in docs/PLAN.md and read it whole, with "The caravan moves on its own" above it, before starting. No caps on quests, spend or usage: do not add any. Merge main into your branch before you merge back; these touch the same Warriv files. Every quest that starts agents counts claude.exe after its dev instance test.

## macOS

Horadric on a Mac, public and downloadable. See "macOS" in docs/PLAN.md and read it whole first. Mac code is checked here with `cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings` and tested on a real Mac in CI's macos job.

- [x] Headless sessions: the spike (step 1 of the Folio plan): waiting for the human to say go
  Start the plan in docs/plan/headless.md (read it whole first; the decisions there are made, do not reopen them). Do step 1 only, the spike, and stop there.
  Decided by the human: Claude sessions go headless, driving the installed unmodified claude.exe over stream-json on pipes, NO Agent SDK and no Node or Python. The terms' grey zone is accepted. Terminal sessions stay working beside headless ones. Markdown will be our own small parser (no pulldown-cmark), not part of this step.
  Protocol facts and sources: lore note #504 (lore_recall headless Claude protocol). The control protocol is undocumented SDK internals; confirm everything against the real CLI.
  What the spike must deliver:
  1. A way to start headless claude.exe from Rust with pipes (CreateProcess, never claude.cmd), env without CLAUDECODE, ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, flags: --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio. Find out whether -p is needed.
  2. A hidden debug command (for example `horadric folio-spike --cwd DIR prompt`) that sends prompts, prints every stream line, and answers can_use_tool from the console.
  3. Confirm and write down: init shows apiKeySource none (subscription login); Horadric's hooks still fire and a dev instance tile follows the phase; a Bash permission ask arrives as can_use_tool and allow/deny work (always echo updatedInput); AskUserQuestion and ExitPlanMode arrive and can be answered; interrupt works mid turn; a prompt sent mid turn queues; --resume of a session started in a terminal works, and a headless session resumes in a terminal.
  4. Record each of those runs as NDJSON fixtures under crates/horadric-core/tests/fixtures/headless/ for step 2's tests. Strip anything personal from them.
  5. Update docs/plan/headless.md: replace guesses in The protocol with what you found, mark step 1 done, and leave lore notes on the traps.
  Use a cheap model for test prompts (--model claude-haiku-4-5-20251001). Test only as a dev instance, never touch the installed Horadric, count claude.exe afterwards. Commit with `git commit -- <your files>`: the index is shared and a bare commit sweeps in other sessions' staged work.
  Warriv: Left for the human: the title gates it on their go, and no session has run it yet, so there is nothing to answer. Model: opus, since it is an undocumented protocol spike that spans the code.
  Warriv: Model: opus
  Warriv: The human's own session has done the spike outside the log: 97a9e62 contract, 5c5ce9d headless host, 4f26b71 Folio model, f6257aa plain Folio, 8482f58 plan says what it confirmed. This quest is likely done; left for the human to close or drop.
  Dropped: Done in a master session on 2026-10-29: spike, model and a plain Folio landed (5c5ce9d to ddc6c34)
- [x] Start a quest's worktree from main, not from whatever the main tree has checked out @start-a-quest-s-worktree-from-main-not-82610
  ui/worktree.rs:108 runs git worktree add -b <branch> <path> with no start point, so a quest branches from the main tree's current HEAD. On optipeople.website 2026-10-26 'Fresh 1.5x captures' carried another quest's commit (5ab5fe8) and conflicted only because of it, costing two fix-ups. Pass the project's main branch (the one land() rebases into) as the start point. Add a test.
  Assumed: the start point is the branch the main tree has checked out (what land() rebases into), by name rather than HEAD; while the main tree is on no branch (mid pull --rebase, the likely cause of 5ab5fe8) it falls back to origin/HEAD, then main, then master. land() itself is unchanged, since its fast forward runs in the main tree and must target its checked-out branch.
  Outcome: claude-opus-5-5, 43m, red, 1.9M tokens
- [x] Fix the merge of start-a-quest-s-worktree-from-main-not @fix-the-merge-of-start-a-quest-s-worktree-85808
  "Start a quest's worktree from main, not from whatever the main tree has checked out" is done on the branch `start-a-quest-s-worktree-from-main-not` but did not merge into `main`: the check `cargo test --workspace` fails after rebasing it on `main`.
  Take its commits onto your branch with `git cherry-pick main..start-a-quest-s-worktree-from-main-not`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D start-a-quest-s-worktree-from-main-not`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  test tasks::tests::the_mode_is_kept_in_the_config ... ok
  test tend::tests::a_crowded_file_brings_its_oldest_notes_a_handful_at_most ... ok
  test tend::tests::nothing_stale_or_crowded_asks_nothing ... ok
  test tend::tests::stale_notes_on_changed_files_come_first ... ok
  test tests::a_dev_instance_drops_an_owner_that_is_not_itself ... ok
  test tests::a_refusal_names_the_owner_and_the_caller ... ok
  test tests::dev_is_any_value_but_empty_or_zero ... ok
  test tests::dev_moves_the_default_port_but_not_an_explicit_one ... ok
  test tests::state_must_match_when_both_sides_know_it ... ok
  test transcript::tests::a_missing_file_has_no_title ... ok
  test transcript::tests::a_transcript_is_found_by_its_folder_or_else_anywhere ... ok
  test transcript::tests::codex_limits_are_read_from_the_rollouts_end ... ok
  test listener::tests::event_owned_here_or_by_nobody_stays ... ok
  test transcript::tests::folder_names_match_claude_codes ... ok
  test transcript::tests::history_is_newest_first_titled_and_skips_the_open_ones ... ok
  test transcript::tests::the_config_dir_beats_home ... ok
  test transcript::tests::finds_a_title_far_behind_a_huge_tool_result ... ok
  test listener::tests::new_session_request_is_forwarded ... ok
  test listener::tests::new_session_without_the_app_is_unavailable ... ok
  test listener::tests::a_recall_needs_its_header_and_no_origin ... ok
  test listener::tests::tagged_post_is_forwarded ... ok
  test listener::tests::the_mod_is_answered_what_the_app_knows_of_its_session ... ok
  test listener::tests::wrong_path_is_404 ... ok
  test listener::tests::reload_needs_its_own_header_and_no_origin ... ok
  test listener::tests::status_is_forwarded_as_an_event ... ok
  test listener::tests::the_mod_must_name_its_session_and_not_be_a_web_page ... ok
  test listener::tests::an_event_from_another_agent_is_read_in_its_shape ... ok
  test listener::tests::event_owned_elsewhere_is_passed_on ... ok
  test checkpoint::tests::a_failed_write_leaves_the_file_another_session_made ... ok
  test listener::tests::a_browser_call_given_up_on_is_no_longer_wanted ... ok
  test checkpoint::tests::an_interrupted_turn_leaves_a_hand_edit_alone ... ok
  failures:
  ---- checkpoint::tests::a_turn_is_snapshotted_and_undone_in_a_real_repository stdout ----
  thread 'checkpoint::tests::a_turn_is_snapshotted_and_undone_in_a_real_repository' (26932) panicked at crates\horadric-hooks\src\checkpoint.rs:708:47:
  called `Result::unwrap()` on an `Err` value: "git init: fatal: cannot copy 'C:/Program Files/Git/ucrt64/share/git-core/templates/hooks/pre-push.sample' to 'C:/Users/morte/AppData/Local/Temp/horadric-c...
  note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
  failures:
  checkpoint::tests::a_turn_is_snapshotted_and_undone_in_a_real_repository
  test result: FAILED. 98 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.68s
  error: test failed, to rerun pass `-p horadric-hooks --lib`
  ```
  Warriv: Model: sonnet
  Warriv: The red test is checkpoint's git init failing to copy a template hook into %TEMP%, a file system flake unrelated to worktree.rs. Cherry-pick, rerun the checks; if that test fails again, look at it on main before touching the quest's code. Sonnet, since there is no conflict to design around.
  Warriv: The work was green; the merge failed only because my memory commit 0f551a6 landed on main between its rebase and its fast-forward. Filed 'Land the worktree fix from fix-the-merge-of-start-a-quest-s-worktree' (haiku) to cherry-pick a5c28b5 and land it.
- [x] Land the worktree fix from fix-the-merge-of-start-a-quest-s-worktree @land-the-worktree-fix-from-fix-the-merge-5576
  Model: haiku
  The branch fix-the-merge-of-start-a-quest-s-worktree holds one finished commit (a5c28b5, crates/horadric-ui/src/worktree.rs) that could not fast-forward only because a Warriv memory commit (.horadric/warriv.md) landed on main after it rebased. No conflict is expected. Run git cherry-pick main..fix-the-merge-of-start-a-quest-s-worktree, run every check in .horadric/config.json through horadric heavy, commit, then git branch -D fix-the-merge-of-start-a-quest-s-worktree. If checkpoint's git init into %TEMP% fails copying a template hook, that is a known flake: rerun, do not change code for it.
  Filed by Warriv: carries the work so the quests after it can go on.
  Warriv: Stopped undone while its own clippy and tests run in heavy. Own work, no tell.
  Outcome: haiku, 53m, conflict, 1.0M tokens
- [x] Fix the merge of land-the-worktree-fix-from-fix-the-merge @fix-the-merge-of-land-the-worktree-fix-8799
  "Land the worktree fix from fix-the-merge-of-start-a-quest-s-worktree" is done on the branch `land-the-worktree-fix-from-fix-the-merge` but did not merge into `main`: rebasing it on `main` conflicts.
  Take its commits onto your branch with `git cherry-pick main..land-the-worktree-fix-from-fix-the-merge`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D land-the-worktree-fix-from-fix-the-merge`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  Auto-merging crates/horadric-ui/src/worktree.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/worktree.rs
  error: could not apply e1195f5... Start a quest's worktree from the main branch, not the main tree's HEAD
  hint: Resolve all conflicts manually, mark them as resolved with
  hint: "git add/rm <conflicted_files>", then run "git rebase --continue".
  hint: You can instead skip this commit: run "git rebase --skip".
  hint: To abort and get back to the state before "git rebase", run "git rebase --abort".
  hint: Disable this message with "git config set advice.mergeConflict false"
  Could not apply e1195f5... # Start a quest's worktree from the main branch, not the main tree's HEAD
  ```
  Warriv: Model: sonnet
  Warriv: Its parent landed with a real conflict in worktree.rs against 6576c9d. Told it to keep both features and treat the checkpoint git-init failure as a flake. Sonnet: one file, both intents are written down.
  Warriv: Stopped undone but waits on its own clippy and tests, queued in heavy, conflict already resolved. Own work, no tell.
  Outcome: claude-opus-5-5, 30m, green, 758k tokens
- [x] Land a quest while its session is still alive, and hand a conflict back to that session @land-a-quest-while-its-session-is-still-84614
  After: Fix the merge of start-a-quest-s-worktree-from-main-not
  Root cause of most merge pain (Tristram review 2026-10-28, chronicle.jsonl: 51 conflict events in 7 projects, all found only at landing). Today close_finished ends the session first (ui/runner.rs:1560-1573, 1794-1798), then land() rebases (ui/worktree.rs:556-590), and on conflict aborts and files 'Fix the merge of X' for a fresh session with no context (core/merge.rs:105-138). Fix-ups then chase a moving main: nvoke desktop chained 'fix-the-merge-of-fix-the-merge-of...' 4 deep, and nooni's Dots merge resolved against the human's ruling. Change: on quest done, rebase onto main and run checks BEFORE ending the session. On conflict or red, leave the rebase stopped (or redo it in the worktree) and tell the live session what conflicted, with the other side's commit subjects so it knows the intent, and let it resolve, run checks and call quest done again. Only when the session is gone fall back to today's fix-up quest. A fix-up or a resolving session should hold the project's landing lock so main cannot move under it again. Keep the Outcome line honest (conflict, then resolved).
  After: Start a quest's worktree from main, not from whatever the main tree has checked out
  Assumed: a failed rebase is undone and the session redoes it, rather than leaving it stopped, so a session that dies leaves a clean tree. The landing hold covers landings that start from close_finished only; a session ended by hand still lands at once. Up to 3 hand-backs, then a fix-up quest.
  Outcome: claude-opus-5-5, 78m, red, 7.3M tokens
  Warriv: Landed red on the checkpoint git-init temp flake (second time today), not its code. Told the fix-up to cherry-pick and rerun without touching the code, and filed "Make checkpoint tests' git init not copy hook templates" (haiku) to end the flake.
- [x] Fix the merge of land-a-quest-while-its-session-is-still @fix-the-merge-of-land-a-quest-while-its-2954
  "Land a quest while its session is still alive, and hand a conflict back to that session" is done on the branch `land-a-quest-while-its-session-is-still` but did not merge into `main`: the check `cargo test --workspace` fails after rebasing it on `main`.
  Take its commits onto your branch with `git cherry-pick main..land-a-quest-while-its-session-is-still`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D land-a-quest-while-its-session-is-still`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  test tasks::tests::the_mode_is_kept_in_the_config ... ok
  test tend::tests::a_crowded_file_brings_its_oldest_notes_a_handful_at_most ... ok
  test tend::tests::nothing_stale_or_crowded_asks_nothing ... ok
  test tend::tests::stale_notes_on_changed_files_come_first ... ok
  test tests::a_dev_instance_drops_an_owner_that_is_not_itself ... ok
  test tests::a_refusal_names_the_owner_and_the_caller ... ok
  test tests::dev_is_any_value_but_empty_or_zero ... ok
  test tests::dev_moves_the_default_port_but_not_an_explicit_one ... ok
  test tests::state_must_match_when_both_sides_know_it ... ok
  test transcript::tests::a_missing_file_has_no_title ... ok
  test tasks::tests::the_list_is_found_from_a_folder_inside_the_project ... ok
  test transcript::tests::a_transcript_is_found_by_its_folder_or_else_anywhere ... ok
  test transcript::tests::codex_limits_are_read_from_the_rollouts_end ... ok
  test transcript::tests::folder_names_match_claude_codes ... ok
  test listener::tests::reload_request_is_forwarded ... ok
  test transcript::tests::the_config_dir_beats_home ... ok
  test transcript::tests::history_is_newest_first_titled_and_skips_the_open_ones ... ok
  test transcript::tests::finds_a_title_far_behind_a_huge_tool_result ... ok
  test listener::tests::new_session_needs_the_header_and_no_origin ... ok
  test listener::tests::tagged_post_is_forwarded ... ok
  test listener::tests::an_event_from_another_agent_is_read_in_its_shape ... ok
  test listener::tests::the_mod_is_answered_what_the_app_knows_of_its_session ... ok
  test listener::tests::reload_needs_its_own_header_and_no_origin ... ok
  test listener::tests::untagged_post_is_passed_on_without_a_tag ... ok
  test listener::tests::status_is_forwarded_as_an_event ... ok
  test listener::tests::the_mod_must_name_its_session_and_not_be_a_web_page ... ok
  test listener::tests::event_owned_elsewhere_is_passed_on ... ok
  test listener::tests::wrong_path_is_404 ... ok
  test checkpoint::tests::an_interrupted_turn_leaves_a_hand_edit_alone ... ok
  test listener::tests::a_browser_call_given_up_on_is_no_longer_wanted ... ok
  test checkpoint::tests::a_turn_is_snapshotted_and_undone_in_a_real_repository ... ok
  failures:
  ---- checkpoint::tests::a_failed_write_leaves_the_file_another_session_made stdout ----
  thread 'checkpoint::tests::a_failed_write_leaves_the_file_another_session_made' (30452) panicked at crates\horadric-hooks\src\checkpoint.rs:708:47:
  called `Result::unwrap()` on an `Err` value: "git init: fatal: cannot copy 'C:/Program Files/Git/ucrt64/share/git-core/templates/hooks/pre-push.sample' to 'C:/Users/morte/AppData/Local/Temp/horadric-c...
  note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
  failures:
  checkpoint::tests::a_failed_write_leaves_the_file_another_session_made
  test result: FAILED. 98 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.83s
  error: test failed, to rerun pass `-p horadric-hooks --lib`
  ```
  Warriv: Stopped undone but waits on its own clippy, queued in heavy, then its tests. Not stuck: no tell.
  Outcome: claude-opus-5-5, 51m, conflict, 1.1M tokens
- [x] Fix the merge of fix-the-merge-of-land-a-quest-while-its @fix-the-merge-of-fix-the-merge-of-land-a-6055
  "Fix the merge of land-a-quest-while-its-session-is-still" is done on the branch `fix-the-merge-of-land-a-quest-while-its` but did not merge into `main`: rebasing it on `main` conflicts.
  Take its commits onto your branch with `git cherry-pick main..fix-the-merge-of-land-a-quest-while-its`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D fix-the-merge-of-land-a-quest-while-its`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  Auto-merging crates/horadric-ui/src/runner.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/runner.rs
  Auto-merging crates/horadric-ui/src/worktree.rs
  error: could not apply 381eee3... Land a quest while its session is still alive, and hand a conflict back to it
  hint: Resolve all conflicts manually, mark them as resolved with
  hint: "git add/rm <conflicted_files>", then run "git rebase --continue".
  hint: You can instead skip this commit: run "git rebase --skip".
  hint: To abort and get back to the state before "git rebase", run "git rebase --abort".
  hint: Disable this message with "git config set advice.mergeConflict false"
  Could not apply 381eee3... # Land a quest while its session is still alive, and hand a conflict back to it
  ```
  Warriv: Model: opus. The conflict in runner.rs is with 'Tell running quests when main moves', which landed meanwhile and also changed runner.rs: keep both features. A red checkpoint test on git init in %TEMP% (hook template copy) is a known flake, rerun it, do not change code for it.
  Warriv: Stopped undone but waits on its own clippy, queued in heavy behind another session's tests; conflict already resolved keeping both sides. Own work, no tell.
  Outcome: claude-opus-5-5, 35m, green, 1.2M tokens
- [x] Tell running quests when main moves under the files they touch @tell-running-quests-when-main-moves-under-2455
  After: Fix the merge of fix-the-merge-of-land-a-quest-while-its
  After: Fix the merge of land-a-quest-while-its-session-is-still
  Nothing refreshes a quest from main while it runs (core/worktree.rs:190-210). Evidence: the replays quest on optipeople.website started 16:40, the other side landed 16:43 to 16:46, the conflict surfaced at 17:24; horadric.dev 'Settings window' conflicted 225 minutes after the overlapping section quests landed; nvoke 'search across every lead' was 303 minutes behind with 14 merges in between. Change: after each landing, compare the files it changed with each running worktree's changed files (git diff main...branch --name-only). Where they overlap, tell that session between turns: main moved, these files overlap yours, rebase now (git rebase main) and resolve while it is small. Optionally do a quiet trial merge (git merge-tree) and only tell when it really conflicts. No overlap, no message, no tokens.
  After: Land a quest while its session is still alive, and hand a conflict back to that session
  Warriv: Model: sonnet
  Warriv: Stopped while its clippy run waits in the heavy queue behind another session's check. Waiting on its own work, not stuck: no tell, no hand on.
  Warriv: Stopped again, now with clippy clean and its tests running in the background. Waiting on its own work: no tell, no hand on.
  Warriv: Stopped a third time, now waiting on clippy over the merged code before it fast-forwards main. Own work, not stuck: no tell, no hand on.
  Assumed: a session is told only when its branch overlaps the landed files AND git merge-tree says it would really conflict, to save tokens; a clean overlap stays quiet.
  Outcome: sonnet, 59m, landed, 2.9M tokens
- [x] Catch duplicate scope when a quest is filed @catch-duplicate-scope-when-a-quest-is-82622
  horadric.dev 'Settings window' was effectively filed twice: four 'Settings: ... section' quests built the same thing, and its fix-up 'dropped a duplicate Settings window build main already had' (about 20M tokens thrown away). When a quest is added (quest add, Warriv, aims), compare its title and notes with open, running and recently done quests in the same project, and say so when it overlaps: to the filer on the CLI, to Warriv as an event. Suggest --after or a merge of the two. Do not block silently, and keep false positives low (cheap word overlap first; only ask a model when the overlap is real). Serialize only real overlap; parallelism otherwise stays.
  Outcome: claude-opus-5-5, 53m, landed, 8.0M tokens
- [x] Find merge hotspot files and make them conflict-free @find-merge-hotspot-files-and-make-them-82634
  About two thirds of 51 conflicts were mechanical: two quests appending to the same log or adding entries to the same registry. Proven fixes: mrr 'PLAN.md merge=union' in .gitattributes (bf0678a) ended its log conflicts; nvoke desktop 'one file per feature for the IPC contract, preload' (17661b4) took ipc/contract.ts from 14 conflict events to zero. Build it into Horadric: the look-back (core/lookback.rs:44-47) already counts conflicts per file. Have it name the hotspot files per project, and for append-only logs (PLAN.md, LOG.md, trackers) propose merge=union; for registries (route lists, env lists, nav, catalogs) file a quest in that project to split them per feature. Current hotspots to seed: nvoke apps/server/src/env.ts, api/app.ts, packages/shared/src/catalog.ts, apps/server/src/cli.ts, docs/SPEC.md; optipeople.website components/templates/standard-page.tsx, content/pages/modules.ts, content/shared/types.ts; nooni Footer.tsx, seo.ts, llms.ts, product-meta.ts.
  Warriv: Woke on a stop without done: the session waits on its own queued test run and a review agent, then commits. Not stuck, no tell (rule: waiting on own background work). Model: sonnet
  Outcome: claude-opus-5-5, 112m, landed, 8.7M tokens
- [x] Blocked quests say why, what the human must do, and how old they are @blocked-quests-say-why-what-the-human-82646
  The owner loses track of [!] quests: they do not know why one is blocked, whether it still matters, or if it is stale. Today the log stores a free-text reason and an optional wait (core/tasks.rs:11-21, 390-479) but no blocked-since time; age shows only in the Quest Log window via the chronicle (ui/questlog.rs:404-413), not on the board (ui/board.rs:61-74). Change: 1) store blocked-since on the line. 2) Classify what it waits on: the human, a time, another quest, a file/command, or an outside thing. 3) Require (or have Warriv write) one plain line of 'what you would do' when it waits on the human, e.g. 'Say go or drop on clip 1 at ~/Downloads/nvoke-clip-1-emails.mp4'. 4) Board and a cross-project view: group 'Needs you' first, sorted by age, then 'Waiting on time/other quest'. 5) After about 5 days waiting on the human, ask once: still relevant? resume or drop, and dropping closes it with a note. Example of today's confusion: nvoke '[!] Company index: measure Common Crawl hits' carries several paragraphs and a Warriv note but no age or single action.
  Outcome: claude-opus-5-5, 124m, blocked, 24.3M tokens
  Warriv: Told it to rerun the tests through heavy and merge on green: rerunning killed checks is reversible and needs no human.
  Outcome: claude-opus-5-5, 156m, landed, 5.6M tokens
- [x] Context panel: see and toggle every connector, plugin and skill a project's sessions load @context-panel-see-and-toggle-every-3750
  The owner was surprised to learn every session carries about 52k tokens before doing any work, re-read on every turn. Measured on an mrr inbox-sweep session (121 turns): deferred tool names about 9k tokens (mostly claude.ai connectors: monday, Microsoft 365, Atlassian, Slack, Read AI, Google Drive), skill listing about 9k (Figma, Vercel, Cloudflare, Supabase, monday plugins), MCP server instructions about 3k (Optipeople Demo pastes a large system-context JSON), hook output about 9k (includes Horadric's own lore recall and the timeforing hook). An inbox sweep uses almost none of it. Want: a per-project panel in Horadric (and a cross-project overview) listing each thing that loads, where it comes from (claude.ai account connector, user/project MCP, plugin, skill, hook, CLAUDE.md), and its token cost, building on 'horadric quest start' which already itemises a first call. Each line gets a toggle that writes the real setting into the project's .claude/settings.json: enabledPlugins for plugins, env ENABLE_CLAUDEAI_MCP_SERVERS=false for claude.ai connectors (both confirmed present in the installed Claude Code; verify whether connectors can be turned off one by one or only all together, and say so in the panel), disabledMcpjsonServers / project MCP config for others. Flag likely waste: a connector or skill no session in this project has called in N days (count from transcripts). Also let quests run lean by default with a per-quest opt-in for a connector (e.g. Zoho errands need the browser, not monday). Measure Horadric's own hook output and trim it if large. Show the before/after tokens per turn.
  Warriv: Model: opus
  Warriv: Warriv: blocked only to rerun checks killed for low memory. Told it go: rerun through heavy, ff-merge on green.
  Outcome: opus, 76m, landed, 5.9M tokens
- [x] Slim CLAUDE.md to under 120 lines @slim-claude-md-to-under-120-lines-7672
  Run /slim-claude-md. The human approved this plan (200 to ~110 lines). Stale: lines 3 and 18 say Windows-only/Win32, but there is a macOS port (horadric-ui/src/mac/, mac_keys.rs, install_mac.rs, objc2 crates, mac workflows). Dependency list misses libc and the objc2-* crates. Moves: dependency paragraph to new docs/dependencies.md (one-line pointer stays). 'Verifying Windows code' and 'Testing against a real agent' to new docs/dev-instance.md, but keep the kill command and the host warning in CLAUDE.md. Merge Warriv switches, errands, heavy-check sequence, reload and first install from 'Developing from inside Horadric' into RELEASING.md; keep the ship-local vs ship-public trigger words. Cut the em dash rule to one line (global CLAUDE.md has it). Add: a 7-crate map (horadric, core, hooks, link, memory, pty, ui) and one-line pointers to docs/plan/the-brotherhood.md, brotherhood-ownership.md, one-portal.md, lore.md, memory.md, macos.md; mention Warriv and the browser pane. Lint: cargo fmt --all, cargo clippy --workspace --all-targets -- -D warnings. Commit only touched files.
  Warriv: Model: sonnet
  Outcome: sonnet, 18m, red, 919k tokens
- [x] Fix the merge of slim-claude-md-to-under-120-lines @fix-the-merge-of-slim-claude-md-to-under-10131
  "Slim CLAUDE.md to under 120 lines" is done on the branch `slim-claude-md-to-under-120-lines` but did not merge into `main`: the check `cargo test --workspace` fails after rebasing it on `main`.
  Take its commits onto your branch with `git cherry-pick main..slim-claude-md-to-under-120-lines`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D slim-claude-md-to-under-120-lines`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  test tend::tests::stale_notes_on_changed_files_come_first ... ok
  test tests::a_dev_instance_drops_an_owner_that_is_not_itself ... ok
  test tests::a_refusal_names_the_owner_and_the_caller ... ok
  test tests::dev_is_any_value_but_empty_or_zero ... ok
  test tests::dev_moves_the_default_port_but_not_an_explicit_one ... ok
  test tests::state_must_match_when_both_sides_know_it ... ok
  test transcript::tests::a_missing_file_has_no_title ... ok
  test transcript::tests::a_transcript_is_found_by_its_folder_or_else_anywhere ... ok
  test transcript::tests::codex_limits_are_read_from_the_rollouts_end ... ok
  test transcript::tests::finds_a_title_far_behind_a_huge_tool_result ... ok
  test transcript::tests::folder_names_match_claude_codes ... ok
  test transcript::tests::history_is_newest_first_titled_and_skips_the_open_ones ... ok
  test transcript::tests::the_config_dir_beats_home ... ok
  test listener::tests::new_session_request_is_forwarded ... ok
  test listener::tests::new_session_without_the_app_is_unavailable ... ok
  test listener::tests::reload_request_is_forwarded ... ok
  test listener::tests::new_session_needs_the_header_and_no_origin ... ok
  test listener::tests::tagged_post_is_forwarded ... ok
  test listener::tests::the_mod_is_answered_what_the_app_knows_of_its_session ... ok
  test listener::tests::an_event_from_another_agent_is_read_in_its_shape ... ok
  test listener::tests::untagged_post_is_passed_on_without_a_tag ... ok
  test listener::tests::wrong_path_is_404 ... ok
  test listener::tests::status_is_forwarded_as_an_event ... ok
  test listener::tests::the_mod_must_name_its_session_and_not_be_a_web_page ... ok
  test listener::tests::event_owned_elsewhere_is_passed_on ... ok
  test checkpoint::tests::an_interrupted_turn_leaves_a_hand_edit_alone ... ok
  test listener::tests::a_browser_call_given_up_on_is_no_longer_wanted ... ok
  test checkpoint::tests::a_turn_is_snapshotted_and_undone_in_a_real_repository ... ok
  failures:
  ---- checkpoint::tests::a_failed_write_leaves_the_file_another_session_made stdout ----
  horadric: no checkpoint for undo-test-3: git for-each-ref: fatal: not a git repository (or any of the parent directories): .git
  thread 'checkpoint::tests::a_failed_write_leaves_the_file_another_session_made' (836) panicked at crates\horadric-hooks\src\checkpoint.rs:847:9:
  assertion `left == right` failed
  left: Failed("git for-each-ref: fatal: not a git repository (or any of the parent directories): .git")
  right: NotOwned(["x.rs"])
  note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
  failures:
  checkpoint::tests::a_failed_write_leaves_the_file_another_session_made
  test result: FAILED. 98 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.86s
  error: test failed, to rerun pass `-p horadric-hooks --lib`
  ```
  Escalated: opus, from sonnet: checks red on `cargo test --workspace`
  Warriv: Model: haiku
  Warriv: The red was the checkpoint git-init temp flake (a_failed_write_leaves_the_file_another_session_made, not a git repository), not this branch: it only touches CLAUDE.md, RELEASING.md and two new docs. Cherry-pick 40f9f09 and rerun the checks through heavy; change no code. If the flake repeats, rerun once more. Haiku, since it is a docs cherry-pick.
  Warriv: Stopped undone but waits on its own clippy and tests in heavy, commit cherry-picked. Own work, no tell.
  Outcome: haiku, 33m, green, 1.0M tokens
- [x] Make checkpoint tests' git init not copy hook templates @make-checkpoint-tests-git-init-not-copy-8247
  Model: haiku
  Two landings went red today on the same flake: repo() in crates/horadric-hooks/src/checkpoint.rs (tests, near line 708) runs git init -q in %TEMP%, and git fails copying ucrt64/share/git-core/templates/hooks/pre-push.sample. Pass --template= (empty) so git copies no templates, in this helper and any other test helper that runs git init in a temp dir (grep the workspace for "init" in tests). Check: cargo test -p horadric-hooks a few times in a row stays green.
  Filed by Warriv: the flake turned two landings red on 2026-10-29.
  Outcome: haiku, 143m, landed, 2.9M tokens
- [x] Sessions waiting on their own checks are not nudged or counted as stopped @sessions-waiting-on-their-own-checks-are-12436
  Model: opus
  Seen 2026-10-29: 8 quest sessions counted as stopped without done or blocked, and several nudged with 'If you are finished...', while they waited on their own cargo checks queued in horadric heavy (run_in_background, often with Monitor). Loot::waiting_on_own_work in crates/horadric-core/src/rarity.rs (since e81cbca) should cover this, so find where it leaks: a task notification from a Monitor event or an earlier task may lower outstanding to 0 while the check still runs (ended_tasks), Monitor's waiting flag is cleared by any prompt, and the look back's stop count may not ask waiting_on_own_work at all. Fix each leak with a test.
  Part: heavy and the session prompt, in crates/horadric/src (heavy command): sessions passed --timeout to heavy twice today and lost a whole queued run; accept and ignore it, or say in one line that the Bash tool's timeout is the one to set. Test runs were also killed for low memory three times on 'Blocked quests say why'; heavy should wait for free memory before cargo test, or rerun once by itself when killed.
  Part: chasing main, in crates/horadric-ui/src/runner.rs and worktree.rs: 'Blocked quests say why' merged main and reran all three checks five times because main moved during each run. When what came in from main touches no file the branch changed, landing should not need the session to rerun everything. Read lore on land-while-alive (6576c9d) first, which may already do this for quests started after it.
  Filed by Warriv from the 2026-10-29 look back.
  Warriv: Stopped undone while two of its own background agents (heavy fix, landing without rerun) still run, the very leak it fixes. Own work, no tell.
  Assumed: after a land whose main move touched none of the branch's files, land() reports the new main commit as the one the checks passed on (they ran on the commit before it); also changed the CLAUDE.md worktree rule to rerun checks only when main's new changes overlap the branch's files.
  Warriv: Warriv: stopped undone again, landing part green, still waiting on its own heavy agent. Own work, no tell.
  Outcome: opus, 55m, landed, 7.7M tokens
- [x] Land the stray session.rs edit: a Stop with background work stays Working @land-the-stray-session-rs-edit-a-stop-18725
  Model: sonnet
  Since 01:18 on 2026-10-29 the main checkout holds an uncommitted edit to crates/horadric-core/src/session.rs that no quest owns (see it with: git -C C:/Users/morte/Documents/Github/horadric.dev diff crates/horadric-core/src/session.rs). On Stop it keeps the phase Working while self.loot.outstanding > 0, with the test a_stop_with_a_background_shell_running_is_not_done. It has ridden every Ship Local since. Apply that diff in your worktree, check it fits what edd8483 (sessions waiting on their own background work) already does, keep it if it does, run the three checks and land it. Do not touch or revert the file in the main checkout; once yours lands, its copy there matches main.
  Assumed by Warriv: committing a tested, already shipped edit is easier to undo than letting it ride unowned.
  Outcome: sonnet, 10m, landed, 482k tokens
