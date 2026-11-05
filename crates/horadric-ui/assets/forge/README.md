# The Forge, page side

What runs inside a file shown in the browser pane (WebView2, origin
`https://*.forge.horadric/`). Plain JS and CSS, no build step, no
dependencies, nothing fetched from the network. The plan is
[docs/plan/the-forge.md](../../../../docs/plan/the-forge.md).

| File | Injected into | Does |
|---|---|---|
| `forge.js`, `forge.css` | every Forge page | scroll keeping, pins, pin runes, goto, errors |
| `deck.js`, `deck.css` | HTML decks only (`<section class="slide">`), after `forge.js` | the deck shell: scaling, strip, keys, present, print |
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
the deck controls' own shadow root.

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

## The deck shell

- **Views**: `single` (one slide, fitted and centred, letterboxed) and
  `scroll` (every slide in a column, fitted to the width). The strip of
  thumbnails sits on the left; it starts hidden when the pane is under 720
  px wide.
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
  `setMode("single" | "scroll")`, `setStrip(on)`, `rebuildStrip()`, and
  `current`, `count`, `mode`, `size`.

## Trying it

Open the pages in `test/` (`markdown.html`, `deck.html`, `image.html`) in
the browser pane by `file://` URL. They load the scripts with relative
`<script>` tags. Press P to point. From the console, or `browser_evaluate`:

```js
__forge.receive({ forge: "pins", pins: [{ n: 1, selector: "#end" }] });
__forge.receive({ forge: "goto", line: 11 });
__forge.sent.at(-1);   // the last message the page posted
```

The pin input lives in a closed shadow root, so a test types into it with
DevTools `Input.insertText` after a real click, not with a selector.
