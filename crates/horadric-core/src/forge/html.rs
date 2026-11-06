//! What the Forge's own pages share: escaping and the document around them.

/// Text as HTML content.
pub(super) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    esc_into(s, &mut out);
    out
}

pub(super) fn esc_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// A whole page: the human's content in `body`, our stylesheet and an
/// optional script. Drawn in Horadric's look, whose colours the app sets
/// after this stylesheet as `--hd-` properties; the core cannot know the
/// theme, so [`LOOK`] carries the default theme's as a fallback.
pub(super) fn page(title: &str, class: &str, body: &str, script: &str) -> String {
    let mut out = String::with_capacity(body.len() + LOOK.len() + STYLE.len() + 512);
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("<title>");
    esc_into(title, &mut out);
    out.push_str("</title>\n<style>");
    out.push_str(LOOK);
    out.push_str(STYLE);
    out.push_str("</style></head>\n<body><main class=\"");
    out.push_str(class);
    out.push_str("\">\n");
    out.push_str(body);
    out.push_str("</main>");
    if !script.is_empty() {
        out.push_str("\n<script>");
        out.push_str(script);
        out.push_str("</script>");
    }
    out.push_str("\n</body></html>\n");
    out
}

/// Horadric's skeuomorph, shared by every page the Forge makes: a matte
/// faceplate with the seam every Horadric window has inside its edge, a
/// screen sunk into it, engraved legends. The colours are the
/// Skeuomorphism theme's with the first project colour, so a page opened
/// anywhere but the app still reads as Horadric; the app sets the theme
/// in use and the project's colour after this.
pub const LOOK: &str = r#"
:root { color-scheme: dark;
  --hd-plate: #151619; --hd-plate-low: #0e0f11; --hd-key-top: #2b2d31; --hd-key-low: #181a1e;
  --hd-key-side: #0b0c0d; --hd-surface: #1b1d21; --hd-well: #090a0c; --hd-screen: #060709;
  --hd-lamp-off: #25272c; --hd-text: #e8e9ed; --hd-dim: #8f939e; --hd-legend: #6e727c;
  --hd-read: #d4d5d9; --hd-cast: rgba(0, 0, 0, 0.6); --hd-bevel-light: rgba(255, 255, 255, 0.11);
  --hd-bevel-shade: rgba(0, 0, 0, 0.35); --hd-hollow-shade: rgba(0, 0, 0, 0.55);
  --hd-hollow-light: rgba(255, 255, 255, 0.05); --hd-engrave-dark: rgba(0, 0, 0, 0.5);
  --hd-engrave-light: rgba(255, 255, 255, 0.05); --hd-hover: rgba(255, 255, 255, 0.06);
  --hd-press: rgba(255, 255, 255, 0.025); --hd-accent: #a78bfa;
  --hd-accent-dim: rgba(167, 139, 250, 0.5); --hd-accent-faint: rgba(167, 139, 250, 0.12);
  --hd-rune: #c0b4e8; --hd-waiting: #ffb224; --hd-error: #ff5d66;
  --hd-error-dim: rgba(255, 93, 102, 0.4); --hd-done: #3dd68c;
  /* The Folio's ink for code: a parchment shade taken toward the text. */
  --hd-code: #dec5a0;
  --hd-ui: "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  --hd-mono: "Cascadia Mono", "Cascadia Code", Consolas, monospace; }
* { box-sizing: border-box; }
html { background: var(--hd-plate); color: var(--hd-read); }
body { margin: 0; min-height: 100vh; font: 16px/1.65 var(--hd-ui); }
/* The seam: a dark cut with the light catching the lip under it. */
body::before { content: ""; position: fixed; inset: 5px; border-radius: 9px; z-index: 2;
  pointer-events: none; border: 1px solid var(--hd-engrave-dark);
  box-shadow: 0 1px 0 var(--hd-engrave-light), inset 0 1px 0 var(--hd-engrave-light); }
/* A screen sunk into the plate: shade under its top edge, the plate's lit
   lip along its bottom. */
.hd-screen, main { background: var(--hd-screen); border-radius: 10px;
  box-shadow: inset 0 2px 7px var(--hd-hollow-shade), inset 0 -1px 0 var(--hd-hollow-light),
    0 1px 0 var(--hd-engrave-light); }
/* Engraved: the plate's shade above the letters, its lit lip below. */
.hd-legend { font: 600 11px/1.2 var(--hd-ui); letter-spacing: 0.14em; text-transform: uppercase;
  color: var(--hd-legend);
  text-shadow: 0 -1px 0 var(--hd-engrave-dark), 0 1px 0 var(--hd-engrave-light); }
:focus-visible { outline: 1px solid var(--hd-accent); outline-offset: 2px; border-radius: 4px; }
::selection { background: var(--hd-accent-dim); color: var(--hd-text); }
@media print {
  html:root { background: #fff; color: #1d1e21; }
  body::before { display: none; }
}
"#;

/// The markdown page reads as the Folio's transcript does, on a screen of
/// the Folio's measure; a CSV is its table on a screen the window's size,
/// scrolled inside so its header stays in view.
const STYLE: &str = r#"
main { width: min(780px, calc(100% - 36px)); margin: 26px auto 34px; padding: 40px 48px 46px; }
main > :first-child { margin-top: 0; }
main > :last-child { margin-bottom: 0; }
@media (max-width: 600px) { main { padding: 26px 20px 30px; } }

h1, h2, h3, h4, h5, h6 { color: var(--hd-text); font-weight: 600; margin: 1.7em 0 0.5em; }
h1 { font-size: 26px; line-height: 1.3; padding-bottom: 0.4em;
  border-bottom: 1px solid color-mix(in srgb, var(--hd-read) 8%, transparent); }
h2 { font-size: 21px; line-height: 1.35; }
h3 { font-size: 18px; line-height: 1.45; }
/* The Folio's small heading: capitals, spaced, dimmed. */
h4, h5, h6 { font-size: 13px; line-height: 1.6; letter-spacing: 0.07em; text-transform: uppercase;
  color: color-mix(in srgb, var(--hd-read) 62%, transparent); }
p, ul, ol, blockquote, pre, table, details { margin: 0 0 14px; }
strong { color: var(--hd-text); font-weight: 600; }
del { color: color-mix(in srgb, var(--hd-read) 55%, transparent); }
a { color: color-mix(in srgb, var(--hd-accent) 55%, var(--hd-read)); text-decoration: underline;
  text-decoration-color: color-mix(in srgb, var(--hd-accent) 45%, transparent);
  text-underline-offset: 3px; text-decoration-thickness: 1px; }
a:hover { color: var(--hd-text); text-decoration-color: var(--hd-accent); }
img { max-width: 100%; border-radius: 4px;
  border: 1px solid color-mix(in srgb, var(--hd-read) 10%, transparent); }
hr { border: 0; height: 1px; margin: 26px 0;
  background: color-mix(in srgb, var(--hd-read) 10%, transparent); }
mark { background: var(--hd-accent-faint); color: var(--hd-text); border-radius: 3px; padding: 0 2px; }

/* Lists: bullets in the project's colour, numbers in it toward the ink. */
ul, ol { padding-left: 26px; }
li + li, li > ul, li > ol { margin-top: 6px; }
li > ul, li > ol { margin-bottom: 0; }
ul > li::marker { color: color-mix(in srgb, var(--hd-accent) 70%, transparent); }
ol > li::marker { color: color-mix(in srgb, var(--hd-accent) 65%, var(--hd-read));
  font-variant-numeric: tabular-nums; }
li.task { list-style: none; }
li.task:has(> input:checked) { color: color-mix(in srgb, var(--hd-read) 50%, transparent); }
li.task > input { appearance: none; margin: 0 9px 0 -24px; width: 15px; height: 15px;
  vertical-align: -2px; border-radius: 4px; position: relative;
  background: color-mix(in srgb, var(--hd-read) 3%, transparent);
  border: 1.25px solid color-mix(in srgb, var(--hd-read) 35%, transparent); }
li.task > input:checked { border-color: transparent;
  background: color-mix(in srgb, var(--hd-accent) 75%, var(--hd-read)); }
li.task > input:checked::after { content: ""; position: absolute; left: 4px; top: 1px;
  width: 3.5px; height: 7.5px; border: solid var(--hd-screen); border-width: 0 2px 2px 0;
  transform: rotate(45deg); }

/* A quote: a bar in the project's colour, its words a little quieter. */
blockquote { padding: 0 0 0 18px; position: relative;
  color: color-mix(in srgb, var(--hd-read) 74%, transparent); }
blockquote::before { content: ""; position: absolute; left: 2px; top: 2px; bottom: 2px; width: 3px;
  border-radius: 1.5px; background: color-mix(in srgb, var(--hd-accent) 40%, transparent); }
blockquote > :last-child { margin-bottom: 0; }

/* Code in a line: a faint plate and the Folio's ink for code. */
code { font: 0.84em/1.5 var(--hd-mono); color: var(--hd-code); padding: 0.12em 0.32em;
  border-radius: 4px; background: color-mix(in srgb, var(--hd-read) 7.5%, transparent); }
/* A block of code: a darker well with a thin edge, as in the Folio. */
pre { font: 13px/19px var(--hd-mono); padding: 12px 14px; overflow: auto; border-radius: 8px;
  background: rgba(0, 0, 0, 0.32); tab-size: 4;
  border: 1px solid color-mix(in srgb, var(--hd-read) 7%, transparent); }
pre code { font: inherit; color: color-mix(in srgb, var(--hd-read) 90%, transparent);
  background: none; padding: 0; border-radius: 0; }
pre.mermaid { font-family: var(--hd-ui); background: none; border: 0; padding: 4px 0;
  text-align: center; color: var(--hd-dim); }
pre.mermaid svg { max-width: 100%; height: auto; }
/* Mermaid casts a pale shadow round every node, which on the dark glass
   reads as a glow. */
pre.mermaid svg * { filter: none !important; }
/* A diagram that would not draw: the runtime's card, its inline light
   colours overridden with a lamp's worth of the error colour. */
pre.mermaid [data-forge-error] { text-align: left; padding: 12px 16px !important;
  border: 0 !important; border-radius: 8px !important; background: rgba(0, 0, 0, 0.32) !important;
  color: var(--hd-read) !important; font: 12.5px/1.55 var(--hd-mono) !important;
  box-shadow: inset 3px 0 0 var(--hd-error) !important; }

details { padding: 0 0 0 22px; }
details > summary { margin-left: -22px; cursor: pointer; font-weight: 600; color: var(--hd-text);
  list-style: none; }
details > summary::-webkit-details-marker { display: none; }
details > summary::before { content: ""; display: inline-block; width: 6px; height: 6px;
  margin: 0 12px 2px 3px; border: solid var(--hd-dim); border-width: 0 1.5px 1.5px 0;
  transform: rotate(-45deg); transition: transform 90ms ease; }
details[open] > summary::before { transform: rotate(45deg); margin-bottom: 4px; }
details[open] > summary { margin-bottom: 8px; }

/* Tables as the Folio draws them: no box and no stripes, the header a
   small spaced label over a firmer rule, thin rules between rows. */
table { border-collapse: collapse; display: block; overflow: auto; max-width: 100%;
  font-size: 15px; line-height: 1.5; }
th, td { padding: 7px 12px; text-align: left; vertical-align: top; }
th { font-size: 12.5px; font-weight: 600; letter-spacing: 0.03em; color: var(--hd-dim);
  border-bottom: 1px solid color-mix(in srgb, var(--hd-read) 16%, transparent); }
td { color: color-mix(in srgb, var(--hd-read) 92%, transparent);
  border-bottom: 1px solid color-mix(in srgb, var(--hd-read) 7%, transparent); }
tbody tr:last-child td { border-bottom: 0; }
tbody tr:hover td { background: var(--hd-hover); }

/* The CSV: the screen fills the window and scrolls inside, so the header
   row, engraved on a strip of the plate, stays in view. */
main.forge-csv { position: fixed; inset: 18px; width: auto; max-width: none; margin: 0;
  padding: 0; overflow: auto; }
main.forge-csv table { display: table; overflow: visible; min-width: 100%; font-size: 14px; }
main.forge-csv th { position: sticky; top: 0; z-index: 1; cursor: pointer; user-select: none;
  white-space: nowrap; padding: 11px 14px 10px; background: var(--hd-plate);
  font: 600 11px/1.2 var(--hd-ui); letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--hd-legend); border-bottom: 0;
  text-shadow: 0 -1px 0 var(--hd-engrave-dark), 0 1px 0 var(--hd-engrave-light);
  box-shadow: inset 0 -1px 0 var(--hd-engrave-dark), 0 1px 0 var(--hd-engrave-light); }
main.forge-csv th:hover { color: var(--hd-dim); }
main.forge-csv th[data-sort] { color: var(--hd-text); }
/* The column sorted by: a small caret in the project's colour. */
main.forge-csv th[data-sort]::after { content: ""; display: inline-block; margin-left: 8px;
  border: 4px solid transparent; vertical-align: 1px; }
main.forge-csv th[data-sort="asc"]::after { border-bottom: 5px solid var(--hd-accent); border-top-width: 0; }
main.forge-csv th[data-sort="desc"]::after { border-top: 5px solid var(--hd-accent); border-bottom-width: 0; }
main.forge-csv td { padding: 7px 14px; white-space: pre-wrap; }
main.forge-csv th:first-child, main.forge-csv td:first-child { padding-left: 32px; }
main.forge-csv td:first-child { position: relative; color: var(--hd-text); }
/* An unlit lamp at the head of each row; the row the pointer is on lights it. */
main.forge-csv td:first-child::before { content: ""; position: absolute; left: 13px; top: 15px;
  width: 6px; height: 6px; border-radius: 50%; background: var(--hd-lamp-off);
  box-shadow: inset 0 1px 1px var(--hd-hollow-shade); }
main.forge-csv tr:hover td:first-child::before { background: var(--hd-accent); box-shadow: none; }
main.forge-csv th.num { text-align: right; }
main.forge-csv td.num { text-align: right; font: 13px/1.65 var(--hd-mono);
  font-variant-numeric: tabular-nums; }
.forge-cut { position: fixed; right: 30px; bottom: 26px; margin: 0; padding: 6px 11px;
  border-radius: 6px; background: var(--hd-surface); color: var(--hd-dim); font: 12px/1.4 var(--hd-ui);
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), 0 3px 10px var(--hd-cast); }

/* On paper: no faceplate and no screen, dark ink on white, still in
   Horadric's type. `html:root` outranks the colours the app sets. */
@media print {
  html:root { --hd-read: #1d1e21; --hd-text: #000; --hd-dim: #5c5f66; --hd-legend: #5c5f66;
    --hd-code: #1d1e21; --hd-hover: transparent; --hd-screen: #fff; }
  body { font-size: 11pt; min-height: 0; }
  .forge-cut { display: none; }
  main, main.forge-csv { position: static; width: auto; max-width: none; height: auto; overflow: visible; margin: 0;
    padding: 0; background: none; box-shadow: none; border-radius: 0; }
  a { color: #1d1e21; text-decoration-color: #9a9da4; }
  ul > li::marker, ol > li::marker { color: #5c5f66; }
  blockquote::before { background: #c4c6cb; }
  pre { background: #f4f4f5; border-color: #e1e2e5; white-space: pre-wrap; break-inside: avoid; }
  code { background: #f1f1f3; }
  pre code { background: none; }
  li.task > input:checked { background: #5c5f66; }
  li.task > input:checked::after { border-color: #fff; }
  h1, h2, h3, h4, h5, h6 { break-after: avoid; }
  tr, img, pre.mermaid { break-inside: avoid; }
  main.forge-csv th { position: static; background: none; text-shadow: none; color: #5c5f66;
    box-shadow: inset 0 -1px 0 #9a9da4; }
  main.forge-csv td:first-child::before { display: none; }
  main.forge-csv td:first-child, main.forge-csv th:first-child { padding-left: 2px; }
  main.forge-csv thead { display: table-header-group; }
}

@media (prefers-reduced-motion: reduce) { details > summary::before { transition: none; } }
"#;
