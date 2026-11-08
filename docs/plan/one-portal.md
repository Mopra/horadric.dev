# One portal for the whole brotherhood

Asked for on 2026-10-27: with Horadric on a desktop, a laptop and a VPS,
the phone sees every brother and every session, as tabs. Built on
2026-11-01: pairing v3 with the phone's key pair and derived keys,
vouching on pair, forgetting by signed word, a Linux node serving vouched
phones, the `brothers` list, the upgrade, the typing switch, the page's
socket per brother, the Town Portal key as the menu of brothers and
phones with "Forget everywhere", and push for every brother's waiting
sessions from the PC holding the phone's subscription. Also built that
day and live: pairing a phone by signing in, which finds the human's PCs
and is let in on one after its human compares a six digit code (lore
#431: a login only finds devices, it is never what they trust). The QR
code stays as the way in without an account; the protocol is PORTAL.md,
"Pairing v4". The protocol is in PORTAL.md, "Pairing
v3"; the decisions and traps are in the lore.

## Left

- **Verified on screen and by a script phone**: paired with Alpha, served
  by Beta and a dev node, forgotten on Beta, cut off by all three. Not
  run yet. The node's phone serving (`node_relay.rs`) was not compiled
  for Linux here; CI is the first to.
- **Pairing by signing in, tried for real**: a phone signs in on the live
  page, picks a PC, both show the same code, and after both yeses it is a
  v3 pairing. Each half is tested on its own (the page and relay against
  a fake PC and fake WorkOS in `horadric.portal`, the PC in
  `asking.rs`, both against one pinned vector), never together.
