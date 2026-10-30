# The Forge: working on files together with Claude

Proposed 2026-10-30. The human's work at Optipeople is mostly
presentations, slides, designs, documents and Claude artefacts. In
Claude desktop a thing Claude makes opens beside the chat and both work
on it there. The Forge is that, in Horadric, and better: the work is a
real file in the project, it is shown as Claude writes it, and the human
points at the part to change instead of describing where it is.

Named for the Hellforge, where items are made.

## What it must do

1. Show any file Claude or the human makes, beside the session, and
   keep it current as it changes on disk.
2. Let the human point at a part of it (a slide, a heading, a box in an
   image) and say what to change. The session gets the words, exactly
   where they point, and a crop of the spot.
3. Let the human make small changes by hand, in place, without a
   round trip through Claude.
4. Keep every version, step back through them, restore one.
5. Get the work out: present, PDF, PNG per slide, `.pptx`, copy as an
   image, open in the program that owns the format.

## Decisions

**The web view draws it (decided 2026-10-30).** The settled rule kept
the one web view for web pages. The human allowed it to also show the
files worked on with Claude: they are the human's content, not
Horadric's own UI, which stays Direct2D. No second web view: see the
next point.

**File tabs live in the browser pane.** The browser pane already has
tabs, a pane on the stage, popping out to a window of its own, one
profile, and the agents' `browser_*` tools. A Forge tab is a browser tab
whose page is a file. So Claude can already screenshot and inspect its
own work with the tools it has, and the human gets one place beside the
sessions, not two. When the chosen tab is a file, the pane's bar shows
the Forge's controls (pins, history, export) instead of the address.

**Files are files.** What is shown is a file on disk in the project
folder, or in Tristram's folder for work that belongs to no project.
Nothing is kept inside Horadric that a file could hold. Any editor,
PowerPoint, git and Claude's own tools see the same bytes.

**Served from a virtual host, read only.** WebView2's
`SetVirtualHostNameToFolderMapping` maps the project folder to
`https://forge.horadric/`, so relative links, images and stylesheets in
a deck work as they would on a server. Writes never go through the page:
an edit by hand is a message to Horadric, which writes the file.

**Shown as Claude writes.** A file tab opens or reloads when a session
writes a file the Forge can show. Folio sessions see it in the stream's
`Write` and `Edit` steps; terminal sessions in the `PostToolUse` hook,
which already lights files for the Arcane Sanctuary. Behind a switch per
project, on by default, and never more than one tab per file.

**Pins are the way to point.** A pin is a place in a file plus the
human's words. It is sent to the session as text Claude can act on
without guessing (file, slide, selector or source lines, the element's
text) with a crop of the spot as an image. Pins stay drawn as small
numbered runes on the file until Claude's next write to it lands, then
fade. Runes, not glows.

**Concurrent edits are safe by the tools we already have.** Claude
Code's `Edit` refuses a file that changed since it read it, so a hand
edit between Claude's read and write fails loudly and Claude reads
again. The Forge writes a hand edit in one go and never while a write by
a session to the same file is in its `PreToolUse` to `PostToolUse`
window; it waits it out.

## Formats

| Kind | Files | Shown by | Pin lands on |
|---|---|---|---|
| Markdown | `.md` | Our renderer to HTML, each block tagged with its source lines | Source line range plus the block's text |
| HTML and artefacts | `.html`, `.htm` | As is | CSS selector, outer HTML trimmed, text |
| React artefacts | `.jsx`, `.tsx` (one file, default export) | The artefact runtime, below | Selector and text, plus the component's source line when known |
| HTML decks | `.html` with `<section class="slide">` per slide | As is, with the deck shell's keys | Slide number, then as HTML |
| PowerPoint | `.pptx` | Converted to one PNG per slide, shown as a deck | Slide number and a box in pixels |
| Word | `.docx` | Converted to PDF | Page and a box |
| PDF | `.pdf` | WebView2's own viewer | Page and a box |
| Images | `.png`, `.jpg`, `.gif`, `.webp`, `.svg` | As is, zoomable | A box in pixels (SVG: also the element) |
| Diagrams | `.mmd`, fenced `mermaid` in markdown | Mermaid in the runtime | Node or edge id |
| Tables | `.csv` | A plain sortable table | Row and column |

**The artefact runtime.** Claude.ai's artefacts are single file React
components using Tailwind, `lucide-react`, `recharts` and shadcn's
components. Claude writes them the same way here because the human asks
for "an artefact". The Forge shows them with a runtime bundled into the
binary and served from the virtual host: React, a JSX transform,
Tailwind's class compiler, and those libraries, all offline. It is the
one place this plan adds dependencies; each goes in
[dependencies.md](../dependencies.md) with its size and why when it
lands. Imports outside the bundle are an error shown in the tab, not a
fetch from a CDN.

**The deck shell.** HTML decks are the format Claude makes best and the
Forge handles best: one file, a `<section class="slide">` per slide,
16:9, a stylesheet beside it. The shell (injected, not written into the
file) scales slides to the pane, gives arrow keys, a slide strip, and
presenting full screen. A project can hold its house style in
`.horadric/forge/` (a stylesheet and a starting deck) and the Forge
tells the session about it when it opens a deck, so every deck Claude
makes looks like Optipeople's.

**PowerPoint files.** Shown by converting each slide to PNG: through
PowerPoint itself over COM when it is installed (exact), else
LibreOffice headless when it is (close), else a message saying which to
install. Conversion runs on a thread, cached by the file's hash.
Claude edits the real `.pptx` with the pptx skill; the Forge re-renders
when it changes. Hand edits on `.pptx` are not planned: "open in
PowerPoint" covers them.

## Editing by hand

Small changes the human makes without asking Claude:

- **Text** in markdown, HTML and decks: double click makes the block
  editable in place. On blur the Forge maps it back to the source
  (markdown by the block's line range; HTML by the element's offsets in
  the source, which the Forge records when it serves the file) and
  writes the file. If the mapping is not exact (the text came from
  script, say), the edit becomes a pin instead: "change this text to
  ...", and Claude does it.
- **Source view**: a toggle shows the file as text, editable, beside or
  instead of the render. Horadric's own text editing from the Folio's
  composer, not a JavaScript editor.
- **Images**: draw a box or an arrow on it to pin; crop and rotate by
  hand, written as a new version.
- **Slides**: reorder by dragging in the slide strip, duplicate,
  delete, all written back to the deck's source.

## Versions

Every write to a file shown in the Forge, by anyone, keeps the bytes it
replaced, in `%APPDATA%\Horadric\forge\<project>\` by path and time,
pruned to a size limit. The tab's history control steps back and forth
through them, shows any version rendered, compares two (text diff for
text, side by side for the rest) and restores one. A restore is just
another write. Turn checkpoints (`refs/horadric/turns/`) stay the undo
for a whole turn; this is the finer one, per file and per hand edit.

## Getting it out

- **Present**: full screen on the chosen monitor, slides advanced by
  keys or clicker, presenter notes from `<aside class="notes">` on the
  other screen.
- **PDF**: WebView2's `PrintToPdf`, one page per slide for decks.
- **PNG**: one per slide, or the visible part, at 2x.
- **Copy as image**: the slide or pinned box to the clipboard, for
  Teams and PowerPoint.
- **To `.pptx`**: asks the session to convert with the pptx skill, so
  the result is a real deck, not pictures.
- **Open with**: the program Windows has for the file, and show in
  Explorer.

## For Claude: the tools

The Horadric MCP server gains a few, beside the `browser_*` ones:

- `forge_open(path, at?)`: show a file, optionally at a slide, page or
  source line.
- `forge_pins(path?)`: the open pins, for when a session wants them
  again.
- `forge_render(path, slide?)`: the rendered file as an image, so Claude
  checks its work without opening a tab the human is looking at.
- `forge_style(project)`: the house style's paths and a short note on
  how decks are made here.

And one line in the session's appended system prompt: files it writes
for the human to look at open in the Forge; prefer HTML decks and single
file artefacts; check the render with `forge_render` before saying it is
done.

## Steps

Each is usable on its own, in this order.

1. **See it (the small first step).** A file tab in the browser pane
   for HTML, markdown, images, SVG and PDF. Opened from the files tile,
   from a link in the Folio, and by itself when a session writes one.
   Reloads on change keeping the scroll position. Done when Claude
   writing `deck.html` in a Folio session opens it beside the
   conversation and every save shows within a quarter second.
2. **Point at it.** Pins on HTML, markdown, images and PDF: click or
   drag, type, sent to the session the tab belongs to (or chosen, if the
   project has several) with location text and crop. Several pins per
   message gather in the composer as chips. Runes on the file until
   Claude's next write. Terminal sessions get the text pasted and the
   crop as a file path.
3. **Decks.** The deck shell: scaling, slide strip, keys, present full
   screen with notes, PDF and PNG export, copy as image, the house style
   folder and `forge_style`.
4. **Artefacts.** The bundled runtime for `.jsx` and `.tsx`, Mermaid,
   CSV tables, and the error shown in the tab when one breaks, with a
   button that sends the error to the session.
5. **Versions.** Kept on every write, the history control, compare and
   restore.
6. **Hand edits.** Text in place with source mapping, the source view,
   slide reordering, image crop and rotate.
7. **Office files.** `.pptx` and `.docx` shown by conversion,
   `To .pptx` export through the session.
8. **The tools.** `forge_open`, `forge_pins`, `forge_render`, the system
   prompt line.

Steps 1 to 3 are what makes it worth using daily for decks. The rest
make it whole.

## Open questions

- **Which session gets a pin** when the project has several and the
  tab was opened by hand. Proposed: the one that last wrote the file,
  else the one last focused, with a picker on the pin.
- **Shared with others?** Colleagues looking at a deck through the Town
  Portal or a link is a different feature, not planned here.
- **macOS.** WKWebView has the pieces (custom scheme handler for the
  virtual host, `createPDF`, snapshots). Goes in [macos.md](macos.md)
  once Windows is built.
- **Size of the artefact runtime.** Expected a few megabytes in the
  binary. If too big, load it from a folder beside the binary on first
  use instead.
