# The Forge, page side

What runs inside a file shown in the browser pane (WebView2, origin
`https://*.forge.horadric/`). Plain JS and CSS, no build step, no
dependencies, nothing fetched from the network. The plan is
[docs/plan/the-forge.md](../../../../docs/plan/the-forge.md).

| File | Injected into | Does |
|---|---|---|
| `forge.js`, `forge.css` | every Forge page | scroll keeping, pins, pin runes, goto, errors, text edits in place, image crop |
| `deck.js`, `deck.css` | HTML decks only (`<section class="slide">`), after `forge.js` | the deck shell: toolbar, scaling, resizable strip, speaker notes, keys, present, print, slides added, moved, duplicated and deleted |
| `test/` | nothing | pages to try it all by hand, see the end |

## Injecting

Run `forge.js` first, then `deck.js` for decks, with
`AddScriptToExecuteOnDocumentCreated` (both wait for the DOM themselves)
or as `<script>` tags. The CSS is found one of two ways:

- **Given**: set `window.__forgeAssets = { forgeCss: "<text of forge.css>",
  deckCss: "<text of deck.css>" }` before the scripts run. Use this when
  injecting, since an injected script has no URL to resolve from.
- **Beside the script**: with no `__forgeAssets`, each script loads its CSS
  from the same folder as its own `src` (how the test pages work).

Both scripts are idempotent: a second injection does nothing.
`forge.css` goes into a closed shadow root and never touches the page.
`deck.css` goes into the page (all rules under `html.forge-deck`) and into
the deck controls' own shadow root (all rules starting at `:host`, so they
match nothing in the page).

Everything the page side adds: the globals `window.__forge` and
`window.__forgeDeck`, the elements `<forge-overlay>` and `<forge-deck-ui>`
under `<html>`, a `div.forge-strip` at the end of `<body>` (decks), and
`forge-*` classes and `--forge-*` custom properties on `<html>` and the
slides (decks).

## Transport

Page to host: `window.chrome.webview.postMessage(obj)`, so the host reads
`WebMessageAsJson`. Host to page: `PostWebMessageAsJson(json)` (a JSON
string through `PostWebMessageAsString` is parsed too). Every message is an
object with a `forge` field naming it; anything else is ignored.

Outside WebView2 (a plain browser) posts go to `console.debug` instead.
Either way the last 200 posts are kept in `__forge.sent`, and
`__forge.receive(obj)` delivers a host message by hand, for testing.

Coordinates: `rect` fields are CSS pixels. "Viewport" means relative to the
visible top left; "document" means relative to the page's top left
(viewport plus scroll).

## Page to host

### `ready`
Sent once on `load`.
```json
{ "forge": "ready", "file": "/decks/q3.html", "title": "Q3", "slides": 12, "x": 0, "y": 0 }
```
`file` is `location.pathname`. `slides` counts `section.slide` (0 when not a
deck). `x`, `y` are the scroll position at load. The host answers with
`restore-scroll` when it is reloading a file it showed before.

### `scroll`
```json
{ "forge": "scroll", "x": 0, "y": 1840 }
```
At most every 150 ms while scrolling, with a trailing one so the last
position always arrives. Also sent when `restore-scroll` moves the page.
The host keeps the last one per tab and file.

### `pin`
The human pointed at something and said what to change.
```json
{
  "forge": "pin",
  "file": "/decks/q3.html",
  "words": "Make this box orange",
  "kind": "element",
  "at": {
    "slide": 3,
    "page": 2,
    "lines": [9, 12],
    "selector": "#two-columns > div > div:nth-of-type(2) > p",
    "text": "Right box",
    "html": "<p>Right box</p>",
    "image": { "x": 199, "y": 199, "w": 400, "h": 300,
               "naturalWidth": 1200, "naturalHeight": 800, "src": "https://..." }
  },
  "rect": { "x": 825, "y": 670, "w": 401, "h": 38 },
  "dpr": 1.5,
  "anchor": { "selector": "...", "rect": { "x": 825, "y": 670, "w": 401, "h": 38 },
              "slide": 3, "slideRect": { "x": 0.1, "y": 0.15, "w": 0.4, "h": 0.2 } }
}
```
- `kind`: `"element"` (a click picked an element) or `"box"` (a drag drew a
  box; `at` then describes the element under the box's centre).
- `at` holds only what applies:
  - `slide`: 1-based index of the enclosing `section.slide`.
  - `page`: from the nearest `[data-page]` or `[data-page-number]`
    ancestor (PDF.js style pages). WebView2's own PDF viewer runs no
    injected script, so for a PDF shown that way the host knows the page,
    not the page side.
  - `lines`: `[first, last]` source lines from the nearest `[data-lines]`
    ancestor. The markdown renderer writes `data-lines="12-18"` (or `"12"`
    for one line) on each block it renders.
  - `selector`: id if unique, else an `nth-of-type` path up to an element
    with a unique id or `body`.
  - `text`: the element's text, whitespace folded, at most 300 chars.
  - `html`: its outer HTML, at most 600 chars. Element pins only.
  - `image`: when the element is an `<img>`, the pinned part in the image's
    own pixels (`object-fit: contain` and `scale-down` allowed for). A click
    on an image gives the whole image.
- `rect`: the pinned element or box in **viewport** CSS px at the moment
  of sending. Crop the screenshot at `rect * dpr`. The pin input is drawn
  beside, not over, the spot, but take the crop before anything else moves.
- `dpr`: `devicePixelRatio`.
- `anchor`: where to draw the pin's rune later. Store it and send it back
  in `pins` unchanged. `rect` here is in **document** px; `slideRect` (box
  pins on a slide) is in fractions of the slide, so it survives the deck
  being scaled.

### `pinning`
```json
{ "forge": "pinning", "on": false }
```
Pin mode changed on the page side (the P key, or Esc in pin mode). Not sent
when the host's own `pinning` message changed it, so there is no loop.

### `error`
```json
{ "forge": "error", "message": "Uncaught TypeError: ...", "stack": "TypeError: ...\n    at ..." }
```
From `window` `error` (script errors, and resources that failed to load:
"Could not load img https://..."), and `unhandledrejection`. Each distinct
error once per page load. The page also shows a small card with it.

### `send-error`
```json
{ "forge": "send-error", "file": "/app.html", "message": "...", "stack": "..." }
```
The human pressed "Send to session" on an error card. Send it to the
session as a request to fix.

### `slide` (decks)
```json
{ "forge": "slide", "n": 4, "of": 12, "notes": "Talk about pointing." }
```
On load and on every change of the current slide, by any means. `notes` is
the text of the slide's `<aside class="notes">`, empty when it has none.
This drives the presenter view on the second screen.

### `deck` (decks)
```json
{ "forge": "deck", "slides": 12, "w": 1920, "h": 1080, "mode": "single", "strip": true }
```
On load and whenever the view or strip changes. `w`, `h` is the design
size: `--slide-width` and `--slide-height` on `:root`, or `data-width` and
`data-height` on `<html>`, `<body>` or the first slide, or the size the
deck's own CSS gives a slide, else 1920 by 1080.

### `present` (decks)
```json
{ "forge": "present", "on": true, "slide": 1, "of": 12, "notes": "...", "fullscreen": true }
{ "forge": "present", "on": false, "slide": 7, "of": 12 }
```
Present mode started or ended. `fullscreen` says whether the page's
Fullscreen API request worked; without a user gesture (the host asked) it
does not, and the host should make the window full screen itself. The host
should handle WebView2's `ContainsFullScreenElementChanged` to make the
pane's window full screen on the chosen monitor.

### `edit`
The human changed text in place: a double click made a block editable
(`contenteditable="plaintext-only"`), Enter or leaving it ends the edit,
Esc puts it back. Not in pin mode, not while presenting.
```json
{ "forge": "edit", "id": 3, "file": "/notes.md", "lines": [9, 12],
  "old": "Revenue up 12 percent", "new": "Revenue up 14 percent" }
{ "forge": "edit", "id": 4, "file": "/deck.html", "offsets": [812, 830],
  "old": "Left box", "new": "Left side" }
```
- What is edited: a markdown block as a whole (the top-level `[data-lines]`
  element; Mermaid, tables, rules and pictures are not), else the nearest
  box that holds only text and inline elements. A whole slide is not.
- `old`, `new`: the element's text before and after, from its text nodes
  with `script`, `style`, `template` and `noscript` left out, unfolded.
- `lines`: markdown only, the block's `data-lines`.
- `offsets`: HTML and decks, from the element's `data-forge-at`, which the
  host writes into the served copy of every HTML page (never the file):
  the byte range of the element's content in the file. Left out when the
  element has none (made by script, or a tag the file leaves open).

The host answers with `edit-result`. When the text cannot be put back
exactly, it answers `pin: true` and the page puts the element back as the
file has it and sends a `pin` instead, with the words `Change this text
to: "..."`.

### `reorder` (decks)
The slide strip changed the slides: dragged to a new place, or a thumb's
duplicate or delete button (delete asks once more).
```json
{ "forge": "reorder", "id": "deck-1", "file": "/deck.html", "count": 5, "order": [2, 1, 3, 4, 5] }
{ "forge": "reorder", "id": "deck-2", "file": "/deck.html", "count": 5, "order": [1, 2, 3, 3, 4, 5], "blank": true }
```
`order` is the new list of the current slides by number. A permutation
moves; one number left out deletes that slide; one number twice in a row
duplicates it (the copy goes right after it). `count` is how many slides
the page had, so a page older than the file is refused. `blank` (Add
slide) makes the copy a new empty slide of the same layout: each box of
words says only "Title", "Point" or "Text", and its notes are emptied
(`add_after` in `edit/html.rs`).

### `crop`
On a PNG, JPEG or WebP shown alone: the Crop button or C, a drag draws the
part to keep, the turn buttons turn it by 90°, Enter or Save sends.
```json
{ "forge": "crop", "id": 5, "file": "/shots/a.png", "box": { "x": 10, "y": 20, "w": 400, "h": 300 },
  "rotate": 90, "data": "data:image/png;base64,..." }
```
`box` is in the image's own pixels, `rotate` clockwise degrees, applied
after the crop. The page encodes the result itself in the file's format
(`canvas.toDataURL`, JPEG and WebP at 0.92); the host checks that the bytes
are that format and writes them.

## Host to page

### `restore-scroll`
```json
{ "forge": "restore-scroll", "x": 0, "y": 1840 }
```
Waits for `load`, fonts and images (at most 1.5 s more), then scrolls, and
keeps putting it back for about a second while late layout moves it,
unless the human scrolls first.

### `pinning`
```json
{ "forge": "pinning", "on": true }
```
Turn pin mode on or off (the bar's pin button). In pin mode one layer
covers the page: hovering outlines the element under the pointer, a click
pins it, a drag of more than 6 px draws a box. An input then opens beside
the spot; Enter sends a `pin`, Esc cancels. Pin mode stays on after a pin.
The P key toggles it too (not while typing in an input, textarea, select
or editable element).

### `pins`
```json
{ "forge": "pins", "pins": [ { "n": 1, "selector": "...", "slide": 3 },
                             { "n": 2, "slide": 3, "slideRect": { "x": 0.1, "y": 0.15, "w": 0.4, "h": 0.2 } },
                             { "n": 3, "rect": { "x": 352, "y": 1571, "w": 200, "h": 150 } } ] }
```
Draw these pins as small numbered runes, replacing any drawn before. Each is
`n` plus a pin's `anchor` fields. The anchor is tried in order: `selector`
(follows the element anywhere, hidden when it is not shown, such as on
another slide), then `slide` with `slideRect` (or the whole slide), then
`rect` in document px. Runes follow scroll, resize and slide changes.

### `clear-pins`
```json
{ "forge": "clear-pins" }
```
The runes fade out. Sent when the session's next write to the file lands.

### `goto`
```json
{ "forge": "goto", "slide": 4 }
{ "forge": "goto", "line": 57 }
{ "forge": "goto", "page": 2 }
```
`slide`: in a deck, shows that slide; elsewhere scrolls to it. `line`:
scrolls to the smallest `[data-lines]` block holding that line (or the
next block, when the line is between blocks) and outlines it briefly.
`page`: scrolls to `[data-page=n]`, `[data-page-number=n]`, `#page-n` or
`#pagen`.

### `present` (decks)
```json
{ "forge": "present", "on": true }
```
Start or end present mode: one slide, no strip or controls, black
letterbox, a click or the keys advance.

### `deck-view` (decks)
```json
{ "forge": "deck-view", "mode": "scroll", "strip": false }
```
Either field may be left out. Answered with `deck`.

### `edit-result`
```json
{ "forge": "edit-result", "what": "edit", "id": 3, "ok": true }
{ "forge": "edit-result", "what": "edit", "id": 4, "ok": false, "pin": true }
{ "forge": "edit-result", "what": "reorder", "id": "deck-1", "ok": false, "why": "..." }
```
The answer to `edit`, `reorder` or `crop`, by its `id`. On `ok` the file was
written (or was already so) and the page loads again as the file changes.
`pin`: see `edit`. `why`: shown in a small note in the corner; the page
shows the file as it is.

## The deck shell

- **Toolbar** along the top: the strip on or off, previous and next with
  the count, One or All (the view), a `?` that shows the first-run hint
  again, Add slide (a new empty slide in the current one's layout, after
  it) and Present. Labels go when the stage is under 640 px wide. Runes
  kindle in it while a slide edit is being written.
- **Views**: `single` (one slide fitted to the stage with a 20 px margin;
  when the pane is taller than the slide needs, the slide goes to the top
  and its speaker notes show under it) and `scroll` (every slide in a
  column, fitted to the width).
- **Strip** of thumbnails on the left, a fifth of the pane wide, starting
  hidden when the pane is under 720 px. Drag its edge to resize (kept per
  deck in `localStorage`, double click the edge to forget it). Drag a
  thumbnail to move a slide; hover shows duplicate and delete.
- **Kept across reloads** (the page reloads on every save) in
  `sessionStorage`: the current slide and the view, and after a slide
  edit the slide it should land on (the new one, the moved one). The
  first-run hint (double click to edit, P to point, drag to reorder) is
  remembered once dismissed.
- **Keys**: Right, Down, PageDown, Space next; Left, Up, PageUp, Shift+Space
  back; Home, End; S strip; V view; F (or F5) present; Esc ends present. In
  the scroll view Up and Down scroll as usual. The shell takes these keys
  before the deck's own script does, so a deck with its own navigation does
  not move twice.
- **Slides are styled, never moved**: the shell sets their size
  (`border-box`, the design size), position and scale with `!important`
  rules, so the deck's own selectors keep matching. Slides whose background
  is transparent get the body's. `aside.notes` is hidden.
- **Print**: `@page` is 13.333 by 7.5 inches (PowerPoint widescreen) with
  no margin, each slide one page at `zoom: 1280 / design width`. For
  `PrintToPdf` set `PageWidth` 13.333, `PageHeight` 7.5, all margins 0,
  `ShouldPrintBackgrounds` true, `ShouldPrintHeaderAndFooter` false.
- `window.__forgeDeck`: `goto(n)`, `next()`, `prev()`, `present(on)`,
  `setMode("single" | "scroll")`, `setStrip(on)`, `rebuildStrip()`,
  `moveSlide(n, before)`, `duplicateSlide(n)`, `deleteSlide(n)`,
  `addSlide(after)` (each sends a `reorder`), and `current`, `count`, `mode`, `size`.
- `window.__forge` also has `note(text, bad)`, `edit(el)` (starts an edit
  where a double click on `el` would) and `crop()`.

## Writing hand edits (host side)

`src/forge/edit.rs`. The source mapping is in `edit/text.rs` (the text a
reader sees, each character tied to its source bytes; markdown markup and
HTML tags are skipped, so formatting outside the changed words stays) and
`edit/html.rs` (element offsets, the served copy's marks, slides). A file
is written in one go on a thread of its own, never while a session's
`Edit` or `Write` to it is between `PreToolUse` and `PostToolUse` (waited
out, at most 20 s). A page can only change its own file.

## Trying it

Open the pages in `test/` (`markdown.html`, `deck.html`, `image.html`) in
the browser pane by `file://` URL. `deck.html` writes Horadric's `--hd-`
colours itself, since nothing injects them there, and has a `.bar` of its
own that must keep its look. They load the scripts with relative
`<script>` tags. Press P to point. From the console, or `browser_evaluate`:

```js
__forge.receive({ forge: "pins", pins: [{ n: 1, selector: "#end" }] });
__forge.receive({ forge: "goto", line: 11 });
__forge.sent.at(-1);   // the last message the page posted
```

The pin input lives in a closed shadow root, so a test types into it with
DevTools `Input.insertText` after a real click, not with a selector.
