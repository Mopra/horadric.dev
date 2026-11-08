# The Forge: working on files together with Claude

Proposed 2026-10-30. Files Claude and the human work on (decks,
artefacts, markdown, images, Office files) shown beside the session in
the browser pane, pinned, edited by hand, versioned and exported.

Steps 1 to 8 of the first plan are built: file tabs for every format,
pins (on files and, since 2026-11-08, on ordinary web pages), the deck
shell and presenting, the artefact runtime, versions, hand edits, Office
files by conversion, and the `forge_*` tools with their prompt line. How
each works and why is in the lore, not here.

One change from the first plan: a file a session writes no longer opens
in the Forge by itself. The switch in Project settings is off by default
(decided 2026-11-06), because developing with an agent writes many files
and a tab for each was too much.

## What is left

- **Pins on PDF and Word files.** A PDF shows in WebView2's own viewer,
  and a `.docx` is converted to one, and that viewer runs no script of
  ours, so neither can be pinned. Either Horadric draws the box over the
  viewer itself, or the pages are rendered to pictures as `.pptx` slides
  are and shown as a deck.
- **Web page pins are not kept.** A pin on a web page goes to the session
  and its rune is drawn by the page, but the host does not hold it:
  `forge_pins` does not list it, and the runes go when the page does.
  Keep them by address if sessions turn out to want them again.
- **macOS.** WKWebView has the pieces (a custom scheme handler for the
  virtual host, `createPDF`, snapshots). Goes in [macos.md](macos.md).

Not planned: colleagues looking at a deck through the Town Portal or a
link, and hand edits on `.pptx` ("open in PowerPoint" covers them).
