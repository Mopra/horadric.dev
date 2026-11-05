# Implementation plan

Where Horadric is, what comes next, and what was decided along the way. It
is the handover document: someone picking the project up cold should need
nothing else.

This page is the index of work still to do. Each part is a file of its
own in [plan/](plan/), with its reasoning and decisions. Read this page,
then open only the parts your work touches. A new part is a new file in
`plan/` and a line here under Next.

When a part is built, its decisions, traps and untested corners go into
the lore as notes (`lore_note`, naming the code files), and its file and
line here are deleted. So what was decided about anything already built
is found with `lore_recall`, by question or by file, and the old plan
files are in git history (removed 2026-10-19).

Ideas that are not planned yet, most of them from the Diablo name, are in
[IDEAS.md](IDEAS.md).

## Shape of the thing

Seven crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `horadric-core` | Session, Phase, Registry, the state machine | any |
| `horadric-hooks` | The localhost listener, its client, the settings installer | any |
| `horadric-link` | The relay and the crypto for linked machines | any |
| `horadric-pty` | Pseudo consoles (ConPTY, and a unix half), the session host and its pipe protocol | Windows, unix |
| `horadric-ui` | Cluster and terminal windows, layout, drawing (`src/mac/` for AppKit) | Windows, macOS |
| `horadric-memory` | Embeddings, the memory store, recall, Whisper for dictation | any |
| `horadric` | The command line, `horadricw` for Explorer, the wiring | Windows, macOS, Linux |

`horadric-core` and the pure halves of `horadric-ui` (`layout`, `columns`, `theme`,
`palette`, `keys`, `frame`, `files`, `viewer`, `highlight`, `shell`, `board`) have no
I/O and
are tested. `horadric-pty` has a
test that runs `cmd.exe` in a real pseudo console. Everything else is
verified on screen.

Threads: the listener thread owns the socket, a feeder thread applies events
to the registry, the UI thread owns every window and never blocks. Each
console adds two: a reader that parses what its session host sends into
the grid and notices the exit, and a writer that owns the pipe's sending
side so typing never blocks the UI. The host process has its own. Everything off the UI thread reaches it as a message to one
hidden message-only window, never a thread message: thread messages are
dropped while Windows runs a modal loop, and dragging a terminal's edge is
one. The registry is an `Arc<Mutex<Registry>>`, each console's grid an
`Arc<Console>` with the terminal behind a mutex.

## Next

- [One portal](plan/one-portal.md): a phone paired once sees every brother and every session. Built; left: the test with three machines, and pairing a phone by signing in (being built).
- [macOS](plan/macos.md): for others who want it, the human uses no Mac, so lowest priority. The Folio, browser pane, quests, files, usage, Discord and Settings are built, unverified on a real Mac; left: the rest of each, and switching accounts.
- [Lore](plan/lore.md): watch quest compaction run for real.
- [Memory](plan/memory.md): built; left: arming the daily distill.
- [The Brotherhood](plan/the-brotherhood.md): built, the Mac too; left: the Mac's keeper, taking over and leaving.
- [Secrets](plan/secrets.md): tokens for quests that push, call APIs or deploy.
- [Identities](plan/identities.md): each project's gh, git, npm and Vercel logins, so no session switches another's account.
- [Signals](plan/signals.md): built; left: a look on screen, and the Mac's cost lines and progress.
- [Dictation](plan/dictation.md): built, with a global chord; left: a bigger model, Danish, a plate outside Horadric, and the Mac's key and draft.
- [Towns](plan/towns.md): several Tristrams, work and private, each with its own Claude login and browser profile. Agreed 2026-11-05, reopening `CLAUDE_CONFIG_DIR` for towns only.
- [The Forge](plan/the-forge.md): files worked on with Claude beside the session (decks, artefacts, markdown, images), pinned, edited by hand, versioned and exported. Proposed.
- [The hirelings](plan/hirelings.md): built; left: an on-screen check.
- [GPU light](plan/gpu-light.md): every glow, shadow and spark drawn with real bloom, the shadow effect and particles instead of stacked shapes, window by window, then the Mac. Steps 1 and 2 are built; step 3 waits on the human's look.

## Reference

Specifications of built parts that the code follows. Where one and the
code disagree, one of them is a bug.

- [Who owns a session](plan/brotherhood-ownership.md): the brotherhood's signed word, whose a session is and who is out, with the invariants both simulations check.
