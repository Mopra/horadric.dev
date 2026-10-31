# The Brotherhood

Every machine with Horadric on it, joined into one brotherhood. Steps 1
to 18 are built (signing in, joining, the roster and its signed words,
remote tiles and panes, Linux nodes over SSH and the relay, handing off,
keepers and takeover, the shared lore): ask `lore_recall` what was
decided about them. The wire is in [PORTAL.md](../PORTAL.md), the
ownership rules in [brotherhood-ownership.md](brotherhood-ownership.md).
What is left is below.

## Still to do

- **The Mac as a brother, the rest.** The Mac signs in (device code),
  joins with the six digit code and serves its sessions to brothers
  (`mac/brothers.rs` on `horadric_link::relay` and `signin`), unverified
  until a Mac build runs it. Left: showing brothers' tiles and panes on
  the Mac (its client of each brother: hello, rosters, keepers, lore),
  handing off and keeping, serving its files (`tree`, `read`, `changes`),
  and moving the Linux node onto `horadric_link::relay` and `signin`.
- **Teams, the rest.** A machine already treats one the relay marks
  `"kin":"team"` as a teammate's: view only from its join, and no lore
  either way (docs/PORTAL.md, "Proposed: teams"). Left, each needing the
  human: the relay grouping an organization's people and saying `kin`
  (a production relay deploy), WorkOS organizations switched on for the
  login, and then holding dialogue back so a teammate can get a
  project's notes and quest summaries, with a per project switch, and
  the tray marking a teammate's machines.

Each of these is its own decision when it comes.

## Open questions

- Should a quest follow a takeover too? It moves with a handed off
  session, but the silent owner's quest log keeps a taken over one held
  by a session that now runs elsewhere. Nor does it follow a session
  that lands outside a worktree of its own, or on a Linux node, which
  has no quest log; the sender's copy then waits blocked, naming the
  brother, for the human.
- Handing off a plain terminal: a shell has no transcript to resume, so
  probably never, only its folder.
- In a team, should dialogue be shared at all? The lore holds
  conversations word for word. Notes and quest summaries are meant for
  others; a teammate's dialogue may not be. Likely notes and quests by
  default, dialogue only within one person's own brothers.
- Who pays when teams use SSO? AuthKit is free, enterprise SSO is priced
  per connection. Fine to defer until a team asks for it.
