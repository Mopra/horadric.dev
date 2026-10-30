# Done quests

Moved here from .horadric/tasks.md a few days after they were done. Horadric's quest log still shows them.

## SSH hosts

- [x] SSH: read hosts from config.json and open an SSH terminal from the project menu @ssh-read-hosts-from-config-json-and-open-53027
  See "SSH hosts" in docs/PLAN.md. `"hosts": [...]` in `.horadric/config.json`, each anything `ssh` takes.
  "SSH to <host>" in the project menu, one entry a host, starts a plain terminal running the Windows `ssh <host>`.
  Own glyph, host as the second line until the remote sets a title. Exit 0 closes it; 255 keeps the pane with the error and pauses the tile, a click reconnects.
  Pure and tested: reading hosts, the command line, the exit code rule. Verify on screen against localhost or a real server.
- [x] SSH: tell every session in a project with hosts about them @ssh-tell-every-session-in-a-project-with-53408
  See "Telling the agent" in docs/PLAN.md. Through `--append-system-prompt` on start and resume, joined with the task list's prompt into one.
  Say to use `ssh <host> '<command>'` with `-o BatchMode=yes`. Check on screen which `ssh` the agent's Git Bash runs and whether it reaches the Windows ssh-agent; say which to use in the prompt if it matters.
  Verify with `horadric run -- -p "Run uptime on <host>" --model claude-haiku-4-5-20251001`.
- [x] SSH: "Add host" in the project menu with suggestions from ~/.ssh/config @ssh-add-host-in-the-project-menu-with-53612
  Asks with the app themed input (see the task list item), and offers the `Host` names in `~/.ssh/config` that have no wildcard (skip `Match` blocks). The parser is pure and tested.

## The task list

- [x] Replace the Win32 text dialog with an app themed input @replace-the-win32-text-dialog-with-an-app-53837
  `ask::text` in `crates/horadric-ui/src/ask.rs` is a stock Win32 dialog (system font, system chrome, OK and Cancel), and it looks nothing like the rest of Horadric. The tasks tile's plus uses it for a new task, and the tile menu uses it to rename a session.
  Draw our own in Direct2D and DirectWrite with the theme's colours and font: an input in place (in the tasks tile for a new item, on the tile for a rename) or a small borderless popup beside it. Enter confirms, Esc cancels, clicking outside cancels.
  A new task can take notes as well as a title, since the notes go to the agent. Build it once and use it everywhere `ask::text` is called, including "Add host" from the SSH items. Verify on screen in dark and light.
- [x] Show a project's tasks tile when it has a list but no session @show-a-project-s-tasks-tile-when-it-has-a-54571
  Today the tile only exists while the project has a cluster, so a list with nothing running is invisible and its runner does not run. See "The task list", Not done yet.
- [x] Let the runner wait for the five hour limit to reset and go on @let-the-runner-wait-for-the-five-hour-54875
  The usage window already knows the limit. When it runs out mid list, hold the runner and start the next item after the reset instead of stopping.

## Step 4: worktrees and the git glance

- [x] Resolve the project key to the parent repository @resolve-the-project-key-to-the-parent-55611
  The key is the working directory today, so every worktree would become its own cluster. Use `git rev-parse --git-common-dir`. Sessions outside a repo keep working. This comes before worktrees.
- [x] Give each session its own git worktree, optional per project @give-each-session-its-own-git-worktree-55753
  See "Step 4" in docs/PLAN.md. Branch named from the session name. `setup` commands in `.horadric/config.json` run in each new worktree (gitignored files do not come along), and a port range per worktree so dev servers do not collide.
  A project can turn it off and keep the shared working tree.
- [x] Show changed files with plus and minus counts per worktree @show-changed-files-with-plus-and-minus-56503
  `git diff --numstat`, uncommitted versus committed, plus a button that opens the worktree in VS Code.
- [x] Let the runner hold several items at once, one worktree each @let-the-runner-hold-several-items-at-once-57140
  `parallel` in `config.json`. The list then lives in the main working tree only. Keep the fuses: at most one start per project every 10 seconds, never resume a paused session.

## Before daily use

- [x] Find claude.cmd from an npm install, not only claude.exe @find-claude-cmd-from-an-npm-install-not-57557
  Needs `cmd.exe /c` and its own quoting rules. See "Not in any step yet" in docs/PLAN.md.
- [x] Propose how sessions can survive Horadric quitting or crashing @propose-how-sessions-can-survive-horadric-57680
  The consoles live in the Horadric process, so a crash ends every agent. Surviving it means the consoles in a separate process, which is a real decision.
  Do not build it. Write the options and a recommendation into docs/PLAN.md, then report blocked so the human decides.
- [x] Resume the running sessions after a crash @resume-the-running-sessions-after-a-crash-73061
  Option A in "Sessions that outlive Horadric" in docs/PLAN.md. Keep `running` in state.json all the time, and on a start after an unclean exit resume those sessions as `app --reload` does. Keep the fuses: once each, never a session already live.
- [x] Run each session's console in its own host process @run-each-session-s-console-in-its-own-73278
  Option B in "Sessions that outlive Horadric" in docs/PLAN.md, with every point listed there: named pipe per session, output ring and replay, versioned five message protocol, breakaway job, orphans, dev pipe names, and reload that no longer waits.
  Tray Quit asks whether to keep sessions running, "keep running" the default when any is mid turn. Verify on screen: kill the dev UI, start it again, every session is where it was. Count `claude.exe` processes after.
- [x] Hear commits when the repository root is above the project folder @hear-commits-when-the-repository-root-is-74668
  A session started in a subfolder of a repo does not see commits, since the index is outside what the files tile watches. See "The files tile", Not done yet.
- [x] Let the columns stand on a monitor other than the primary one @let-the-columns-stand-on-a-monitor-other-74815
  They use `SPI_GETWORKAREA` of the primary screen. Needs a way to pick the screen, kept in state.json.

## Terminal and viewer

- [x] Place the IME composition window at the terminal cursor @place-the-ime-composition-window-at-the-75048
- [x] Mouse reporting to programs in the terminal @mouse-reporting-to-programs-in-the-75357
- [x] Cursor blink and a configurable font family in the terminal @cursor-blink-and-a-configurable-font-75607
- [x] Diff gutter and search in the file viewer @diff-gutter-and-search-in-the-file-viewer-76121
  Mark the changed lines of a modified file in the gutter, and a search like the terminal's Ctrl+Shift+F. See "The file viewer", Not done yet.

## Step 5

- [x] Start with Windows from the tray, and a way to show collapsed terminals @start-with-windows-from-the-tray-and-a-76703
  Autostart must stay off for dev instances.
- [x] Signed updater, lifted from Purrch @signed-updater-lifted-from-purrch-76788
  See `../purrch.fun/src-tauri`. Plan it against the existing `reload` hand over before building.
- [x] Updater: sign and verify a release manifest with CNG P-256 @updater-sign-and-verify-a-release-76903
  See "The updater" in docs/PLAN.md. `latest.json` with version, notes, a SHA-256 per binary and one signature over canonical bytes. `BCryptHash` and `BCryptVerifySignature` through the `windows` crate, no new dependency.
  `horadric release keygen` and `horadric release sign <dir>`. Tests: manifest bytes, a good signature passes, a flipped byte or a changed version fails, an older version is not an update. Tests use a key made in the test, never the real one.
- [x] Updater: generate the real key and have the human back it up @updater-generate-the-real-key-and-have-77187
  Run `horadric release keygen`, put the public half into the source, commit. Then report blocked: the human must back up `%USERPROFILE%\.horadric\updater.key` before any build with that public key ships, since losing it strands every install.
- [x] Updater: check for a newer release over WinHTTP and offer it in the tray @updater-check-for-a-newer-release-over-77448
  See "The check" in docs/PLAN.md. On start, daily, and a tray item "Check for updates". Verified manifest, newer version, then a tray item "Update to <version>". Failures logged, said only when the check came from the tray. Dev instances check `HORADRIC_UPDATE_URL` only.
- [x] Updater: download, verify the hashes, and hand over through reload @updater-download-verify-the-hashes-and-77909
  See "The install" in docs/PLAN.md. Into `%LOCALAPPDATA%\Horadric\updates\<version>\`, both hashes checked before anything runs, then the same `Reload` request `horadric reload` posts.
  Verify with the fake install folder from the reload test and a local server serving a release signed by a test key (`HORADRIC_UPDATE_KEY`, debug builds only). Check the rollback too, and count `claude.exe` after.
- [x] Updater: publish the first release and write RELEASING.md @updater-publish-the-first-release-and-78563
  Bump the workspace version, `cargo build --release`, `horadric release sign`, `gh release create v<version> --draft` with the three files. Write RELEASING.md like Purrch's: the key, cutting a release, publishing is shipping. Report blocked for the human to publish the draft.

## Ship

- [x] Commit everything to main and ship a new build @commit-everything-to-main-and-ship-a-new-79061
  Always the last item. Merge every finished branch and worktree into main, and commit anything left in the working tree that belongs there. Leave nothing half done on main: if an item's work is not finished, say so and report blocked instead of shipping.
  Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. All three must pass. Push main to origin.
  Then ship as CLAUDE.md says, under "Developing Horadric from inside Horadric": `cargo build --release`, say what is being shipped, run `horadric task done`, and run `target\release\horadric.exe reload` as the very last command. Do nothing after it. Afterwards, `%APPDATA%\Horadric\reload.log` says whether the new build came up.
- [x] Skip a UTF-8 BOM when reading tasks.md and config.json (PowerShell 5 writes one, and the first item and the mode are lost) @skip-a-utf-8-bom-when-reading-tasks-md-79250
- [x] Offer to merge a finished parallel item's branch into main @offer-to-merge-a-finished-parallel-item-s-79399
- [x] Check the tray Quit question on screen: Yes keeps the hosts running, No stops them, Enter picks keep only when a session is mid turn @check-the-tray-quit-question-on-screen-80560
- [x] Check "Tiles on screen" in the tray menu on screen: picking each screen moves the columns, and the stage stays unless covered @check-tiles-on-screen-in-the-tray-menu-on-80944
- [x] Check Show terminal in the tray menu on screen: close the stage with its cross, pick it, the same project comes back @check-show-terminal-in-the-tray-menu-on-81134
- [x] Keep a fake install (own APPDATA, LOCALAPPDATA and port) from pointing the real Start with Windows value at itself on its first start @keep-a-fake-install-own-appdata-81272
- [x] Merge release-0.2.0 into main before shipping: it already merges every finished branch (conflicts resolved) and bumps to 0.2.0 @merge-release-0-2-0-into-main-before-81393
- [x] Stay a while and listen: a catch-up of what happened while you were away (see "Stay a while and listen" in docs/PLAN.md) @stay-a-while-and-listen-a-catch-up-of-50523

## Animations

- [x] A finished turn lands with weight: the key bounces once and a loot beam rises from it
  "Loot drops" in docs/IDEAS.md. The beam is light rising off the key, fading as it goes, once per finished turn. It pairs with Identify: the lamp stays lit until read, the beam is the moment it lands. No sound yet.
- [x] A waiting tile grows more urgent the longer it waits: the breath quickens after 1, 5 and 15 minutes
  The breath's period shortens in steps, never faster than a calm pulse. Pure and tested.
- [x] A resumed tile wakes up: its key unlatches and rises, its lamp warms from dark glass to its colour
- [x] An ending session powers down: its lamp goes out like a CRT, its key sinks and fades, the tiles below close the gap
  A tile that leaves stays drawn as a ghost until it has gone, and the cluster keeps its height until then.
- [x] Subagents as sparks orbiting the working lamp, one per running subagent
  Count SubagentStart and SubagentStop per session (not saved). The state machine still ignores them for the phase.
- [x] The tool icon turns over when the tool changes
- [x] The activity trace scrolls smoothly instead of stepping
- [x] The context bar fills like liquid, with a shimmer past 75 %
- [x] A finished task strikes through left to right, then its row folds away
- [x] A task taken by a session sends a light from its row to the new tile
- [x] A cluster's accent glows faintly while any of its sessions works
- [x] A new cluster rises into place and fades in
- [x] While a cluster is dragged, the others in its column make room for it @horadric.dev-42672
- [x] Keep a dev instance from sweeping worktrees it did not make (a test session in a worktree of horadric.dev removed other agents' clean merged worktrees) @keep-a-dev-instance-from-sweeping-52869

## Leftovers from the plan

- [x] Scroll the file viewer sideways instead of wrapping long lines @scroll-the-file-viewer-sideways-instead-57720
  See "The file viewer" in docs/PLAN.md, "Not done yet". Shift+wheel and a horizontal wheel scroll; the diff gutter and search marks stay in place. Verify on screen.
- [x] Round the stage's panes themselves, not only the glass inside them @round-the-stage-s-panes-themselves-not-58381
  See "The look" in docs/PLAN.md, "Not done". Panes are square child windows today. Verify on screen in dark and light, and that nothing clips the terminal text.
- [x] Stay a while and listen: list commits made under a finished item @stay-a-while-and-listen-list-commits-made-58738
  See "Stay a while and listen" in docs/PLAN.md, "Not done yet". The journal does not hear them today.
- [x] Stay a while and listen: a mark of its own for a background session's lines, and scrolling past "and N more" @stay-a-while-and-listen-a-mark-of-its-own-59170
  Same section as above. Both small; the scroll works like the tasks tile's wheel.
- [x] Kitty keyboard protocol in the terminal @kitty-keyboard-protocol-in-the-terminal-59815
  See "Not in any step yet" in docs/PLAN.md. Progressive enhancement flags, at least disambiguate and report event types. Check what alacritty_terminal already parses before writing any of it. Pure encoding, tested.

## Ideas

- [x] Item rarity colours: the key's name shows how a session ended @item-rarity-colours-the-key-s-name-shows-60762
  "Item rarity colours" in docs/IDEAS.md. White changed nothing, blue changed files, yellow changed files and tests passed, gold landed on main, green for a batch. Must not read as a phase: phase is light, project is accent. Pure rules for the rarity, tested.
- [x] Loot drop sounds: a short sound with the beam, a higher chime when work lands on main @loot-drop-sounds-a-short-sound-with-the-61500
  "Loot drops" in docs/IDEAS.md. Our own sounds, never Blizzard's: synthesise them, no new dependency. Off by default or a tray toggle, silent under Windows' focus assist.
- [x] The stash: a three by three grid for sessions kept for later @the-stash-a-three-by-three-grid-for-62201
  "The stash" in docs/IDEAS.md. A stashed session is paused and out of the columns; a click brings it back as it was. Kept in state.json.
- [x] Tal Rasha's tombs: start one task in several worktrees at once and pick the winner @tal-rasha-s-tombs-start-one-task-in-63068
  "Tal Rasha's tombs" in docs/IDEAS.md. Builds on parallel items and worktrees in Step 4. Picking one merges or keeps its branch; the others fade out and their worktrees are removed. Keep the runner's fuses: never more than 8, one start per 10 seconds.
- [x] Transmute: the cube and its first recipes @transmute-the-cube-and-its-first-recipes-64088
  "Transmute" in docs/IDEAS.md. Drop tiles on a cube in the column or stage. Recipes: two finished sessions start a reviewer on both diffs; a session and main merges its branch; three idle sessions close with a one-line summary each. Recipes are small named actions, pure matching and tested.
- [x] The transmute animation: tiles swirl into the cube and something comes out @the-transmute-animation-tiles-swirl-into-65627
  "The transmute animation" in docs/IDEAS.md. Respects Windows' animation setting, costs nothing at rest, like the other motion in "The look".
- [x] Runewords: named sequences a session can be given, such as test, review, merge @runewords-named-sequences-a-session-can-66193
  "Runewords" in docs/IDEAS.md. Decide in the plan whether recipes and runewords are one system, and build on Transmute.
- [x] Experience: XP per merged commit and a level in the tray menu @experience-xp-per-merged-commit-and-a-67192
  "Experience" in docs/IDEAS.md. Counted from git, not kept by hand, so it survives a reinstall. Pure and tested.
- [x] The Cow Level: a hidden easter egg @the-cow-level-a-hidden-easter-egg-67499
  "The Cow Level" in docs/IDEAS.md. A hidden way in and a portal that opens. It must never start agents or get in the way of reading a tile.
- [x] Quests: rename Tasks to Quests, with the old names still read @quests-rename-tasks-to-quests-with-the-67925
  "Quests" in docs/IDEAS.md. The tile, the command (`horadric quest done`, with `task` still accepted) and the file (`.horadric/quests.md`, with `tasks.md` still read). Last of the ideas, since every item above runs from this list: make sure the list that is running keeps working through the rename.
- [x] Play the transmute animation when a batch closes @play-the-transmute-animation-when-a-batch-68131
- [x] Check a runeword with a real claude: test and review runes, the reviewer writing its file, and a merge that conflicts @check-a-runeword-with-a-real-claude-test-68350
- [x] Let a session read its review without a permission prompt: allow the reviews folder in the settings Horadric passes (Read for the session, Write for the reviewer) @let-a-session-read-its-review-without-a-69740
- [x] Keep state.json when it fails to parse: set the bad file aside and drop only what is bad, instead of losing every session and overwriting it @keep-state-json-when-it-fails-to-parse-69853
- [x] A told rune whose turn the human interrupts never ends: decide whether the runeword waits, re-tells, or stops (see Runewords in docs/PLAN.md) @a-told-rune-whose-turn-the-human-69943

## Codex and Grok Build

- [x] Spike: install the Codex CLI and answer the plan's open questions about Codex and Grok Build @spike-install-the-codex-cli-and-answer-72784
  Step 1 of the plan. Test with a dev instance. Answer: does a Codex command hook see `HORADRIC_SESSION` from the parent; does Codex read `auth.json` again while running and write it back on refresh; does `codex resume <id>` take a session started elsewhere; does a Grok `http` hook expand `$HORADRIC_SESSION` in its URL on Windows; which Grok event means waiting on you, and does `Stop` come at every turn's end.
  Also check what the installed Horadric receives from Grok today: Grok reads the hooks in `~/.claude/settings.json`, so every `grok` already posts to the Claude hook. Does it send the header?
  Never switch or copy the real logins in `~/.codex` or `~/.grok`: read them, do not write them. Write the answers into the plan's table and steps, and change the steps below if an answer calls for it. No code beyond throwaway scripts.
- [x] Add an Agent to the core model: Claude, Codex or Grok, saved on every session @add-an-agent-to-the-core-model-claude-73414
  The seam in "The shape". Missing reads as Claude, so old state loads. It answers the program, resume arguments and default flags per agent; the registry, phases, tiles and journal keep working on `HookEvent` and never learn the agent's name. Pure and tested. Nothing changes for Claude.
- [x] Sign in to Codex and Grok again, then finish the spike's live checks @sign-in-to-codex-and-grok-again-then-73593
  Needs the human: both logins here have expired. Run `codex login`, then `grok update` (0.2.22 is installed, 1.0.44 is out) and `grok login`. Then, with throwaway hooks and `-p` or `exec` turns: Codex's `Stop`, `PermissionRequest` and `Interrupt` on real turns; whether Grok 1.x still refuses an `http` hook to 127.0.0.1; which Grok event means waiting on you; whether `stop` comes at every turn's end; whether Grok reads a changed `auth.json` without a restart. Write the answers into the plan's table and step 1.
- [x] `horadric hook`: the command hook Codex and Grok both post through @horadric-hook-the-command-hook-codex-and-16517
  Step 2. Neither can post over HTTP: Codex has command hooks only and Grok refuses loopback URLs. The command reads the event on stdin and posts it with the tag from its environment and the agent's name, like `horadric status`. It is the one process per event Horadric spawns, so it starts fast, has a short timeout, fails open, and does nothing without `HORADRIC_SESSION`. Turns a Codex or Grok payload into a `HookEvent`. Pure and tested.
- [x] Codex: pass its hooks on the command line and read its events @codex-pass-its-hooks-on-the-command-line-16670
  Step 3. `-c hooks.<Event>=[...]` for each event plus `--dangerously-bypass-hook-trust` when Horadric starts `codex`, so `~/.codex` is never written and a `codex` outside Horadric runs no Horadric hook. `PermissionRequest` is waiting, `Stop` done, `Interrupt` idle, a `SessionEnd` after a turn with no `Stop` is a failed turn. Pure and tested.
- [x] Codex: start, resume and pick defaults for a Codex session @codex-start-resume-and-pick-defaults-for-16984
  Step 5 for Codex. `codex resume <id>` for a paused tile (it takes Codex Desktop sessions too), `-m` and `-c model_reasoning_effort=` for the defaults, History from `~/.codex/sessions`. Verify on screen with a real `codex`, and count its processes.
- [x] Grok: install its command hook and read its events @grok-install-its-command-hook-and-read-17542
  Step 4. `~/.grok/hooks/horadric.json`, a `command` hook calling `horadric hook`, added by `install` and removed by `uninstall`, never by a dev instance. Phases from the live checks above. If Grok 1.x accepts a loopback URL after all, the listener drops a Grok payload that arrives on the Claude path by its shape. Pure and tested.
- [x] Grok: start, resume and pick defaults for a Grok session @grok-start-resume-and-pick-defaults-for-a-17845
  Step 5 for Grok. The plus button and `horadric new --agent grok` start it when `grok` is found, `--resume <id>` carries a paused tile on, the model and effort defaults pass as `-m` and `--effort`, a tile shows a small mark for its agent, and History lists Grok's own conversations from `~/.grok/sessions`. Verify on screen with a real `grok`, and count its processes after any change to how sessions start.
- [x] Limits per provider in the usage window, Codex's from its transcripts @limits-per-provider-in-the-usage-window-18547
  Step 6. A named screen per provider in use (Claude, ChatGPT, Grok). Codex's limits from the newest `token_count` in the transcript its hooks point at, read on each `Stop`: `rate_limits.primary` and `secondary`, `used_percent` and `resets_at`. The settings follow the agent, each with its own models and efforts. Grok shows no limits until a source turns up. Verify on screen.
- [x] Switch ChatGPT and xAI accounts the way Claude accounts switch @switch-chatgpt-and-xai-accounts-the-way-19232
  Step 7, built on "Accounts" in docs/PLAN.md. Codex: the whole `auth.json` is the login, `account_id` the account, the email from the `id_token`; force the file store, and stop and resume its sessions, since a running Codex treats another account on disk as a permanent error. Grok: its `auth.json` entry is the login, and Grok reads it again by itself, so a switch is only the write. A switch touches one provider's sessions. Test with a fake `CODEX_HOME` and `GROK_HOME`, never the real logins.
- [x] Resume Claude sessions without resending the prompt they were started with @resume-claude-sessions-without-resending-20088
- [x] Keep Grok from showing an SSRF error for the Claude hook it borrows @keep-grok-from-showing-an-ssrf-error-for-20174
  Grok runs the `http` hooks in `~/.claude/settings.json` and prints "blocked by SSRF protection" in its TUI at every event. `[compat.claude] hooks = false` in Grok's config stops it, but that is the user's file; look for a flag or environment variable Horadric can pass when it starts `grok` instead.
- [x] Mouse-over tooltips @mouse-over-tooltips-42172
  Mouse-over for all items in Horadric, describing what they do.
- [x] Tooltips on a pane's header: stash, zoom, close, the browser bar and its tabs, through tip.rs once the browser tabs work in pane.rs is committed @tooltips-on-a-pane-s-header-stash-zoom-71623
- [x] Quest Log @quest-log-43924
  Quests my ikke auto-start når du venstre-klikke på dem i listen, man skal se quest loggen først for at få en ide om hvad questen går ud på inden man accepter den.
- [x] Quest Giver @quest-giver-43923
  Knap ved Tasks listen der selv giver bud på hvad der kunne laves af tasks for det aktuelle projekt.
- [x] Ship public: release everything on main to every install @ship-public-release-everything-on-main-to-20289
  Always the last item. Everything finished on main goes out, not only the Codex and Grok work. Follow RELEASING.md and "Ship public" in CLAUDE.md.

## Discord Activity

- [x] Discord: a client for the Rich Presence pipe @discord-pipe-56616
  The frame codec (opcode and length, little endian, then JSON), the handshake, `SET_ACTIVITY` and clearing, all pure and tested, in a module of their own. Then the client on its own thread: try `\.\pipe\discord-ipc-0` to `-9`, handshake and wait for `READY`, keep only the latest presence, retry every 30 s while Discord is closed and say nothing about it, at most one update every 4 s, never send an unchanged one, clear before closing.
  Its interface is a handle the app keeps with `set(Option<Activity>)` and a `stop` that clears; the `Activity` type is shared with the presence quest, so define it first, commit it at once, and say in its doc comment that the other quest builds on it.
  The client id is the constant `1555242626897416212` (the human's "Horadric" application), overridden by `HORADRIC_DISCORD_CLIENT_ID`. Discord runs on this machine: check it against the real client with a small example or test binary that sets and clears an activity.
- [x] Discord: what the presence says @discord-presence-56628
  The pure function from the sessions and the setting to an `Option<Activity>`, as "What it says" in the plan: the counts, the project only when names are allowed, the small image by the most urgent state, a start time that holds while any session works, None with nothing running. Tested for each. Wait for the client quest's `Activity` type to be on main (`git log`), or agree on it by reading its commit; do not make a second one.
- [x] Discord: the art for the presence @discord-art-56640
  First find out whether Rich Presence takes an `https` URL for `large_image` and `small_image` today; if it does, they live in the repository and the keys are their URLs. Make the large image (Horadric's own mark) and one small image per state (waits, working, idle) in the lamps' colours, 512 by 512 PNG, drawn the way the app draws them so they match. Put them in `docs/discord/` with a short note of the key for each, and the steps for the human to upload them if URLs do not work.
- [x] Discord: the "Show on Discord" setting in the tray @discord-tray-56652
  Off (the default), "Without project names", "With project names", checked as chosen, kept with the other app settings so a dev instance has its own. Nothing is sent yet: expose the choice where the wiring quest can read it and hear it change. Verify the menu on screen with a dev instance.
- [x] Discord: wire the presence into the app @discord-wire-57355
  Needs the four above on main. Keep a client while the setting is on, hand it the presence whenever the registry or the setting changes (it drops what did not change), and clear on Off, quit and reload as the plan says. Count that the UI thread never waits on the pipe. Check with a real Discord and a dev instance, the installed one left off.
- [x] Discord: check it on screen with a real Discord and write it into the README @discord-check-57759
  With a dev instance: each setting, a session working, waiting and done, the elapsed time holding across turns, names hidden and shown, quit and reload clearing it. Screenshot the profile. Add a short part to README.md and mark the plan section done.
- [x] Discord: keep the clock of a run of work through a reload @discord-keep-the-clock-of-a-run-of-work-71634
- [x] Performance pass two: the leftovers in the plan @performance-pass-two-the-leftovers-in-the-71878
  See Performance in docs/PLAN.md, 'Left for later'. Each pane paint makes a new layer and geometry for its glass (glyphs.rs); characters missing from the terminal font get a DrawText each; the pane caption repaints on every spinner frame of the title; each cluster paint clones its sessions twice. Measure the UI thread on a dev instance before and after, as the first pass did.
- [x] Keep the browser page through a reload of Horadric @keep-the-browser-page-through-a-reload-of-72055
  See Browser pane in docs/PLAN.md: the page outlives its pane but is not yet kept over a reload, so every ship drops open pages. Save each project's address (and history position if WebView2 allows) before the handover and open them again in the new build. Verify with reload under HORADRIC_DEV=1.
- [x] Give Grok sessions the browser tools @give-grok-sessions-the-browser-tools-72801
  Claude gets horadric mcp through --mcp-config and Codex through -c mcp_servers; Grok has none because it keeps MCP servers in its own config (see Browser pane in docs/PLAN.md). Find a per-session way in (flag, env var, a config it reads), or else an entry Horadric owns in Grok's config. Check with a real grok session that the browser_* tools reach its project's pane.

## The Runetome

- [x] Runetome engine: say, keys and run steps, stones that need no session, and the global file @runetome-engine-say-keys-and-run-steps-73961
  See "The Runetome" in docs/PLAN.md: Steps, Casting, Where stones live. In `horadric_core::runeword`: the object form `{"steps": [...]}` beside today's list form, `say`, `keys` (with `runeword::keys` parsing `"Esc"`, `"Ctrl+C"`, `"/clear{Enter}"`), `run` (with `"show": true`) and the three runes; `%APPDATA%\Horadric\runewords.json` read beside `.horadric/config.json`, both read again when they change; a stone that does not parse kept with its reason. Casting: `keys` written into the session's terminal and done at once, `run` by `cmd /c` in the project folder or the session's worktree, a non zero exit stopping it with a toast of its last line, and a runeword of only `run` steps cast on the project with no session, saved in `state.json` beside the sessions so it goes on through a reload. Leave the tile menu's Runeword submenu working until the tome replaces it. Pure and tested; check a `keys` and a `run` step on a dev instance with `cmd.exe` as the agent.
- [x] Rune stones and the Runesmith's pieces: the carved glyph, the generated name, the smith prompt and `horadric runeword list` @rune-stones-and-the-runesmith-s-pieces-73971
  See "The Runetome" in docs/PLAN.md: A stone, The empty stone. `runeword::name` (two to four of the 33 rune names from a hash of the label) and `runeword::carve` (glyph strokes from the same hash), pure and tested. A function in horadric-ui that draws one stone in Direct2D at a given size: a rough rounded slab lit from the top left like the cube, the glyph cut into it, a gold glow while it runs, cracked when it does not parse, and an empty stone. `runeword::smith_prompt` (tested), saying what a stone is, the step kinds, both files and their shape, and to ask the human what it should do and for which projects, then check with `horadric runeword list`. The `horadric runeword list` command printing every stone a project has, with its name, label, steps and any parse error. No tile yet: check the drawing on screen however is quickest, in dark and light.
- [x] The Runetome tile: stones in a project's cluster, cast by click or drag, and the empty stone starting a Runesmith @the-runetome-tile-stones-in-a-project-s-73981
  Needs the two quests above on main. If either is not there yet, mark this one blocked naming which, so it can be accepted again once it is. See "The Runetome" in docs/PLAN.md. A tile in each project's cluster beside the quest log, built in stones first, then the project's, then the global ones, then the empty stone. Tooltip (tip.rs) with the runeword name and the steps. A click casts on the focused stage session when it is this project's, else asks which session; a stone of only `run` steps casts at once; dragging a stone onto a tile or pane casts on that one. While running the stone glows with its step and a click offers Stop. The empty stone starts a session named "Runesmith" on the stage with the smith prompt, as the quest giver does. The changed since last cast mark on project stones. Drop the tile menu's Runeword submenu, keep its Stop. Check on screen with a dev instance: each step kind, a sessionless stone, a drag, and a real `claude` Runesmith (Haiku) making a stone that appears on the tile. Count `claude.exe` processes after. Then add the Runetome to README.md and mark the plan section built.
- [x] Point horadric runeword list at runeword::parse @point-horadric-runeword-list-at-runeword-77455
  Once the Runetome engine is on main, have crates/horadric/src/runeword.rs list stones through the engine's runeword::parse (and its global file path) instead of its own reading of the two forms, so the list and the tile always agree on what cracks.
- [x] Blocked quests resume by themselves once what blocked them is resolved @blocked-quests-resume-by-themselves-once-75845
  Today a [!] quest waits on the human forever, and the runner stops the whole list at it (Next::Stuck in tasks.rs). Let a blocked quest say what it waits on, in a form Horadric can check, and have the runner check it on each look. Kinds worth having: another quest done (by title, the common case, see the Runetome tile quest), a commit or branch on main, a file existing, a command exiting 0, a time. Something like horadric quest blocked "why" --on-quest "title" | --on-file path | --on-cmd "..." | --until time, kept on the item line or in its notes so the file stays the state. When the condition holds: if the holding session is alive, send it a go on message the way go_on does after a usage limit reset and mark the quest [/] again; if the session is gone, start the quest again (start_again). A blocked quest whose condition is not met yet should not stop the list: the runner skips past it to the next open quest, but only for machine checkable waits; a plain why with no condition still needs the human and still stops the list. Update the system prompt so agents use the conditional form when they can, and the board row to show what it waits on. Tests for the parsing, the condition check and the runner choice (pure parts in horadric-core). Check on screen with a dev instance: block one fake quest on another, finish the other, watch the first resume. Count claude.exe after, since this starts agents by itself.
- [x] Let a stone of only keys be cast on a session that is casting another runeword, so a permission prompt can be answered from the tome without stopping it @let-a-stone-of-only-keys-be-cast-on-a-77466

## Runetome feedback

- [x] Remove a stone by right click on it @horadric.dev-78569
  Today there is no way to remove a runeword short of editing the JSON. A right click on a project or global stone offers "Remove <label>" (with a confirm), which deletes it from `.horadric/config.json` or `runewords.json`, keeping the rest of either file as it was. Built in stones cannot be removed, so the menu says so or leaves the item out.
- [x] Better built in stones that show what a stone can do @horadric.dev-78569
  "Test, merge", "Test, review, merge" and "Review, merge" are not useful. Replace them with a few that each show off a step kind and are worth a click on day one: keys, say chains, a run command.
- [x] Ask before casting, with "Do not ask again" @horadric.dev-78569
  A click on a stone asks whether to cast it, showing what it will do, with a "Do not ask again" check kept in state.json.
- [x] Review the runeword and Runetome implementation for other quality of life fixes @horadric.dev-78569
  Read the tile, casting and the engine end to end and fix what gets in the way. List what was found and done in the plan.

## Orchestration

- [x] After: dependencies in the quest log, and the runner starting only ready quests @after-dependencies-in-the-quest-log-and-60905
  Step 1 of "Orchestration" in docs/PLAN.md. In `horadric_core::tasks`: read `After: <title>` notes lines, match by exact title or unique start, ready only when every named quest is `[x]`, a name that matches nothing or several makes the quest not ready and the row says which, cycles are not ready and stop the list. `tasks::next` passes quests that are not ready and takes the first ready open one. `quest add "title" --after "other"` writes the line; `quest blocked "why" --on "title"` writes the same line, replacing `--on-quest` and its `{on quest: ...}` form (keep reading the old form). Board row reads "after <title>", dim, no lamp. Quest giver prompt says to add After: lines. Pure and tested; on screen with a dev instance and cmd.exe: two quests, the second after the first, auto mode, `quest done` on the first starts the second. Mark step 1 built in the plan.
- [x] Finished quests merge themselves into main when the checks pass @finished-quests-merge-themselves-into-60916
  Step 4 of "Orchestration" in docs/PLAN.md. In auto mode, when a quest in its own worktree is done: rebase on main, run the `"checks"` commands from `.horadric/config.json` in the worktree (none means no checks), fast forward main, then the worktree and branch go as today. One merge at a time per project. A conflict or a red check leaves the branch and adds a fix-up quest right below with the failing output in its notes and `After:` handled so quests after the finished one also wait for the fix-up (if After: is not on main yet, put the fix-up first in the list instead and leave a note). Review mode keeps today's click. Pure parts tested (the merge plan, the fix-up quest text); on screen with a dev instance on a scratch repository: one clean merge, one conflict, one failing check. Add `"checks"` for this repository's config (fmt, clippy, test). Mark step 4 built in the plan.
- [x] Up to 16 quests at once, paced by usage @up-to-16-quests-at-once-paced-by-usage-60926
  Step 5 of "Orchestration" in docs/PLAN.md. `MOST_PARALLEL` from 8 to 16; the mode menu offers one at a time, 2, 4, 8 and 16. The runner starts nothing while the fullest limit the status line last reported is at 90 % or more (pure, tested, next to `Limits::out_until`), with a toast once when it holds for that reason. Every other fuse stays, including one start per project every 10 s. Check on screen with a dev instance, cmd.exe and faked limits: 8 starts 10 s apart, and a limit at 92 % holds the next one. Mark step 5 built in the plan.
- [x] Quests in their own worktree run with permission prompts bypassed @quests-in-their-own-worktree-run-with-60936
  Step 6 of "Orchestration" in docs/PLAN.md. A quest session started by the runner or a click in its own worktree gets the agent's bypass permission mode (for claude, `--dangerously-skip-permissions` or `--permission-mode bypassPermissions`, whichever this codebase already uses for modes; Codex and Grok their equivalent, or the project's mode if none). Sessions in the main tree, the quest giver and Warriv keep the project's mode. Pure choice tested; on screen with a dev instance and a real claude on Haiku in a scratch repository: a worktree quest runs a shell command with no prompt, a main tree session still prompts. Count claude.exe after. Mark step 6 built in the plan.
- [x] Warriv: an orchestrator session woken by events, planning for the human @warriv-an-orchestrator-session-woken-by-62791
  Step 2 of "Orchestration" in docs/PLAN.md, as a full planner per "Decided on 2026-10-05". Needs the After: quest and the merge quest above on main. If either is not there yet, mark this one blocked with `--on-quest` (or `--on` once it exists) naming which, so it starts again when it lands. Events, fresh session per wake, the queued events, notes lines as memory, `quest tell "title" "message"`, the fuses (one per project, six wakes an hour, never woken by itself), `"orchestrator": true` to turn it on. Pure and tested: which events wake it, the budget, the first prompt, the queue. On screen with a dev instance: a fake quest blocked with a question its notes answer, Warriv wakes, tells the session, the quest goes on. Count claude.exe after. Mark step 2 built in the plan and add Warriv to README.md.
- [x] The human hears only what Warriv could not settle @the-human-hears-only-what-warriv-could-64366
  Step 3 of "Orchestration" in docs/PLAN.md. Needs the Warriv quest on main; if it is not, mark this blocked on it. With Warriv on, a quest shows red only when Warriv handed it on, its reason is that question, "N quests need you" counts only those, and the notes say what Warriv tried. Tested where pure, checked on screen with a dev instance. Mark step 3 built in the plan.
- [x] quest done from a worktree marks the worktree's copy of the log @quest-done-from-a-worktree-marks-the-64376
  held() in crates/horadric/src/task.rs looks above cwd before HORADRIC_TASKS, so a worktree whose committed tasks.md has the holder line gets marked instead of the main tree's log, and the quest stays [/]. Prefer the main tree when HORADRIC_TASKS is set. Seen on the Warriv quest.

## macOS

- [x] Mac: the workspace builds and its tests pass on a Mac in CI
  cfg gates so every crate compiles for macOS, the state folder under Application Support, a macos job in ci.yml running clippy and the tests.
- [x] Mac: session hosts on a POSIX pseudo terminal
  `horadric-pty` gets a unix half: `forkpty`, a Unix socket under /tmp/horadric-<uid>, the same wire protocol. A test runs /bin/sh in a real pty through a host.
- [x] Mac: the command line
  `horadric`, `app`, `new`, `run`, `hooks`, `status`, `quest`, `serve`, `reload` on a Mac. The login shell's PATH for finding agents.
- [x] Mac: the app, the menu bar and the tiles
  NSApplication, NSStatusItem menu, one NSPanel a project drawn with Core Graphics from the shared layout, columns down the left.
- [x] Mac: the stage and its terminals
  One window, a pane a session, the grid drawn with Core Text, keys, IME, selection, copy, paste, scroll, resize.
- [x] Mac: sessions start, resume, end and survive the app
  Start in a folder, plain terminals, rename and end from the tile menu, attach to running hosts at start, resume saved sessions, Quit asks.
- [x] Mac: a smoke run in CI with snapshots of every window
  HORADRIC_SNAPSHOT draws each window to a PNG; CI posts fake sessions, starts a shell session and uploads the PNGs.
- [x] Mac: Horadric.app, install and uninstall
  Universal app bundle with an icon, signed ad hoc. `horadric install` copies it to ~/Applications, links the command, a LaunchAgent, the hooks. install.sh for curl.
- [x] Mac: the updater
  latest-macos.json signed on Windows, checked with the Security framework, the bundle swapped and the app reloaded.
- [x] Mac: release builds in CI and a public release
  A tag builds Horadric-macos.tar.gz; RELEASING.md says how it is signed and attached. Ship public.
- [x] Mac: the site and the README offer the Mac download
  install.sh served from horadric.dev, a Mac download beside Windows, the xattr line for browser downloads.

## macOS

- [x] Arcane Sanctuary: light up what agents do, and Tristram's sky @arcane-sanctuary-light-up-and-tristram-s-sky-90001
  Plan: docs/plan/the-arcane-sanctuary.md, steps 3 and 4 merged at the human's word. Built in main by session horadric.dev-67466 with subagents, beside the pane quest, then wired into the pane once it lands.

## Orchestration

- [x] Warriv: record wakes in the chronicle @warriv-record-wakes-in-the-chronicle-66066
  See 'Warriv in the quest log' in docs/PLAN.md, first bullet. New Happened records in horadric_core::chronicle for a wake starting (wake id, conversation id, events by kind and quest), each quest command Warriv runs (tell, note, add, blocked with quest and text) and a wake ending (settled, handed on with the quests, cut short). The app writes start and end from crates/horadric-ui/src/warriv.rs; the CLI writes the commands when warriv::is_warriv says the session is Warriv. Old chronicles must still read. Pure and tested. Check on screen with a dev instance and cmd.exe as the agent that a faked wake leaves the three kinds of lines in chronicle.jsonl. Mark the bullet built in the plan.
- [x] Warriv: its own lane and wake detail in the quest log @warriv-its-own-lane-and-wake-detail-in-66553
  See 'Warriv in the quest log' in docs/PLAN.md, second and third bullets. chronicle::graph places a dot per wake on a lane beside the trunk and a thin line to each quest the wake touched; pure and tested. Draw it in render/questlog.rs in a gold of its own. Clicking a dot opens the wake's detail: when, how long, the events, each command in order, how it ended with the questions handed on, and Read the session. Check on screen in dark, light and narrow with a dev instance and fake chronicle lines. Mark the bullets built in the plan.
  After: Warriv: record wakes in the chronicle
- [x] Warriv: a live line on the quests tile @warriv-a-live-line-on-the-quests-tile-66077
  See 'Warriv in the quest log' in docs/PLAN.md, last bullet. While Warriv is awake the quests tile shows 'Warriv: settling N' under the mode in the working colour; with its six wakes spent, 'Warriv rests until HH:MM' dimmed; nothing otherwise. The Desk in horadric_core::warriv knows the queue and the budget; the words are a pure tested function. Check on screen with a dev instance. Mark the bullet built in the plan.

## The caravan moves on its own

- [x] Aims: Warriv files the next quests when the log runs dry @aims-warriv-files-the-next-quests-when-66502
  Part 1 of "The caravan moves on its own". `Aim:` lines at the top of tasks.md, `horadric quest aim "text"` and `quest aim done "text"` (writes `Aim reached:`). On the stalled event with an aim open, Warriv's prompt carries the aims and tells it to file at most eight quests with After: lines, each with a `Filed by Warriv for: <aim>` note, or mark the aim reached. Never a title already in the log; three filing-nothing wakes in a row hand the aim to the human. No aim: stalled goes to the human as today. Pure and tested: aims read and written, the prompt, the title match. On screen with a dev instance, a scratch repository and a real claude on Haiku: an aim, an empty log, Warriv files quests and the runner starts them. Count claude.exe after. Mark part 1 built in the plan.
- [x] Warriv reviews: a review mode where Warriv reads first @warriv-reviews-a-review-mode-where-warriv-66512
  Part 2 of "The caravan moves on its own". A fourth mode in the mode menu. A quest going `[?]` wakes a reviewer with the quest, its notes and the branch's diff against main; it ends with `quest pass` (lands as the click does), `quest fix "title" "what"` (tells the holder, or files a fix-up below as a failed merge does) or `quest blocked --quest`. One reviewer a project at a time, the rest queued, outside the six wakes. Pure and tested: the event, the queue, the prompt, the new commands. On screen with a dev instance and a scratch repository: one quest passed and merged, one sent back. Count claude.exe after. Mark part 2 built in the plan and add the mode to README.md.
- [x] While you were away: one card when the human comes back @while-you-were-away-one-card-when-the-66563
  Part 3 of "The caravan moves on its own". After 30 minutes without input and then input, a card on the stage lists quests landed, a line per Warriv wake, errands run, and last the questions for the human each with a one line answer that does what `quest tell` does. Esc closes it. Nothing happened, no card. `chronicle::away(since)` builds it, pure and tested. Check on screen in dark, light and narrow with a dev instance and fake chronicle lines. Mark part 3 built in the plan.
  After: Warriv: record wakes in the chronicle
- [x] Errands: runewords with "every", armed once and cast on a clock @errands-runewords-with-every-armed-once-66522
  Part 4 of "The caravan moves on its own", the engine and run-only errands. `"every"` on a stone (`30m`, `1h`, `day 09:00`, `weekday 08:30`, a weekday and a time) parsed by `runeword::every`, next cast by `runeword::due`, a missed cast runs once at start. Arming from the tome with a hash of the steps in state.json, disarmed when they change. Fuses: one errand at a time a project, skipped at 90 % usage, stopped past `"for"` (default 30 min), a failure toasts once and marks the stone red. Stones with test, review or merge and `"every"` show cracked. Run-only stones cast as sessionless runewords. Pure and tested; on screen with a dev instance: a `run` errand every minute runs, is disarmed by a step change, and holds at a faked 92 %. Mark the engine built in the plan.
- [x] Errands with say steps run in a fresh session of their own @errands-with-say-steps-run-in-a-fresh-68273
  Part 4 of "The caravan moves on its own", the session half. A due errand with `say` steps starts a fresh session named after the stone, in Warriv's gold, in the main tree, the project's permission mode or bypass when the stone says `"mode": "bypass"` (arming shows that in red). Steps cast turn by turn, closed after the last Stop. `warriv::errand_prompt` (pure, tested) says what the quest commands are, that findings become quests with a `From:` notes line, and to file nothing whose From: is already in the log. `{since}` is the last finished cast. On screen with a dev instance, a scratch repository and Haiku: an errand every 2 minutes files one quest with a From: line and not the same one twice. Count claude.exe after. Add to CLAUDE.md that arming a publishing errand is the human's standing go ahead. Mark it built in the plan.
  After: Errands: runewords with "every", armed once and cast on a clock
- [x] Errands in the tome and the chronicle, and the Runesmith knows them @errands-in-the-tome-and-the-chronicle-and-69929
  Part 4 of "The caravan moves on its own", what the human sees. An errand stone has a thin ring that fills toward its next cast, dim when unarmed; hover shows the last cast, how it ended and the next. Each cast is a dot on Warriv's lane in the quest log with the quests it filed. `runeword::smith_prompt` explains `"every"`, `{since}`, arming and the From: rule. Check on screen in dark and light with a dev instance. Mark it built in the plan.
  After: Errands with say steps run in a fresh session of their own
  After: Warriv: its own lane and wake detail in the quest log
- [x] Propose a ship local when quests have landed @propose-a-ship-local-when-quests-have-66532
  Part 5 of "The caravan moves on its own". Three or more quests landed on main since the last ship local and the checks passed on main's head: a toast "N quests landed. Ship local?" whose click casts the project's "Ship Local" stone. Once per landing count. The rule is pure and tested; the last ship is read from the chronicle or reload.log, whichever is surer. Check on screen with a dev instance and fake landings. Mark part 5 built in the plan.
- [x] A pulse on the quests tile while Warriv works @a-pulse-on-the-quests-tile-while-warriv-66543
  Part 6 of "The caravan moves on its own". While Warriv, a reviewer or an errand runs, the quests tile's edge breathes slowly in Warriv's gold; still otherwise. Use the gold from "Warriv: its own lane" if it is on main, otherwise define it once in the theme and let that quest use it. No timer runs when nothing breathes. Check on screen in dark and light with a dev instance. Mark part 6 built in the plan.

## Warriv drives

- [x] The "Warriv drives" switch, "and ships public", and the stop key @the-warriv-drives-switch-and-ships-public-66970
  "The switch and the stop" in "Warriv drives". Both switches in state.json (they survive a reload), in the tray menu and the quests tile's mode menu; the tile reads "Warriv drives" in gold while on. Driving turns the orchestrator on for the project, lifts the six wakes an hour, and makes an errand due during the 90 % hold wait for the reset instead of being skipped. Tray item and Ctrl+Alt+W (Shift added on a dev instance) stop it: switch off, Warriv and errand sessions closed, the runner starts nothing new, quests in hand finish. Pure where it can be (what driving changes in the fuses); on screen with a dev instance and cmd.exe. Count claude.exe after. Mark it built in the plan.
- [x] Warriv's memory in .horadric/warriv.md @warriv-s-memory-in-horadric-warriv-md-66980
  Part 1 of "Warriv drives". Rules, Lately and Open. Every Warriv session (wake, round, reviewer, errand) reads it first and writes it last; its system prompt says how. Compacted by Warriv past about 200 lines. A human's answer to a handed-on question becomes a rule at the next wake. `warriv::memory_prompt` and the compaction rule pure and tested. On screen with a dev instance, a scratch repository and Haiku: a wake writes Lately, a human answer becomes a rule. Count claude.exe after. Mark part 1 built in the plan.
- [x] Warriv's rounds: the whole project looked at every hour while it drives @warriv-s-rounds-the-whole-project-looked-70758
  Part 2 of "Warriv drives". A round hourly, after each landing and when the human leaves, only while driving. One Warriv session a project: a round due while one is awake folds into it. The round prompt is the whole picture (log, aims, main's checks, the memory, open questions, errands due). New `horadric runeword cast "name"` so a round can cast a stone. A round with nothing to do writes one Lately line and closes. Pure and tested: when a round is due, the fold, the prompt. On screen with a dev instance and Haiku. Count claude.exe after. Mark part 2 built in the plan.
  After: The "Warriv drives" switch, "and ships public", and the stop key
  After: Warriv's memory in .horadric/warriv.md
  After: Aims: Warriv files the next quests when the log runs dry
- [x] Assume what can be undone, and a blocked quest no longer stops the list @assume-what-can-be-undone-and-a-blocked-68200
  Part 3 of "Warriv drives". While driving, quest sessions' prompt says to make reversible choices with a `quest note "title" "Assumed: ..."` line and go on, blocking only on what cannot be undone or only the human can know; Warriv's prompt the same. A quest blocked on the human is passed over and the list goes on. The away card lists every Assumed: line since the human left, each with a one line answer. Pure and tested: the prompt, the runner's choice. On screen with a dev instance and cmd.exe. Mark part 3 built in the plan.
  After: The "Warriv drives" switch, "and ships public", and the stop key
  After: While you were away: one card when the human comes back
- [x] Warriv ships local, and public when the switch says so @warriv-ships-local-and-public-when-the-72216
  Part 4 of "Warriv drives". A round ships local when main is green and quests landed since the last ship, by casting "Ship Local" in a session of its own; with "and ships public" on, ships public when the work is worth a release. The next round reads reload.log; a rollback or main's checks red twice holds shipping ("shipping held" on the switch) and files a fix-up quest; shipping resumes once it lands and checks pass. Update CLAUDE.md: the switch is the human's go ahead to ship. Pure and tested: when to ship, the hold. On screen with a dev instance: a faked rollback holds shipping. Do not ship the installed Horadric to test it. Mark part 4 built in the plan.
  After: Warriv's rounds: the whole project looked at every hour while it drives
  After: Propose a ship local when quests have landed
- [x] A Model: line per quest, written by Warriv, started by the runner @a-model-line-per-quest-written-by-warriv-66990
  Part 5 of "Warriv drives". `Model: haiku|sonnet|opus` in a quest's notes starts its session with that `--model` for Claude Code, ignored for other agents. Warriv's prompt says to write one on quests it files or meets without one, by difficulty, never changing a human's. Pure and tested: reading the line, the flag. On screen with a dev instance and a real claude: a Model: haiku quest runs on Haiku. Count claude.exe after. Mark part 5 built in the plan.
- [x] Errands on events: landed, shipped, away and back @errands-on-events-landed-shipped-away-and-69940
  Part 6 of "Warriv drives". `"on"` on a stone instead of `"every"`, armed the same way, cast at that event. Parsed in `runeword::parse`, pure and tested. On screen with a dev instance: an `"on": "landed"` run stone fires on a faked landing. Mark part 6 built in the plan.
  After: Errands with say steps run in a fresh session of their own
- [x] Spectator mode: the stage follows the work while the human is away @spectator-mode-the-stage-follows-the-work-68210
  Part 7 of "Warriv drives". While driving and away, the stage shows the project where something last happened, the pane mid turn focused, at least 20 s each, Warriv first. Only when the stage was the foreground window when the human left; never takes the foreground. First input restores the stage as it was. The choice of what to show is pure and tested. Check on screen with a dev instance and cmd.exe sessions in two projects. Mark part 7 built in the plan.
  After: The "Warriv drives" switch, "and ships public", and the stop key
- [x] A daily look back that files quests and rules @a-daily-look-back-that-files-quests-and-72226
  Part 8 of "Warriv drives". The first round after 04:00 reads the day's chronicle and the memory, files a quest per pattern (repeated conflicts, a flaky check, sessions that stopped without reporting, quests sent back twice) and writes lessons as rules. The pattern finder over chronicle lines is pure and tested and goes in the round's prompt. On screen with a dev instance and fake chronicle lines. Mark part 8 built in the plan.
  After: Warriv's rounds: the whole project looked at every hour while it drives
- [x] While Warriv drives, a due errand waits out the 90 % hold, and the stop closes errand sessions @while-warriv-drives-a-due-errand-waits-68283
  Part of "The switch and the stop" in "Warriv drives". `warriv::when_full(drives)` already says Wait or Skip; wire it into the errand clock, and close errand sessions in `stop_drive` (crates/horadric-ui/src/warriv.rs, where a comment marks the spot). On screen with a dev instance and cmd.exe. Count claude.exe after.
  After: Errands: runewords with "every", armed once and cast on a clock
- [x] A quest command to reorder the log, so a round can @a-quest-command-to-reorder-the-log-so-a-72236
  Part 2 of "Warriv drives" says a round may reorder the log; no quest command moves a quest yet. Something like `quest move "title" --below "title"`, pure and tested in tasks, allowed for Warriv like the other quest commands, and named in warriv::driven_prompt.
  After: Warriv's rounds: the whole project looked at every hour while it drives
- [x] Find why a fresh dev instance showed another dev instance's shipproj session @find-why-a-fresh-dev-instance-showed-73729
- [x] A dev instance refuses CLI requests from a caller with a different state folder @a-dev-instance-refuses-cli-requests-from-73788
  Why: on 2026-10-05 the warriv worktree started its dev app with HORADRIC_PORT=4110, the
  daily-look-back worktree's port. Its own app quit ("already running, port 4110 answers") and
  its `horadric new` in a scratch shipproj reached the other dev instance, which started a claude
  there. Idea: CLI requests carry the caller's state folder; an app with a different one refuses
  and names the port's owner.
- [x] The Settings window, with the tray's settings moved into it @the-settings-window-with-the-tray-s-38567
  See "The Settings window" in docs/PLAN.md. A window of its own that closes (not a tile in the columns), drawn like the usage window with the existing controls (dropdown.rs, field.rs). Sections down the left; this quest builds the frame and Appearance (theme, terminal font, tiles on screen), Notifications (notify when waiting, loot sounds), Startup and updates (start with Windows, check now) and Privacy (Discord). Every change applies at once, no Save button. The tray gains "Settings..." and loses those lines; Warriv drives stays in the tray. Pure and tested: section layout and hit testing. Verify on screen with a dev instance in dark and light, and that each change takes effect while the window is open.
  Assumed: a window of its own (taskbar button, centred on the cursor's screen, Esc closes), as the quest said; the tray keeps 'Update to X' as an action beside 'Settings...'.
- [x] Fix the merge of the-settings-window-with-the-tray-s @fix-the-merge-of-the-settings-window-with-53156
  "The Settings window, with the tray's settings moved into it" is done on the branch `the-settings-window-with-the-tray-s` but did not merge into `main`: rebasing it on `main` conflicts.
  Take its commits onto your branch with `git cherry-pick main..the-settings-window-with-the-tray-s`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D the-settings-window-with-the-tray-s`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  Auto-merging README.md
  CONFLICT (content): Merge conflict in README.md
  Auto-merging crates/horadric-ui/src/app.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/app.rs
  Auto-merging crates/horadric-ui/src/dropdown.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/dropdown.rs
  Auto-merging crates/horadric-ui/src/layout.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/layout.rs
  Auto-merging crates/horadric-ui/src/lib.rs
  Auto-merging crates/horadric-ui/src/render.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/render.rs
  Auto-merging crates/horadric-ui/src/settings.rs
  CONFLICT (add/add): Merge conflict in crates/horadric-ui/src/settings.rs
  Auto-merging crates/horadric-ui/src/tray.rs
  CONFLICT (content): Merge conflict in crates/horadric-ui/src/tray.rs
  Auto-merging docs/PLAN.md
  CONFLICT (content): Merge conflict in docs/PLAN.md
  error: could not apply c03c61a... Add the Settings window and move the tray's settings into it
  hint: Resolve all conflicts manually, mark them as resolved with
  hint: "git add/rm <conflicted_files>", then run "git rebase --continue".
  hint: You can instead skip this commit: run "git rebase --skip".
  hint: To abort and get back to the state before "git rebase", run "git rebase --abort".
  hint: Disable this message with "git config set advice.mergeConflict false"
  Could not apply c03c61a... # Add the Settings window and move the tray's settings into it
  ```
  Warriv: Decided: drop the branch, merge nothing. c03c61a is a duplicate build of the Settings window made in parallel; main's ad12f6a landed 40 seconds earlier, shipped in 0.16.0, and four sections build on it. Model: haiku
  Assumed: c03c61a was a duplicate build of ad12f6a (same quest, same base, 39s apart) that main already has and extended, so kept main's code and took only its README line.
- [x] Settings: the Sessions section @settings-the-sessions-section-39605
  After: Fix the merge of the-settings-window-with-the-tray-s
  See "The Settings window" in docs/PLAN.md. Default model, effort and permissions for the sessions Horadric starts (SavedState.defaults), and which agents are offered. The usage window keeps its rows as a shortcut to the same values, so a change in either shows in both. Verify on screen with a dev instance.
  After: The Settings window, with the tray's settings moved into it
  Assumed: an Agent row picks whose defaults show (Claude Code, Codex, Grok Build); effort is a list there, not the slider; 'In the project menu' switch hides an installed agent's New session line (agents_off in state.json), Claude Code always offered.
- [x] Settings: the Projects section, opened from the project menu @settings-the-projects-section-opened-from-39630
  After: Fix the merge of the-settings-window-with-the-tray-s
  See "The Settings window" in docs/PLAN.md. Every project in one list; picking one shows its colour, work mode, SSH hosts, and Warriv drives with and ships public (the tray keeps Warriv drives too). The project menu loses Work mode, Colour and Add host and gains "Project settings...", which opens the window at that project. Verify on screen with a dev instance and two projects.
  After: The Settings window, with the tray's settings moved into it
  Assumed: the project list is a dropdown row at the top of the Projects section (not a second column), and 'Add host' is a button row that opens the usual ask with ~/.ssh/config names found as typed.
- [x] Settings: the Runetome section @settings-the-runetome-section-39640
  After: Fix the merge of the-settings-window-with-the-tray-s
  See "The Settings window" in docs/PLAN.md. "Ask before a click casts" and bringing back put away built in stones, moved from the Runetome menu into the window; the Runetome menu keeps its actions. Verify on screen with a dev instance.
  After: The Settings window, with the tray's settings moved into it
- [x] Settings: the Keys section, with the hotkeys settable @settings-the-keys-section-with-the-39650
  After: Fix the merge of the-settings-window-with-the-tray-s
  See "The Settings window" in docs/PLAN.md. The three hotkeys (next waiting session, stay a while and listen, stop Warriv) are fixed in app.rs register_hotkey. Show them, let each be set by pressing the new chord, kept in state.json, registered again at once, and say when a chord is taken by another program. Parsing and naming chords is pure and tested. Verify on screen with a dev instance.
  After: The Settings window, with the tray's settings moved into it

## macOS

- [x] OSC 7501: read the Program Status Protocol from the terminal stream @osc-7501-read-the-program-status-protocol-29676
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. First bullet. Find the spec itself first (Mitchell Hashimoto, 2026-10-08) and quote its format in the plan. A pure, tested parser where the console sees the bytes, feeding a phase into the registry. Hooks win where both speak; OSC 7501 gives a phase to sessions without hooks. Verify on screen with a dev instance and a real claude 2.1.295 or later, then with a script printing the sequences in a cmd.exe terminal.
  Assumed: a hooked session takes OSC 7501 only to end a turn its hooks still call working, after 10 quiet seconds (LOST_HOOK). The probe answer is sent as key presses on Windows because ConPTY drops OSC input.
- [x] OSC 7501: plain terminals show what their program says @osc-7501-plain-terminals-show-what-their-41320
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. First bullet. A plain terminal's tile takes working, blocked, done and error from OSC 7501, with the message as its second line and progress drawn when given. Check Codex and Grok sessions too: where their hooks are thin, OSC 7501 fills in. Check on screen in dark and light.
  After: OSC 7501: read the Program Status Protocol from the terminal stream
  Warriv: Model: sonnet
  Warriv: Warriv here: this quest has been under way about 7 hours with nothing on main. If a part works, run the three checks and commit it now. If something blocks you, say what in one line so I can settle it.
  Warriv: Under way 7 hours with no commit (last real commit 861582f). Told the session to commit what works or name its blocker.
  Warriv: Warriv here: 10 hours and your worktree has no changes. I narrowed the quest: do only a plain cmd.exe terminal's tile taking working, blocked, done and error from OSC 7501, with the message as its second line. Codex and Grok are now a separate quest. Run the three checks and commit that part, then report done. If something stops you, report blocked with it in one line.
  Warriv: Silent 10 hours after a nudge, worktree empty. Narrowed to the plain terminal tile only (progress drawing and light theme check stay in, as the quest asks) and split Codex and Grok into their own quest. If still silent next round, hand to the human: the session may be dead.
  Warriv: Silent 11 hours, two nudges, worktree still empty at 1b663c0. The session looks dead and I can not start one, so handed to the human.
- [x] OSC 7501: Codex and Grok tiles fill in from it where their hooks are thin @osc-7501-codex-and-grok-tiles-fill-in-41344
  Model: sonnet
  Split from 'OSC 7501: plain terminals show what their program says'. Check what Codex and Grok sessions send as OSC 7501 and let it fill phases their hooks miss, hooks winning where both speak. Pure parts tested; check on screen with a dev instance.
  After: OSC 7501: plain terminals show what their program says
- [x] Horadric's own Claude mod, installed with the hooks @horadric-s-own-claude-mod-installed-with-29687
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Second bullet. Read the mods and plugin docs first (2.1.287). A plugin with one mod, installed and removed by the hooks installer, refused by a dev instance as hooks are. It posts to the listener with the X-Horadric-Session tag: $.agent.list() states and agent.spawn with agent type. Keep it small and fail quietly when Horadric is not running. Verify with horadric run on Haiku that its events arrive.
  Assumed: loaded through CLAUDE_CODE_PLUGIN_DIRS in the settings env (~/.claude/horadric-mod), not a marketplace, since a marketplace needs claude plugin install. The mod reaches this machine at the next ship local.
- [x] Subagents on the tile by type, ended when they end @subagents-on-the-tile-by-type-ended-when-30480
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Third bullet. Replace the 30 second quiet guess in session.rs with the mod's reports where present, keep the guess as the fallback. The tile shows each running subagent by type. Pure and tested; on screen with a dev instance.
  After: Horadric's own Claude mod, installed with the hooks
  Warriv: Model: sonnet
  The mod now posts HoradricSpawn (agent_id, agent_type) and HoradricAgents (the whole $.agent.list(): agent_id, agent_type, status) to the hook path; parsed into HookEvent.agent_type and HookEvent.agents (ModAgent), not yet used by session.rs.
- [x] The mod tells Claude what Horadric knows @the-mod-tells-claude-what-horadric-knows-30490
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Second bullet, the band above the prompt. Show another session editing the same file (overlap.rs) and the quest this session holds. Nothing shown when there is nothing to say. Verify on screen with two sessions on the same file.
  After: Horadric's own Claude mod, installed with the hooks
  Warriv: Model: opus
- [x] A waiting tile names what it waits on @a-waiting-tile-names-what-it-waits-on-29697
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Fourth bullet. Since 2.1.295 a permission wait names the MCP tool. Find where it arrives (Notification or PermissionRequest payload), keep it on the session and show 'waiting on <tool>' on the tile. Pure and tested; on screen with a real claude on Haiku asking for a tool.
- [x] Sessions default: a model for workflow agents @sessions-default-a-model-for-workflow-29707
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Fifth bullet. A row in Settings, Sessions, beside model and effort, that sets CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL in every session's environment, Warriv and errands included. Empty means unset. Check on screen.
  Assumed: the row is labelled "Workflow agents' model", offers the same models as Model, shows only for Claude Code, and stays out of the usage window since no running session can switch it.
- [x] Keep onFailure block off Horadric's hooks @keep-onfailure-block-off-horadric-s-hooks-29718
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Sixth bullet. The installer never writes onFailure on Horadric's hooks and strips it if found, since a closed Horadric would then block every prompt. Pure and tested.
- [x] Propose tiles for Claude Code cloud sessions @propose-tiles-for-claude-code-cloud-41323
  See "Claude Code 2.1.287 to 2.1.296" in docs/PLAN.md. Last bullet. Do not build. Find how cloud sessions can be listed and followed from this machine (claude agents, Remote Control, an API), write the options and a recommendation into the plan, then report blocked so the human decides.
  Warriv: Not stuck: the session ended its turn while its background docs agent runs, and that agent's report wakes it. Left it alone; it will commit the draft and report blocked for the human's pick, as the quest asks.
  Warriv: Proposal committed (0c6e46d) and merged. The quest itself asks the human to pick, so left it blocked for them rather than assuming. Rest of the log moves on.
- [x] Theme colours in the mod's band show in Horadric's terminal @theme-colours-in-the-mod-s-band-show-in-31576
- [x] Claude Code's OSC 7501 on Windows: the pseudo console answers its CSI c fence first @claude-code-s-osc-7501-on-windows-the-70016
  After: OSC 7501: read the Program Status Protocol from the terminal stream
  See the OSC 7501 bullet in docs/PLAN.md. Claude only reports after the terminal answers OSC 7501 ; ?, fenced by CSI c, and conhost answers CSI c itself before the query reaches Horadric. Try PSEUDOCONSOLE_PASSTHROUGH_MODE, or ask Anthropic for an env switch. Check with claude --debug-file: its Terminal capabilities line says programStatus=yes or no.
  Warriv: Model: opus
  Assumed: vendored conpty.dll + OpenConsole.exe (ConPTY 1.25, MIT, 1.2 MB, x64 only) into the repo and the binary, since passthrough mode is gone and Claude Code has no env switch. HORADRIC_CONPTY=inbox falls back to Windows' own. Every session now runs on the new console host; say if you would rather not ship a Microsoft binary.
- [x] OSC 11 colour replies never reach programs on Windows: the pseudo console drops OSC on input @osc-11-colour-replies-never-reach-31866
  Found while building OSC 7501: any OSC written to ConPTY input is dropped, and the bytes after it can go too. Events::ColorRequest in console.rs answers raw. Typing it as win32-input-mode key presses (program_status::as_key_presses) carries it through, but programs that fence with CSI c still lose the race.
  Assumed: COLORFGBG instead of bundling OpenConsole/conpty.dll (which would pass OSC 11 through) since that is a new shipped binary and a dependency decision for the human. Not verified on screen in a dev instance; the env entry sits beside COLORTERM.
- [x] Add choco install horadric as an install option on the site and README @add-choco-install-horadric-as-an-install-13524
  Horadric is approved on Chocolatey (https://community.chocolatey.org/packages/horadric, approved 2026-10-12). Add `choco install horadric` next to the Windows download on horadric.dev and in the README install section. Say the installed app updates itself, so it can run ahead of the Chocolatey version. Also add a step to RELEASING.md: after each release, bump the package in the socialsandbox repo (products/horadric/chocolatey/horadric: version, URL, SHA256 of horadric-x64.zip), then choco pack and choco push. Every version is moderated.
  Assumed: pushed only the site commit (cherry-picked onto origin/main), leaving the website checkout's unpushed Warriv memory commits local, since .horadric/warriv.md may not be meant for the public repo. Its local main now holds a duplicate of the pushed commit; git pull --rebase drops it.
- [x] Town Portal: the PC says what a waiting session asks @town-portal-the-pc-says-what-a-waiting-14035
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 1. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human. Pure parts tested; check with a real claude on Haiku asking for a tool and an AskUserQuestion.
  Assumed: a twin session (worktree ...town-portal-the-pc-says-what-a-waiting) built this same quest and is landing it, so the -2 run stopped without committing and sent it its edge cases (multiSelect, ExitPlanMode, non-Claude agents, detail cap).
- [x] Town Portal: an inbox of waiting sessions on the phone @town-portal-an-inbox-of-waiting-sessions-14925
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 2. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  After: Town Portal: the PC says what a waiting session asks
  Warriv: Model: sonnet
  Assumed: ask.since read as seconds or milliseconds, whichever size it is. Not tried in the browser pane (it dropped every call); relay tests pass and app.js parses. Look at it at phone size before deploy. No PC-side or PORTAL.md change was needed.
- [x] Town Portal: pushes that say what waits and open that session @town-portal-pushes-that-say-what-waits-14045
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 3. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human. The RFC 8291 encryption is pure and tested against the RFC's test vector.
  Assumed: the push title comes from the session's phase and waits_on ("Allow Bash?", "Waiting for you", ...) since the ask quest runs beside this one; once ask lands, webpush::wait_text can read it. Added a "pc" field to the payload so a phone paired with several PCs opens the right one.
  Not tried: a real push through a push service. WebView2 has no push service. The crypto matches RFC 8291's vector byte for byte and a fresh push opens as a phone would. To try on a phone: PUSH_ENDPOINT, PUSH_P256DH, PUSH_AUTH with cargo test -p horadric-ui posts_to_a_push_service -- --ignored. Page not deployed.
- [x] Town Portal: answer a waiting session from the notification @town-portal-answer-a-waiting-session-from-15305
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 4. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  After: Town Portal: an inbox of waiting sessions on the phone
  After: Town Portal: pushes that say what waits and open that session
  Warriv: Model: opus
  Assumed: a permission push offers Yes and No, never Always (a lock screen tap should not widen what is allowed); a question offers its first two choices. The page sends the answer only if the session still waits on the same ask (same since, keys among its options), within 5 minutes.
  Also fixed on portal main: state.open was declared twice in app.js (push target and quests notes Set), which broke opening a session from a push. Not tried: a real push with actions on Android; WebView2 has no push service. Tested in the pane: fragment and page-open paths each sent the answer once, a stale since sent nothing. Page not deployed.
- [x] Town Portal: terminal keys that fit the moment @town-portal-terminal-keys-that-fit-the-14936
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 5. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  After: Town Portal: the PC says what a waiting session asks
  Warriv: Model: sonnet
  Assumed: page-only change, committed on ../horadric.portal main (9989f31), CACHE bumped to v10, not deployed. Browser pane did not answer, so the key logic was checked in node only, not at phone size.
- [x] Town Portal: pinch to zoom the phone's terminal @town-portal-pinch-to-zoom-the-phone-s-14055
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 6. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: text size range 8 to 28px, kept to half pixels; A- and A+ buttons removed; Ctrl+wheel also zooms on a computer. Page only, no PORTAL.md change. Not deployed.
- [x] Town Portal: a phone screen that holds still while it updates @town-portal-a-phone-screen-that-holds-14066
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 7. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  While testing I stopped every node test/fakepc.js on the machine once (a too broad filter), which took two other sessions' fake PCs offline; they need restarting if those sessions were still testing.
- [x] Town Portal: swipe between a project's sessions @town-portal-swipe-between-a-project-s-15098
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 8. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  After: Town Portal: a phone screen that holds still while it updates
  Warriv: Model: sonnet
- [x] Town Portal: the keyboard never covers the input on iOS @town-portal-the-keyboard-never-covers-the-14076
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 9. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: committed on branch keyboard in a worktree ../horadric.portal.keyboard, because another quest has uncommitted edits to the same portal files on main; it merges once main's tree is clean. Tested by a stand-in visualViewport in the pane (input bottom lands on the keyboard top, screen keeps its last line); a real iPhone check is still worth doing.
- [x] Town Portal: transitions, haptics and a loading skeleton @town-portal-transitions-haptics-and-a-14086
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 10. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: haptic('answer') (a double tick) fires for the y, n and digit keys today; the inbox cards from item 2 are not on portal main yet, so their option buttons should call haptic('answer') when they land.
  Assumed: no new messages, so PORTAL.md is unchanged. Page only, merged to horadric.portal main as 6b64283, cache v7. Not deployed.
- [x] Town Portal: long press a session to rename, stop or copy its last reply @town-portal-long-press-a-session-to-14096
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 11. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: tested the page in a headless Edge over CDP at 390x780 with touch, against test/fakepc.js, because the Horadric browser pane kept answering 'connection forcibly closed'. Skipped a live dev-instance run: another quest's dev instance was up on the shared data dir and port. PC side is covered by unit tests (title::last_reply, portal::session_name) and reuses App::rename and App::end.
  Assumed: Copy last reply copies at once via a ClipboardItem promise (Safari needs the tap), and also shows the reply with a Copy button as fallback. Messages documented under 'Added in 0.24' in docs/PORTAL.md. Not deployed.
- [x] Town Portal: see what a session changed, with diffs @town-portal-see-what-a-session-changed-14106
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 12. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: the browser pane did not answer (os error 10054 on every call after the 05:51 reload), so the page was tested at phone size in headless Edge over CDP instead.
- [x] Town Portal: attach a photo to a message @town-portal-attach-a-photo-to-a-message-14117
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 13. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: the page sends a JPEG of at most 700 KB, long side 2048 px, and puts the saved path into the input text rather than a chip. The browser pane was down, so the page is checked by syntax and the relay tests only; a follow-up quest tries it on screen.
- [x] Town Portal: quests and Warriv on the phone @town-portal-quests-and-warriv-on-the-14127
  See "Town Portal on the phone, round two" in docs/PLAN.md, item 14. Follow the rules at the top of that section: both repos, PORTAL.md, additive, tested in the browser pane at phone size, no deploy without the human.
  Assumed: the PC side was checked by unit tests and the page against fakepc.js, not against a live dev instance, since another quest's dev instance held port 43118 the whole time. Not deployed: deploy.sh waits for the human.
  Assumed: a phone that asked for quests keeps getting them on every change while connected, so there is no unfollow message; the quest state word for open is Ready on the page, so it does not read like the Open button.
- [x] Browser tools drop connections and replay input on x.com @browser-tools-drop-connections-and-replay-14602
  From a socialsandbox session 2026-10-12: on x.com compose, most browser_* calls failed with os error 10054 or 10060 (connection forcibly closed / timed out), yet some still ran in the page. browser_type with multi-line text failed every time. A later insertText via browser_evaluate also errored but ran, and the posted text came out mangled: first line kept, the middle lost, the link repeated over and over (rg/packages/horadric x12). It looks like failed calls run anyway, or get retried. Navigate and evaluate also hung past 120s. A tool that errors should not have acted, and should never act twice.
- [x] Town Portal: try the photo button in the browser pane @town-portal-try-the-photo-button-in-the-14652
  The attach quest could not use the pane (its MCP link broke after a reload). At phone size against test/fakepc.js: pick a photo, see the button pulse, the path land in the input, and a big photo shrink under 700 KB. Then once against a dev instance with a real session, check the file lands in portal-attach and the agent reads it.
  Assumed: picked the photo by setting the file input from script (a DataTransfer File), since the pane cannot drive the OS file dialog. Drove the pane over the app's /horadric/browser HTTP route because the MCP tools kept failing with 10054. Fixed a duplicate @keyframes pulse in the portal's style.css (portal main ff0e7fe, not deployed).
- [x] Browser pane tools drop about half their calls with os error 10054 @browser-pane-tools-drop-about-half-their-15071
  Seen 2026-10-12 testing the portal page: browser_evaluate, screenshot, snapshot and close failed with 'An existing connection was forcibly closed by the remote host' on roughly every other call, worse while the page updated twice a second. Retrying the same call usually worked. Some calls ran in the page even though the reply failed.
  Clue from the photo quest: the same ops POSTed straight to 127.0.0.1:43117/horadric/browser (x-horadric-command: browser, x-horadric-session) answered every time, about 30 calls in a row, while browser_* through the MCP failed on most. So the app side is fine and the fault looks like the MCP process's client, or a stale MCP process from before the 06:11 reload.
  Cause found and reproduced: a child process the app starts mid call inherits the socket, and its exit resets the connection after the whole reply arrived (so failed calls had run). Main's 900f7c1 already fixes it (listener reads through a borrow, not an inheritable try_clone) with a test; the installed build (06:11) predates it, so it shows until the next ship. No new code.
- [x] Town Portal: a phone photo needs no permission to read @town-portal-a-phone-photo-needs-no-15553
  Seen 2026-10-12 with a real claude: the photo lands in %APPDATA%\Horadric\portal-attach, outside the project, so Read asks 'Do you want to proceed?' before the agent sees it. The phone can answer with its 1 key, but it should not have to. Allow reads of portal-attach for Horadric's sessions (permissions.additionalDirectories or an allow rule in the settings Horadric passes claude), or save the photo under the project instead.
- [x] Lore: count the tokens each quest spends @lore-count-the-tokens-each-quest-spends-67295
  See docs/plan/lore.md, part 1. Sum transcript usage per session and per quest, split by tool, keep it with the done quest and show it in the quest log window. Pure and tested on transcript fixtures.
- [x] Lore: a context pack for each new quest @lore-a-context-pack-for-each-new-quest-67894
  See docs/plan/lore.md, part 2. Record files read and changed per done quest, then give a new quest the plan parts and files similar quests needed. Compare token counts before and after with part 1.
  After: Lore: count the tokens each quest spends
  Assumed: the pack comes from the 3 most alike done quests, changes count double reads, and Warriv corrects a pack by naming paths in a quest's notes (they lead the pack); no separate pack format.
- [x] Lore: notes shared between a project's sessions @lore-notes-shared-between-a-project-s-67305
  See docs/plan/lore.md, part 3. lore_note and lore_recall on Horadric's MCP server, stored per project in APPDATA, marked maybe stale when their files change.
  Warriv: Already on main: 8a65e94 merged at c863656, branch and worktree gone. The failed auto merge ran after the worktree was removed (os error 267), nothing left to merge. Shipped with the 2026-10-15 Ship Local.
- [x] Lore: a code index on the MCP server @lore-a-code-index-on-the-mcp-server-67826
  See docs/plan/lore.md, part 4. Do not start until the human has chosen what reads the code (Open questions in lore.md). If not chosen, mark blocked asking for it.
  After: Lore: notes shared between a project's sessions
  Decided 2026-10-15: tree-sitter, starting with Rust, TypeScript/JavaScript and Python. Add it to the dependency list in CLAUDE.md.
  Review 2026-10-15: lore_outline first; check Claude Code's LSP tool before building lore_find and lore_calls. See lore.md part 4, Order.
- [x] Lore: count the spend of a quest marked done by hand @lore-count-the-spend-of-a-quest-marked-67808
  Part 1 records tokens only when the agent runs quest done or blocked (crates/horadric/src/task.rs record_spend). A quest the human or the runner marks done has none. Codex rollouts count nothing yet.
  Warriv: Merge conflicted in task.rs against Lore take notes (75379ec). The fix-up quest already runs and carries the branch; nothing else to do. Ship local waits for it to land.
- [x] Fix the merge of lore-count-the-spend-of-a-quest-marked @fix-the-merge-of-lore-count-the-spend-of-69728
  "Lore: count the spend of a quest marked done by hand" is done on the branch `lore-count-the-spend-of-a-quest-marked` but did not merge into `main`: rebasing it on `main` conflicts.
  Take its commits onto your branch with `git cherry-pick main..lore-count-the-spend-of-a-quest-marked`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D lore-count-the-spend-of-a-quest-marked`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  Auto-merging crates/horadric/src/task.rs
  CONFLICT (content): Merge conflict in crates/horadric/src/task.rs
  Auto-merging docs/plan/lore.md
  error: could not apply ca1485f... Count the spend of a quest marked done by hand, and of Codex
  hint: Resolve all conflicts manually, mark them as resolved with
  hint: "git add/rm <conflicted_files>", then run "git rebase --continue".
  hint: You can instead skip this commit: run "git rebase --skip".
  hint: To abort and get back to the state before "git rebase", run "git rebase --abort".
  hint: Disable this message with "git config set advice.mergeConflict false"
  Could not apply ca1485f... # Count the spend of a quest marked done by hand, and of Codex
  ```
  Warriv: Model: sonnet
  Warriv: Its branch conflicted with main again (main moved while it worked); the second fix-up is already under way and carries the work, so nothing else to do.
- [x] Fix the merge of fix-the-merge-of-lore-count-the-spend-of @fix-the-merge-of-fix-the-merge-of-lore-70375
  "Fix the merge of lore-count-the-spend-of-a-quest-marked" is done on the branch `fix-the-merge-of-lore-count-the-spend-of` but did not merge into `main`: rebasing it on `main` conflicts.
  Take its commits onto your branch with `git cherry-pick main..fix-the-merge-of-lore-count-the-spend-of`, resolve each conflict and `git cherry-pick --continue`, make every check in `.horadric/config.json` pass, and commit. Then delete it with `git branch -D fix-the-merge-of-lore-count-the-spend-of`. Your branch merges like any finished quest and carries its work.
  The quests after the finished one wait for this one too.
  What failed said:
  ```
  Auto-merging crates/horadric-core/src/chronicle.rs
  Auto-merging crates/horadric-ui/src/runner.rs
  Auto-merging crates/horadric/src/task.rs
  CONFLICT (content): Merge conflict in crates/horadric/src/task.rs
  Auto-merging docs/plan/lore.md
  error: could not apply 36ea3ac... Count the spend of a quest marked done by hand, and of Codex
  hint: Resolve all conflicts manually, mark them as resolved with
  hint: "git add/rm <conflicted_files>", then run "git rebase --continue".
  hint: You can instead skip this commit: run "git rebase --skip".
  hint: To abort and get back to the state before "git rebase", run "git rebase --abort".
  hint: Disable this message with "git config set advice.mergeConflict false"
  Could not apply 36ea3ac... # Count the spend of a quest marked done by hand, and of Codex
  ```
  Warriv: Model: sonnet
- [x] Lore: take notes when a quest ends @lore-take-notes-when-a-quest-ends-67988
  See docs/plan/lore.md part 3, 'Changed after a review'. On quest done, store the done summary, the files changed and Warriv's learnings as lore notes naming their files, through horadric_core::lore. Pure parts tested.
  Assumed: Warriv's learnings are only the new lines under Rules (Lately is what it did, not what it learned), and in the main tree a quest's files come from its transcripts' Edit and Write calls, since git there mixes in other sessions' changes.
- [x] Lore: move old done quests out of tasks.md @lore-move-old-done-quests-out-of-tasks-md-67999
  See docs/plan/lore.md part 5. Done quests older than a few days go to .horadric/tasks-done.md with their @id, notes and spent records. The quest log window, chronicle and id lookups still find them. Other sessions write tasks.md at the same time: use the same locking the quest commands use.
  Assumed: a few days is three (tasks::KEEP_DONE), counted from the chronicle's last record of the quest; a done quest the chronicle never heard of, or with no @id, counts as old.
  Assumed: the sweep runs on every quest done, not on a timer in the app. A done quest still named in a staying quest's After: line stays in the log.
  Assumed: every tasks::update now takes a lock file .horadric/tasks.lock (shared Lock with lore). tasks-done.md is new and untracked; commit it with tasks.md.
- [x] Tristram: the town cluster, stage and browser @tristram-the-town-cluster-stage-and-68122
  Step 1 of docs/plan/tristram.md: town folder, pinned cluster that stays when empty, its stage and browser pane.
- [x] Tristram: generated CLAUDE.md and project dirs @tristram-generated-claude-md-and-project-70682
  Step 2 of docs/plan/tristram.md: town::brief, NOTES.md, --add-dir for every project folder.
  After: Tristram: the town cluster, stage and browser
  Warriv: Model: sonnet
- [x] Tristram: quest --project and horadric projects @tristram-quest-project-and-horadric-71224
  Step 3 of docs/plan/tristram.md.
  After: Tristram: generated CLAUDE.md and project dirs
  Warriv: Model: sonnet
  Assumed: horadric projects shows sessions as running or paused from saved state, since live phases live only in the app
- [x] Tristram: found a project @tristram-found-a-project-71869
  Step 4 of docs/plan/tristram.md: tile menu item and horadric found <name>.
  After: Tristram: quest --project and horadric projects
  Warriv: Model: sonnet
- [x] Lore: compare quest spend with and without a context pack @lore-compare-quest-spend-with-and-without-3186
  Run horadric quest spend once 10 or more quests have run with a pack. Baseline 2026-10-15: 63 quests, median 6.5M tokens, 60 calls, Read 8%. Write the result into docs/plan/lore.md part 2. After: Lore: a context pack for each new quest
  After: Lore: a context pack for each new quest
  Warriv: Stall is expected: this waits for 10 quests run with a pack, and the pack ships with the Ship Local building at 22:03 and the Ship Public cast at 22:04. No open aim, so nothing more to file.
- [x] Tristram: label the empty town's plus New session @tristram-label-the-empty-town-s-plus-new-70671
  An empty town shows only a plus. Draw the plus and 'New session' centred as a group in the wide add button while the town has no sessions (render.rs, the add button in the cluster paint). A duplicate run of step 1 tried it and it read well on screen.
- [x] Arcane Sanctuary: the graph and its layout @arcane-sanctuary-the-graph-and-its-layout-78326
  Plan: docs/plan/the-arcane-sanctuary.md, step 1. The pure part only, no drawing. Build the graph (files, lore notes, quests; links from the code index's resolved calls, notes' files, quests' touched records). Force layout with Barnes Hut as a pure, tested function from graph and old positions to new ones. Keep positions per project in the state folder. Must stay fast past a few thousand nodes.
- [x] Arcane Sanctuary: the pane on the stage @arcane-sanctuary-the-pane-on-the-stage-79078
  Plan: docs/plan/the-arcane-sanctuary.md, step 2. A Direct2D pane on the stage beside sessions and the browser pane, one per project, kept in the grid order, with the header zoom button to fill the stage like other panes. Opened from the cluster header menu and a stage key. Pan by drag, wheel zoom toward cursor, hover label. Draw on the vsync clock only while something moves; zero CPU when still. Verify on screen with a dev instance.
  After: Arcane Sanctuary: the graph and its layout
  Assumed: the sanctuary sits after the browser in the grid like the browser pane does, not in the saved order (so it cannot be swapped by drag), and is not reopened after a restart. Its key is Ctrl+Shift+G, which takes Ctrl+Shift+G from programs as Ctrl+Shift+B does. Hover shows the node's full name; who touched a file last is left to the lighting step, which hears sessions.
  Assumed: the fit includes the orphan ring, so on this repo the linked cluster fills about a third of the pane at first; wheel zoom or a smaller ring in step 1 would change that.
- [x] Memory: an embedding model in Rust with candle, and a yardstick @memory-an-embedding-model-in-rust-with-82571
  See docs/plan/memory.md. Part 1. Build the recall yardstick (50+ real questions with answers from this project's transcripts and notes, kept in the repo), then run EmbeddingGemma 300M and a small BERT model with candle, measure recall at 5, speed and memory, and pick one. Add candle and its tokenizer to the dependency list in CLAUDE.md with the reason.
  Warriv: Stopped while waiting on its own background benchmark and yardstick agent, which wake it when done. Not stuck, so no tell.
  Warriv: Woken again: yardstick done (1,686 chunks, 65 questions), benchmark still running in its own background task, which wakes the session. Not stuck, no tell.
  Warriv: Woken a third time: second benchmark run (padding off, final corpus) is in its own background task, which wakes the session. Not stuck, no tell.
  Assumed: all-MiniLM-L6-v2 over EmbeddingGemma. Fused with words it finds the most (87.7 % against 84.6 % recall at 5) at 92 MB and 82 ms a chunk, against 858 MB and 234 ms. Gemma alone is better on dialogue and stays runnable in the crate, so a bigger yardstick can reopen it.
  Assumed: the yardstick corpus holds dialogue chunks from the transcripts, screened for secrets, emails and personal matters, in a repo that may go public. Drop crates/horadric-memory/yardstick/corpus.jsonl dialogue lines if that is unwanted.
  Warriv: Woken a fourth time: waiting on its own review agent, then merges and marks done. Not stuck, no tell.
  Warriv: Woken a fifth time: re-measurement for the plan's numbers runs in its own background task, which wakes the session, then it merges and marks done. Not stuck, no tell.
- [x] Memory: the store, monthly segments of verbatim text and vectors @memory-the-store-monthly-segments-of-86273
  See docs/plan/memory.md. Part 2. Text is the truth, vectors a cache tagged by model, re-embedded in the background when the model changes. Versioned format with a test reading every earlier version.
  After: Memory: an embedding model in Rust with candle, and a yardstick
  Warriv: Model: opus
  Assumed: a chunk goes into the segment of the month it is written in, not the month it speaks of, so ended months never change (a backfill lands in the month it ran). Vectors kept as int8, tagged model@weights-commit.
  Warriv: Stopped waiting on its own workspace tests (queued for the CPU slot) and diff review, then commit and quest done. Not stuck, no tell.
- [x] Memory: lore_recall finds by meaning, served by the app @memory-lore-recall-finds-by-meaning-770
  See docs/plan/memory.md. Part 3. BM25 plus vectors, reciprocal rank fusion, weighed by project, kind, age and staleness. The app holds the model; the MCP server asks it and falls back to words when the app is down. Measured against the yardstick.
  After: Memory: the store, monthly segments of verbatim text and vectors
  Warriv: Model: opus
  Assumed: the app fetches MiniLM (90 MB) on the first lore_recall, not at start, and answers by words until loaded; dialogue weighs 0.9 against notes (yardstick best), other projects 0.6, maybe stale 0.8, age halves its 0.3 share every 90 days. The old word count in horadric-core lore (Lore::recall, describe) was removed, recall.rs replaces it.
  Warriv: Stopped with 26dadf9 committed, waiting on its own review agent before merging. Not stuck, no tell.
- [x] Memory: transcripts kept verbatim, dialogue only @memory-transcripts-kept-verbatim-dialogue-2403
  See docs/plan/memory.md. Part 4. Read transcripts on Stop, PreCompact and SessionEnd, keep prompts and agent text with tool calls reduced to name and files, Codex too, backfill what is on disk, leave secrets out.
  After: Memory: lore_recall finds by meaning, served by the app
  Warriv: Model: sonnet
  Assumed: the model is fetched by the first recall, not the first keep, so an app nobody recalls in keeps dialogue without the download; vault secret values are not yet redacted by exact value, only by shape.
- [x] Memory: code by meaning @memory-code-by-meaning-2414
  See docs/plan/memory.md. Part 5. A vector per symbol in the lore's code index (head and doc comment), refreshed by file hash.
  After: Memory: lore_recall finds by meaning, served by the app
  Warriv: Model: sonnet
- [x] Memory: recall across projects, behind a switch @memory-recall-across-projects-behind-a-3176
  See docs/plan/memory.md. Part 6. Off by default, in Settings; hits name their project.
  After: Memory: transcripts kept verbatim, dialogue only
  Warriv: Model: sonnet
- [x] Memory: Warriv distills the day into notes with validity @memory-warriv-distills-the-day-into-notes-3196
  See docs/plan/memory.md. Part 7. The daily look back writes decisions and lessons as notes linking their source chunks, replacing what they contradict; notes keep when they became true and stopped.
  After: Memory: transcripts kept verbatim, dialogue only
  Warriv: Model: opus
  Warriv: Stopped with code committed in its branch and checks green, waiting on its own review agent before merging. Not stuck, no tell.
- [x] Memory: Horadrim, sigils and the Lexicon for recall's answers @memory-horadrim-sigils-and-the-lexicon-2425
  See docs/plan/memory.md, part 8. Inscribe hits with sigils and stripped chatter, never search the inscribed form, unroll by chunk id. Measure tokens per hit and answer score against verbatim on the yardstick.
  After: Memory: lore_recall finds by meaning, served by the app
  Warriv: Model: opus
  Warriv: Stopped waiting on its own checks, yardstick run, review agent and answer scoring, then merges. Not stuck, no tell.
  Assumed: each sigil is told to a session at the head of the first answer that uses it (not the whole Lexicon up front), and a sigil is used only where the session knows it or the answer holds its name twice: telling the whole Lexicon cost more than a few recalls save. Sigils are § plus a word of up to 7 letters (2 Claude tokens, measured). Only the agent's narration is stripped, never the human's words.
  Warriv: Stopped again with review fixes in, clippy and yardstick running in its background task, then judge.py, plan numbers and merge. Not stuck, no tell.
  Warriv: Assumed: the tongue saves about 10% on real dialogue, not the plan's 2x to 3x. Told it to land anyway if judge scores hold, with the real numbers replacing the guess in memory.md part 8, and off by default if scores drop. Chasing the gap would be a later quest.
  Warriv: Stopped a third time, judge.py running in its background, then part 8 numbers, commit and merge. Not stuck, no tell.
  Warriv: Stopped a fourth time, judge.py still running in its background, then part 8 numbers, commit, merge and quest done. Not stuck, no tell.
- [x] Memory: the Scroll, a Settings section about Horadrim @memory-the-scroll-a-settings-section-5893
  See docs/plan/memory.md, part 8. Drawn as an unrolled scroll: what the tongue is, the project's Lexicon, tokens saved and answer score, and a switch to turn it off. Verify on screen with a dev instance.
  After: Memory: Horadrim, sigils and the Lexicon for recall's answers
  Warriv: Model: sonnet
  From the Horadrim quest: the off switch is the file memory\horadrim.off (horadric_memory::horadrim::set_off / is_off), its own file so it never races the Lexicon in memory\horadrim.json (Tongue::load, Lexicon::saved_this_week, Sigil.uses). The answer score is from crates/horadric-memory/yardstick/judge.py.
- [x] Memory: fetch the model once into models, checked by hash @memory-fetch-the-model-once-into-models-86283
  After: Memory: an embedding model in Rust with candle, and a yardstick
  See docs/plan/memory.md part 1. Download all-MiniLM-L6-v2 (config.json, tokenizer.json, model.safetensors at the pinned commit) into the state folder's models\ on first need, verify the SHA-256s written in the plan, never ship them in the installer. Needed before part 3.
  Assumed: horadric-memory takes the download as a closure (app will pass WinHTTP net::get in part 3, the yardstick uses curl) and SHA-256 is hand written, not a new crate. A 'verified' mark skips rehashing 90 MB on later starts.
  Warriv: Stopped waiting on clippy, tests (queued behind another build) and its review. Not stuck, no tell.
- [x] Memory: the look back misses no dialogue and no Warriv talk @memory-the-look-back-misses-no-dialogue-4758
  From review of part 7: distill picks passages by when said, so ones stored late (backlog still reading at 04:00, a turn still running) are never distilled; pick by store position since the last look back instead. warriv_talk misses a running errand's conversation (no ErrandEnded yet) and Warriv conversations with only older records. Check Warriv can Read memory/day/<project>.md outside the project without a permission prompt. With the across switch, other projects' replaced notes come in as current note copies.
  Warriv: Model: sonnet
- [x] Backup: encrypted snapshots to a folder, and restore @backup-encrypted-snapshots-to-a-folder-36036
  See docs/plan/backup.md, part 1. Format, PBKDF2 and AES-256-GCM through CNG (reuse portal_crypto.rs), the Vault trait exactly as the plan writes it, the folder vault, an in memory vault for tests, and horadric backup now|status|restore. Part 2 runs beside you and implements your Vault trait, so land the trait early.
  Assumed: a file gone from the store stays in the backup (an emptied store is when the backup must not follow it); nothing is ever deleted from the vault.
  Assumed: the manifest carries a sealed check value so a wrong passphrase is caught before any write, even on a backup with no files. Lore *.sanctuary.json is left out with *.code.json (rebuilt layout).
  Assumed: the CLI restore refuses while the app runs (quit from the tray first); stopping only the memory thread is left for the Settings button in part 3. Folder and last result live in backup.json in the state folder; passphrase in Credential Manager as '<state name> backup passphrase'. Changing the passphrase is left for part 3.
  Warriv: Stopped while its own cargo test and review agent run; not stuck (lore #31), no tell.
  Warriv: Stopped again with checks green, waiting on its review agent before committing. Not stuck (lore #31), no tell.
  Warriv: Stopped a third time with full checks running in its background before commit and merge. Not stuck (lore #31), no tell.
  Warriv: Stopped a fourth time, checks running in its background after merging main, then fast-forward. Not stuck (lore #31), no tell.
  Warriv: Stopped a fifth time, main moved so it merged main again and reruns checks before the fast-forward. Not stuck (lore #31), no tell.
  Warriv: Stopped a sixth time, committed and main merged, final checks queued behind another session's clippy, then fast-forward. Not stuck (lore #31), no tell.
- [x] Backup: Google Drive with one click sign in @backup-google-drive-with-one-click-sign-36047
  See docs/plan/backup.md, Google Drive. OAuth for installed apps with PKCE in the default browser (never the browser pane), loopback on 127.0.0.1, scope drive.file plus openid email, refresh token in Credential Manager, POST/PATCH/DELETE added to net.rs over WinHTTP, resumable uploads, a Drive vault implementing part 1's Vault trait. Build sign in, HTTPS and the Drive client first; if part 1's trait is not on main when you need it, quest blocked --on "Backup: encrypted snapshots to a folder, and restore". The OAuth client id and secret are being set up by the human's session and will be added to these notes; until then read them from env HORADRIC_GOOGLE_CLIENT_ID and HORADRIC_GOOGLE_CLIENT_SECRET.
  OAuth client ready (2026-10-16), Desktop app in Google Cloud project horadric-511810: client id 693724238201-dlmjnsqhhg2ndfb24gv2n9bolmgoee9d.apps.googleusercontent.com, client secret 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 (public for an installed app, per Google; compile both in, in crates/horadric-ui/src/google.rs). Drive API is enabled.
  The consent screen is In production for any Google account (scopes openid, userinfo.email, drive.file, all non-sensitive, no logo so no verification). Privacy page: https://horadric.dev/privacy.
  Warriv: Not stuck: the session waits on its own review agent before merging, per lore #31. No tell.
  Assumed: in Drive a machine's backup is one flat folder, each file named by its relative path with the slash in it, so one listing covers a backup. Not yet tried: a real sign in and upload, which needs you: run 'horadric google sign-in' then 'horadric google try'.
- [x] Backup: the Settings section and the schedule @backup-the-settings-section-and-the-38699
  See docs/plan/backup.md, The Settings section and Restore. A Backup section in the Settings window (Off, Google Drive, folder; account; passphrase; last backup; Back up now; Restore), and the app backing up on its own: closed months once, the rest at most hourly when changed. Verify on screen with a dev instance.
  After: Backup: Google Drive with one click sign in
  Warriv: Model: sonnet
  After: Backup: encrypted snapshots to a folder, and restore
- [x] Memory: recall on every prompt, from the UserPromptSubmit hook @memory-recall-on-every-prompt-from-the-36057
  See docs/plan/memory.md, part 9. Only hits above a bar set from the yardstick, never the same hit twice in a session, inscribed. Install the hook with the others and make sure a reload brings it to existing installs.
  Assumed: the bar is cosine >= 0.5 on meaning alone (fused ranks are not a relevance score), at most 3 hits a prompt; slash commands are skipped. Without the model nothing is offered.
  Warriv: Stopped with work committed on its branch, waiting on its review agent before merging and quest done. Not stuck (lore #31), no tell.
- [x] Memory: a recall in each quest's prompt beside the context pack @memory-a-recall-in-each-quest-s-prompt-36067
  See docs/plan/memory.md, part 9. Recall on the quest's title and notes, the strong hits only.
  Assumed: the strong bar is recall::STRONG = 0.032 on the fused score before weighing (about top 3 of words and meaning at once), from the yardstick: 40 of 65 questions get a hit, 1.5 each, 63% right. The window waits at most 1.5 s for it as a quest starts.
- [x] Memory: what the lore knows comes with a Grep or Glob @memory-what-the-lore-knows-comes-with-a-38718
  See docs/plan/memory.md, part 9. When an agent searches for a name, add where it is defined and notes naming it as additionalContext (PreToolUse if Claude Code allows it there, else PostToolUse as overlap.rs does). Keep it short and skip it when the lore knows nothing.
  Warriv: Model: sonnet
  Warriv: "Memory: recall on every prompt" runs beside you and also adds a hook and its install; merge main often and expect to share the hook list and the listener. Merges stepped on the same file twice on 2026-10-16.
- [x] Quest sessions waiting on their own background work are not nudged @quest-sessions-waiting-on-their-own-38728
  Model: sonnet
  On 2026-10-16, 14 times a quest session ended its turn while its own background work ran (Bash run_in_background, a review subagent, judge.py, checks in the heavy queue). The runner's nudge (runner.rs fn nudge, tasks::nudge) fired, the session answered 'not finished, waiting', and the look back (lookback.rs) counted each as a stop without done or blocked. Lore note #31. From hook events (PreToolUse/PostToolUse with run_in_background, an Agent call run in the background), know that a session has work of its own still running, and neither nudge it nor count its stop until a later turn ends with nothing left running. Hooks only, never terminal output. Pure parts get tests; check with a dev instance and a session that starts a background sleep.
  Filed by Warriv for: the day's look back
  Assumed: no hook says background work ended, so the stop after one that started none counts as the end (one background task at a time is assumed). Not checked live with a dev instance, only unit tests.
- [x] Ship Local runs the three checks first and stops on red @ship-local-runs-the-three-checks-first-38946
  Model: haiku
  CLAUDE.md's ship local steps (merge, cargo build --release, reload) never say to run fmt, clippy and tests, and on 2026-10-16 several ship local sessions built without them (lore note #30). Warriv casts Ship Local without waiting for checks on main, trusting the ship session to run them. Add the three checks, through horadric heavy, before the release build to the ship local steps in CLAUDE.md (Developing Horadric from inside Horadric), and say there that a red check stops the ship and is reported. Do not change the stones in .horadric/config.json: their steps say 'Ship local' and pick this up.
  Filed by Warriv for: the day's look back
- [x] Memory: horadric quest pack shows the quest's recall too @memory-horadric-quest-pack-shows-the-38979
  After: Memory: a recall in each quest's prompt beside the context pack
  Warriv: Stopped while its checks wait behind a ship build and a review agent reads the diff. Not stuck (lore #31), no tell.
  Warriv: Stopped again: review fixes done (cfg(windows) on lore call, separate no-app message), checks queued behind a ship build. Not stuck (lore #31), no tell.
- [x] Memory: a quest's recall off the window thread, and the model loaded before the first quest @memory-a-quest-s-recall-off-the-window-39314
  After: Memory: a recall in each quest's prompt beside the context pack
  Warriv: Model: sonnet
  Warriv: Stopped after committing 39f180f on its branch, tests still running before merge. Not stuck (lore #31), no tell.
- [x] Tristram: browser tabs as tiles, so the title click folds the cluster again @tristram-browser-tabs-as-tiles-so-the-32388
  Human feedback 2026-10-17: clicking Tristram's cluster title opens the stage with its browser pane, so the title no longer minimizes the cluster like every other project's does. Their proposal: show each browser tab as a tile in the cluster, like a session. A tile click opens that tab on the stage, the title click goes back to folding. Start with Tristram (docs/plan/tristram.md, 'Always shown'), then ask whether project clusters get browser tiles too. This changes the browser pane decision of 2026-09-29 (docs/plan/browser-pane.md), so write the new rule into both plan files.
  Warriv: Model: opus
  Warriv: Assumed: build it for Tristram only and do not stop to ask about project clusters. Leave their browser pane as it is and write 'browser tiles for project clusters too?' as an open question in docs/plan/browser-pane.md for the human. It can be extended later in one quest.
  Assumed: right click on a tab tile opens the town's header menu (no tab menu yet); tab tiles are not draggable; a town with no sessions and no tabs still opens the browser from its header.
- [x] Accounts: switch in one click, and by itself when an account runs out @accounts-switch-in-one-click-and-by-32399
  Human feedback 2026-10-17: switching accounts is too hard today (Account row buried in the usage window, docs/plan/accounts.md). They want it seamless when out of budget and also for an ordinary switch. Ideas to weigh: switch from the tray menu and a hotkey, not only the usage window; show each kept account's remaining 5 hour and weekly budget in the list so the pick is obvious; when a session hits a usage limit, switch to the account with most headroom by itself (a setting, off by default: the human decided 2026-10-17 to turn it on themselves) and resume the stopped turn; sessions blocked on a limit count as free to restart, so the switch does not wait on them. The human has a Team Premium seat and a personal Max 5x.
  Warriv: Model: opus
  Assumed: the hotkey defaults to Ctrl+Alt+K, not Ctrl+Alt+A, since AltGr+A types a letter on Polish and other layouts. Settable under Keys.
  Assumed: the auto switch is Claude Code only (only its StopFailure reports rate_limit). Not tested live with a real limit hit: the hotkey menu with budgets was checked on screen with fake logins.
- [x] Quest sessions waiting on an earlier turn's background work are not nudged either @quest-sessions-waiting-on-an-earlier-turn-32410
  Model: sonnet
  The day's look back on 2026-10-17 still counted 12 stops without done or blocked (Backup part 1 and Drive, recall on every prompt, quest pack recall, recall off the window thread). Lore note #58: e81cbca's check in crates/horadric-core/src/rarity.rs (Background state) only sees background work started in the turn that just stopped. A session starts checks and a review agent, wakes on the checks' <task-notification>, stops again to wait on the review, and is nudged. Track outstanding background work across turns from hook events only: count each run_in_background Bash/PowerShell and background Agent call started, count each <task-notification> with status completed or failed in a UserPromptSubmit prompt as one ended, and treat a Monitor or ScheduleWakeup call as a wait until the next prompt. Neither nudge (runner.rs) nor count the stop (lookback.rs) while any is outstanding. Pure parts get tests; check on a dev instance against a scratch repo, never the real quest log (lore #51).
  Filed by Warriv for: the day's look back
- [x] A dev instance never takes quests from the installed Horadric's projects @a-dev-instance-never-takes-quests-from-32421
  Model: sonnet
  Lore note #51: on 2026-10-16 the ship public's try of the draft build as a dev instance took 'Memory: no recall on a task notification' from horadric.dev's real quest log, started a session in its own hosts, and when the dev instance and its hosts were stopped the quest sat [/] for 14 hours with nobody on it. The same trap was seen on 10-05 (lore 2026-10#4965) and 10-?? (2026-10#6386). A dev instance (HORADRIC_DEV=1) should not run its quest runner, Warriv or errands on a project the installed Horadric knows, unless told so for a test (for example an env var, or a project folder only the dev instance knows). Say it in CLAUDE.md's 'Verifying Windows code'. Also: a quest marked [/] whose session no running Horadric holds should go back to open so the installed runner starts it again. Pure parts get tests; check on screen with a dev instance beside the installed one.
  Filed by Warriv for: the day's look back
  Assumed: the dev runner skips projects in the installed state.json recent list (HORADRIC_DEV_QUESTS=1 overrides); a [/] quest gone 3 minutes in a running-mode project reopens. Not checked on screen with a dev instance; errands are untouched since the dev tome is armed separately.
- [x] Find why horadric.exe crashes on every build (KERNELBASE 0x87a) @find-why-horadric-exe-crashes-on-every-66332
  Evidence: 69 horadric.exe crashes in the Windows Application log since 10-01 (event 1000/1001), 31 of them since 10-15, same fault in every version from 0.7 to 0.30.1. After a crash the cast scheduler stops until Horadric is started again: no casts 10-16 12:43 to 10-17 09:05, after crashes at 13:04 and 22:19. Assumed: also restart Horadric when it exits, if the fix takes long.
  Assumed: no restart-on-exit watchdog added. The crashes were the old process exiting on reload and quit (reload.log pid matches the crash pid), now fixed. Why casts stopped after 10-16 13:04 and 22:19 is not proven to be the crash; watch for it again after this ships.

## macOS

- [x] Memory: no recall on a task notification, and none of them kept as dialogue @memory-no-recall-on-a-task-notification-78997
  Seen 2026-10-16: the UserPromptSubmit recall fired on a <task-notification> prompt (a background task finishing, not the human) and injected two other sessions' task-notification turns. Skip the hook's recall when the prompt is a system notification (task-notification, system-reminder only), and keep such turns out of the dialogue memory (part 4's transcripts), dropping the ones already stored from recall. See docs/plan/memory.md, parts 4 and 9.
  Warriv: Model: sonnet
  Warriv: Are you still on this quest? Your worktree has no changes after 14 hours. Go on from where you left off (skip the hook's recall on task-notification and system-reminder prompts, keep those turns out of the transcripts, drop stored ones from recall), then land it and run quest done.
  Warriv: Warriv 2026-10-17: the session on this (f2853656) was started by a dev instance during the 0.29.0 ship public (state in Horadric-dev) and died with it; its worktree has no changes after 14 hours. Sent a tell, but no running Horadric holds the session. Handing it to the human to restart. Filed 'A dev instance never takes quests from the installed Horadric's projects' so it does not happen again.
- [x] Browser pane draws pages offset and cropped after resize or project switch @browser-pane-draws-pages-offset-and-78204
  Seen 2026-10-22 in the incotech project. The pane showed the page shifted about 275px left and 300px up, cropped to roughly 1250x950, with white around it, while the page itself reported innerWidth 1525 x innerHeight 1259, scrollX/Y 0, no zoom or transform. Full-page screenshots rendered correctly; normal viewport screenshots showed the crop. Google showed the same crop, so it is not the site. It persisted while the stage showed another project and after switching back. Closing and reopening the tab reset it. Likely cause: the WebView2 bounds or rasterized frame are not updated when the pane is resized, moved or re-shown (stage project switch). Fix: resync WebView2 controller bounds (and NotifyParentWindowPositionChanged) on every pane layout change and when the pane becomes visible again; add a check that innerWidth matches the visible pane size.
- [x] Hand a picked thing to the agent @hand-a-picked-thing-to-the-agent-81421
  Copied from T3 Code's composer context references (docs/internals/composer-context-references.md in pingdotgg/t3code). Let the human pick something on the stage and hand it to a session's prompt: an element picked in the browser pane (its selector, text and a crop of the page), a terminal selection, or a file range from the file viewer. It goes in as a short reference in the prompt, with the full payload on disk the agent can read, so a big pick does not flood the prompt. Decide the gesture (a pick mode in the pane, a context menu on a terminal selection) and which session gets it (the focused pane). Read docs/plan/the-agent-s-cursor.md first, it may already cover part of this. Verify on screen with a dev instance.
  Warriv: Model: opus
  Assumed: terminal selections are handed with Shift+right click, so plain right click keeps copying as in every Windows terminal. The file viewer has no paste, so its plain right click opens the Copy / Hand to menu.
  Assumed: the browser pick starts from the browser pane header's right click menu (Pick an element for <agent>), with no shortcut yet. Each menu lists the stage's agents, the one that last had the keyboard first, so the target is explicit.
  Assumed: payloads go to %TEMP%\Horadric\pick-*.md (and a .png crop) beside pasted images, pruned after a day. A file range gets no payload, the reference names the file and lines. The reference is pasted, never sent, so the human adds what to do with it.
  Warriv: The fast-forward 89d8570..3004c17 was refused because another live session has uncommitted edits in crates/horadric-ui/src/app.rs, console.rs and lib.rs, which the branch also touches. No session holds this quest any more, so filed "Land the picked thing branch" to merge it once those files are committed.
- [x] Land the picked thing branch @land-the-picked-thing-branch-86098
  Model: sonnet
  The branch hand-a-picked-thing-to-the-agent (2de8ac5, 3004c17: pick.rs plus app.rs, console.rs, lib.rs, pane.rs, terminal.rs, web.rs, README) finished its quest but did not fast-forward into main: another live session had uncommitted edits in crates/horadric-ui/src/app.rs, console.rs and lib.rs in the shared tree. Wait until git status shows those three files clean (never stash or checkout another session's files), merge main into the branch, resolve any conflict keeping both sides, run fmt, clippy and test through horadric heavy, then git -C the main checkout merge --ff-only the branch. Change nothing else.
  Warriv: Stall at 01:55 is expected: the far/Brotherhood session is live (app.rs, console.rs edited 01:41 to 01:50, commit 8464211 at 01:50). The quest's own git diff condition resumes it once those files are committed. No tell, no hand on.
  Warriv: Round 05:01: app.rs, console.rs and lib.rs are clean on main (committed by the Brotherhood session), so told the session to merge main in, recheck and fast-forward. If no session answers by next round, hand back to the human with: restart it.
  Outcome: sonnet, 355m, green, 586k tokens
