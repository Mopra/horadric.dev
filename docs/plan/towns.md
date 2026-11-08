# Towns

Asked for on 2026-11-05. The human now prefers working in Horadric to
any other tool, and wants the rest of the day in it too: Outlook, Gmail,
Slack, Jira, monday and Discord. Horadric will not grow a mail client or
a calendar. Claude sessions already reach most of these through
connectors, and the browser pane shows the rest. What is missing is a
place for that work that keeps work and private apart.

Some of it is work (Acme: Outlook, Slack, Jira, monday), some is
private or the human's own company (Gmail, Discord). Tristram, the town
above the projects, is where non project work goes today, and there is
one of it. This part answers the open question "one town or several":
**several, each with its own Claude login and its own browser profile.**

## What a town is

- **A name and a folder.** `%USERPROFILE%\Horadric\towns\<slug>`, with
  the existing `Horadric\tristram` kept as the first town so nothing
  moves. A dev instance keeps its own under `Horadric-dev`. Each town is
  an ordinary project keyed by its folder, as Tristram is now, so
  stage, panes, hooks, quests and stash work unchanged.
- **A Claude home, or none.** None means the machine's login, which is
  today's behaviour. A town with a home starts its sessions with
  `CLAUDE_CONFIG_DIR` pointing at it, so it runs on its own Claude
  account at the same time as the rest. That account's claude.ai
  connectors (Microsoft 365, Slack, Gmail) come with it, which is the
  whole point: the work town sees work mail, the private town sees
  Gmail, and neither can see the other's.
- **A browser profile.** The work town's Outlook and Jira logins never
  sit in the private town's browser, and the other way round.
- **An identity, later.** When the [Vault](vault.md) is built, a
  town names one like a project does, so `gh` and git follow it too.

The human names the towns. Tristram stays one of them.

## The decision this reopens

[Accounts](vault.md) switches the Claude login for every session
at once, and a `CLAUDE_CONFIG_DIR` per account was turned down (lore
#268) for two reasons: each home would need the hooks, and a
conversation could not be resumed on another account. **The human
agreed on 2026-11-05 to reopen it, for towns only.**

Why it holds up for towns: a town's sessions never move to another
account, so resuming across accounts is not needed there. The hooks are
the real cost, and step 3 pays it. Projects keep the machine wide login
and Accounts as they are. A switch in Accounts acts on the machine's
login only and leaves every town with a home of its own alone.

## What the code has now (checked 2026-11-05)

- One Claude home for everything. `App::launch`
  (`crates/horadric-ui/src/app.rs`) builds a session's environment from
  `session_env` and the usage defaults; nothing sets `CLAUDE_CONFIG_DIR`.
  The pty `Command` already takes `env_set`, so the plumbing is there.
- Hooks and the Claude mod are written only to `~/.claude/settings.json`,
  hardcoded to the user's home (`crates/horadric-hooks/src/install.rs`).
  A second home would get no hooks and no mod. The Horadric MCP server
  and the status line are passed per session, so those would work.
- Usage merges every session into one `Usage`, on the stated assumption
  that limits belong to the one account (`horadric-core/src/usage.rs`).
- One WebView2 profile, `%LOCALAPPDATA%\Horadric\web`, shared by every
  project on purpose (`crates/horadric-ui/src/web.rs`).
- One town: a single `OnceLock` key and a single `town_hidden` flag
  (`crates/horadric-ui/src/town.rs`, `app.rs`).

## What was built (2026-11-08)

Steps 1 to 4 are built, step 5's CLAUDE.md listing too, not yet looked
at on a dev instance.

- The towns are `towns` in the state (`horadric-core/src/towns.rs`),
  Tristram first; an old `town_hidden` is read into Tristram once. The
  app keeps them in `horadric-ui/src/town.rs`, the Settings rows are in
  `town_settings.rs`.
- **A town has its own Claude home when its folder holds `claude`.** No
  flag in the state: the installer, the transcript lookups, trust and the
  sessions all go by the folder, so a command line process agrees without
  reading the app's state. Going back to the projects' login renames it to
  `claude.off`, kept for next time.
- The hooks and the mod go into every town home on `install`, `reload`,
  `hooks install` and each app start. A dev instance writes them into its
  own town homes (under `Horadric-dev`, at its own port) and still never
  touches `~/.claude`.
- Browser profiles: a project whose identity resolves gets
  `identity-<slug>`; a town gets one only from an identity picked for it,
  else `town-<slug>` when Own browser is on. New towns start with it on;
  Tristram keeps the shared profile it had, so its logins stay.
- Usage: a town home's limits are kept apart (`home_usage` in the state)
  and shown as extra rows named by the town. Warriv drives only projects,
  which all run on the machine's login, so it already paces on that
  login's limits alone.

## Steps

1. **Several towns.** Towns kept in the state as a list (name, slug,
   folder, Claude home or none, browser profile). The first is today's
   Tristram. Settings gets a Towns section: add, rename, hide. Each town
   is a cluster of its own, its name on the cluster so the human always
   sees which side they are on. `town_hidden` becomes per town. The
   generated CLAUDE.md in each town's folder says what the town is for
   and which connectors it has.
2. **A browser profile per town.** WebView2 profiles in the one user
   data folder (`ICoreWebView2ControllerOptions` with a `ProfileName`),
   so one environment and one DevTools port serve every profile.
   Projects keep the default profile. The browser tools already go
   through Horadric, so an agent in a town drives its town's profile.
3. **A Claude home per town.** The town's sessions start with
   `CLAUDE_CONFIG_DIR=<town>\claude`. The installer writes the hooks and
   the mod into every town home as well as `~/.claude`, and `reload` and
   uninstall do the same. Transcript lookups take the session's home,
   not the process's. "Log in" on a town runs `claude auth login` in a
   console with that home, as Add account does now. Accounts leaves town
   homes alone.
4. **Usage per account.** `Usage` keyed by Claude home, so the private
   town's limit never reads as the work one's. Warriv and errands pace
   on the limit of the home they will run in. The usage window shows one
   row per account in use.
5. **Connectors.** Nothing to build: the human adds them on claude.ai for
   each account (work: Microsoft 365, Slack, Atlassian, monday; private:
   Gmail and the rest). Discord has no connector, so it lives in the
   browser pane. The town's CLAUDE.md lists what is connected so agents
   do not guess.

Later, not planned: a glance on a town's tile (unread mail, next
meeting), read through the connectors by a cheap errand, never by
polling every paint.

## Pure and tested

A town's slug and folder, the environment a town's session gets, which
homes the installer writes to, and `Usage` keyed by home.

## Verifying it

On a dev instance with two towns, one with a Claude home logged in to a
second account: a session in each runs `/status` and shows its own
account, and both run at once. A hook event from the town with a home
moves its tile. The usage window shows two rows. Each town's browser
keeps its own login after a restart. A switch in Accounts leaves the
town with a home on its own account.

## Open questions

- Whether a project session may hand a question up to a town, and
  which one (still open from Tristram).
- Browser tabs as tiles in project clusters too, not only in a town's
  (moved from PLAN.md's loose ends).
- Where a town's folder lives on macOS.
