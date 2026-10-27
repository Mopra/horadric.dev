# One portal for the whole brotherhood

Asked for on 2026-10-27: with Horadric on a desktop, a laptop and a VPS,
the phone should see every brother and every session, simply, as tabs.
Today the two do not fit together. Brothers see each other's sessions on
the stage, but a phone pairs with one PC at a time (the page keeps a
list and shows one, behind a dropdown), every PC has to be paired on its
own, and a Linux node cannot be paired at all.

This reverses a decision of [The Brotherhood](the-brotherhood.md)
("a phone stays paired with one PC") and answers its first open
question. The human chose it knowing the cost: a stolen phone reaches
every brother until it is forgotten. Forgetting it once, anywhere, cuts
it off everywhere, and that is what keeps the cost bearable.

## Decisions

- **Pair once, see everything.** A phone pairs with any one brother,
  as today by a QR code. That brother vouches for it in its signed word,
  and every brother that hears the word serves the phone from then on.
- **Each brother serves the phone itself.** The phone opens a socket to
  every brother through the relay (`/phone?id=<that brother's relay
  id>`), as it does to its one PC today, and each brother serves only
  the sessions it owns. No brother passes another's sessions on, the
  same rule brothers keep among themselves. A brother that is asleep or
  out of reach is a tab with a grey dot, not a missing tab.
- **The phone has a key pair, not a key.** The page makes an ECDH P-256
  key pair with WebCrypto (the private half not extractable, kept in
  IndexedDB) and sends the public half when it pairs. Its key with each
  brother is derived, never sent: the ECDH of the phone's key and the
  brother's long term key, through `joining::derive` with the label
  `"horadric phone key v1"`, the shared secret, the phone's public key,
  the brother's public key, the phone's id and the brother's relay id.
  So no brother holds a key that opens the phone's link to another, and
  a brother that is cast out learns nothing it could pose as the phone
  with. Public keys travel as b64url of the 65 byte uncompressed point;
  the shared secret is the 32 byte x coordinate (WebCrypto's
  `deriveBits` of 256). Pairing v3: the phone's `pair` (sealed with the
  ticket, as today) carries `"v":3` and `"key"`; the brother answers
  `{"t":"paired","phone","brother":"<its relay id>","bkey":"<its public
  key>"}`, and from then on both seal with the derived key and the
  phone's hello carries `"v":3`. A brother tells a phone sealed with a
  derived key from a v2 phone by trying every key it has, as today.
- **Two new words.** In `said.rs`, beside `admits` and `cast_out`, both
  with `n` 0:

  | kind | fields | says |
  |---|---|---|
  | `vouches` | `phone`, `key`, `name` | `by`'s human paired phone `phone`, whose public key is `key` |
  | `forgets` | `phone`, `key` | `by`'s human (or the phone itself, through `by`) forgot that phone, for good |

  A phone is **served** by a brother when some `vouches` of its key is
  signed by a member that is in (not out, not contested, by `standing`),
  and no `forgets` names its key. `forgets` from any member that is in
  wins over every `vouches`, whatever came first, as cast out does: it
  is worked out afresh from the words, never stored. A forget counts
  from a contested brother too (only one that is out says nothing),
  since a forget only ever takes a phone away and the contested one may
  be the honest one. A signer's own forgets free their places in its
  16. A brother cast out
  takes the phones it alone vouched for with it, so a stolen PC cannot
  keep adding phones. Of one signer's `vouches`, only the 16 naming the
  lowest keys count, so a thief cannot make every brother hold a
  thousand phones. Pairing the same phone again after a forget is a new
  key pair, never the old key back.
- **The phone learns its brothers from any of them.** After hello, a
  brother sends `{"t":"brothers","list":[{"id","name","key","mark",
  "state"}]}`: every member that is in, with its relay id, its public
  key and whether it is linked to this brother now (`live`, `asleep`,
  `away`). The page trusts it because it came sealed from a brother it
  paired with or one that brother named. A brother that has not heard
  the vouch yet opens none of the phone's frames; the page shows its tab
  as "not reached yet" and tries again when the list changes.
- **Forgetting.** "Forget" on any brother (the Town Portal menu,
  `horadric portal forget`) signs `forgets`. Each brother that takes it
  sends the phone `unpaired` if connected and drops it within 2
  seconds, as today. The page's own "Forget this phone" sends `forget`
  to one brother, which signs it for the phone. A phone not seen by any
  brother for 30 days is forgotten by the brother that vouched for it.
- **Typing is a switch per brother.** "Phones can type here" in the
  brother's `standing.json`, on by default. Off, a phone sees that
  brother's tiles and screens and sends nothing that changes anything:
  no input, start, resume, create, stop, handoff or takeover. A VPS the
  human never sits at can be made watch only from the phone's side of
  the risk.
- **Old phones keep working.** A v2 phone with a symmetric key keeps its
  one PC. A v3 PC answers its hello with `"brotherhood":true`, and the
  page offers "See every brother": it makes its key pair and sends
  `{"t":"upgrade","key"}` over the link it has, which the PC vouches for
  as if it paired. No new scan. The old key stays good with that PC.
- **Push from the brother that paired it.** A browser holds one push
  subscription, bound to one VAPID key, so only one brother can push to
  it. That brother already sees every brother's sessions on its stage,
  so it pushes for a waiting session on the VPS as for its own, and the
  notification names the brother when it is not the only one. If it is
  asleep, nothing pushes; a later step may hand the subscription to
  another brother along with the VAPID key over their sealed link.

## The page

- **Tabs along the top:** "All", then one per brother in roster order,
  each with the brother's mark and a dot (green live, amber asleep, grey
  out of reach). With one brother there are no tabs.
- **All** groups sessions by project, as the stage does, each row with
  its brother's mark and, when a project lives on two brothers, its
  name. Waiting sessions first, as now.
- **A brother's tab** is today's page for one PC: its sessions, its
  projects for "new", its silent brothers to take over from.
- Opening a session opens it from its owner, whichever tab it was
  tapped in. A session handed to another brother moves tabs by itself,
  since its new owner lists it.
- The PC dropdown goes. Phones paired with PCs of different
  brotherhoods (or none) still show each as a tab of its own.

## On the desktop

- The Town Portal key's menu and the tray's Brothers submenu become one
  menu: the brothers (linked, asleep, out of reach, "Cast out..."), the
  phones (each with which brother vouched for it, "Forget everywhere"),
  "Pair a phone...", "Phones can type here", "Close the portal".
- The pairing page says the phone will see every brother.

## Steps

1. **The words.** `vouches` and `forgets` in `said.rs` with their
   signing bytes, `phones()` as a pure function of the words and
   `standing`, `mesh.rs` signing this machine's own. Tests for each, and
   two invariants in both simulations: a phone forgotten by an honest
   brother is served by no honest brother that holds the word; a phone
   vouched for only by a brother that is out is served by none.
2. **The phone key and serving.** Pairing v3 (the phone's public key in
   `pair`), the derived key, a channel opened by a vouched phone's key
   in `serving.rs` so a Linux node serves phones too, `brothers`,
   `upgrade`, `forget`, the typing switch. PORTAL.md gains it. Verified
   with "Alpha", "Beta" and a dev node by a script phone: paired with
   Alpha, served by all three, forgotten on Beta, cut off by all three.
3. **The page.** Key pair, a socket per brother, tabs, All, push per
   brother, the upgrade offer. In `../horadric.portal`. Verified in the
   browser pane against the three.
4. **The desktop menu.** The merged menu and "Forget everywhere".
   Verified on screen.

Step 3's tabs and All can land first for phones already paired with
several PCs by hand: it needs only the socket per PC, which the page
can open today.
