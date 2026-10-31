# One portal for the whole brotherhood

Asked for on 2026-10-27: with Horadric on a desktop, a laptop and a VPS,
the phone sees every brother and every session, as tabs. Built on
2026-11-01: pairing v3 with the phone's key pair and derived keys,
vouching on pair, forgetting by signed word, a Linux node serving vouched
phones, the `brothers` list, the upgrade, the typing switch, the page's
socket per brother, and the Town Portal key as the menu of brothers and
phones with "Forget everywhere". The protocol is in PORTAL.md, "Pairing
v3"; the decisions and traps are in the lore.

## Left

- **Push for every brother's sessions.** A browser holds one push
  subscription, so only the brother the phone paired with pushes. It
  should push for a waiting session on any brother, as it sees them on
  its stage, naming the brother when there is more than one. Today
  `portal_waiting` and `portal_push_payload` in `portaling.rs` take only
  this PC's own sessions, and the push's tap opens a session by this PC's
  id, where a brother's session needs its owner's relay id and its own
  id there.
- **The tray's Other PCs.** The Town Portal key's menu now holds the
  brothers too; the tray still has its own Other PCs submenu, kept since
  it also holds signing in, Linux nodes and keeping. Whether to trim it
  is the human's call.
- **Verified on screen and by a script phone**: paired with Alpha, served
  by Beta and a dev node, forgotten on Beta, cut off by all three. Not
  run yet. The node's phone serving (`node_relay.rs`) was not compiled
  for Linux here; CI is the first to.
