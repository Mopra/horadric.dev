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
  its sessions and files, shows brothers' tiles and panes, hands its
  sessions off and takes them in (asking its human where a project
  goes), keeps a handed off tile's place, files a brother's snapshots as
  its keeper and is fenced when a session of its own is taken over
  elsewhere (`mac/brothers.rs`, `mac/far.rs`, `mac/handing.rs`,
  `mac/keeping.rs`, the shared part in `horadric_core::handoffs`,
  `sworn` and `afar_tiles`), all unverified until a Mac run tries it.
  Left: the Mac naming a keeper and sending it snapshots (its turn ends
  reach no keeping yet), taking a silent brother's session over on the
  Mac, handing off a Folio (only a session in a pane goes), and lore,
  which waits for the Mac to have a lore at all. Brothers' limits of the
  Mac's own account show in its usage window.

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
