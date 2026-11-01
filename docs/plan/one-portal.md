# One portal for the whole brotherhood

Asked for on 2026-10-27: with Horadric on a desktop, a laptop and a VPS,
the phone sees every brother and every session, as tabs. Built on
2026-11-01: pairing v3 with the phone's key pair and derived keys,
vouching on pair, forgetting by signed word, a Linux node serving vouched
phones, the `brothers` list, the upgrade, the typing switch, the page's
socket per brother, the Town Portal key as the menu of brothers and
phones with "Forget everywhere", and push for every brother's waiting
sessions from the PC holding the phone's subscription. The protocol is in PORTAL.md, "Pairing
v3"; the decisions and traps are in the lore.

## Left

- **The tray's Other PCs.** The Town Portal key's menu now holds the
  brothers too; the tray still has its own Other PCs submenu, kept since
  it also holds signing in, Linux nodes and keeping. Whether to trim it
  is the human's call.
- **Verified on screen and by a script phone**: paired with Alpha, served
  by Beta and a dev node, forgotten on Beta, cut off by all three. Not
  run yet. The node's phone serving (`node_relay.rs`) was not compiled
  for Linux here; CI is the first to.

## Signing in (asked 2026-11-01, being built)

A phone pairs by signing in on the page instead of scanning a QR code.
The login finds the human's PCs; one of them lets the phone in after its
human compares a six digit code, the same rule as a brother joining
(lore #431: a login only finds devices, it is never what they trust).
After the yes the phone is a v3 pairing like any other. The QR code
stays as the way in without an account. The protocol is PORTAL.md,
"Pairing v4". Needs a relay deploy and the page's redirect URI in
WorkOS (production and staging) before it works live.
