# The Brotherhood

Every machine with Horadric on it, joined into one brotherhood. Steps 1
to 18 are built (signing in, joining, the roster and its signed words,
remote tiles and panes, Linux nodes over SSH and the relay, handing off,
keepers and takeover, the shared lore): ask `lore_recall` what was
decided about them. The wire is in [PORTAL.md](../PORTAL.md), the
ownership rules in [brotherhood-ownership.md](brotherhood-ownership.md).
What is left is below.

## Still to do

- **The Mac as a brother, the rest.** The Mac signs in, joins, serves
  its sessions and files and shows brothers' tiles and panes
  (`mac/brothers.rs`, `mac/far.rs`), all unverified until a Mac build
  runs it. Left: handing off and keeping (the Mac sends no `handoff`,
  takes no `bases`/`bundle`, keeps no one's sessions and asks no keeper),
  a handed off tile keeping its place, and lore, which waits for the
  Mac to have a lore at all.
- **Teams, the rest.** Built and the relay deployed (docs/PORTAL.md,
  "Teams"). Left: a real organization in WorkOS production with two
  people in it (the human's call, since inviting sends mail), and a run
  between their two PCs, which nothing has done yet. The Mac and the
  Linux node do not dial with `team=1`, so they never see a teammate
  until they apply the team rules themselves.

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
- Who pays when teams use SSO? AuthKit is free, enterprise SSO is priced
  per connection. Fine to defer until a team asks for it.
