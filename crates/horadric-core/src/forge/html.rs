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
/// optional script. Light and dark follow the system, since the pane sits
/// beside a terminal in whichever the human chose.
pub(super) fn page(title: &str, class: &str, body: &str, script: &str) -> String {
    let mut out = String::with_capacity(body.len() + STYLE.len() + 512);
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("<title>");
    esc_into(title, &mut out);
    out.push_str("</title>\n<style>");
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

const STYLE: &str = r#"
:root { color-scheme: light dark; --fg: #1f2328; --muted: #59636e; --bg: #ffffff;
  --line: #d1d9e0; --soft: #f6f8fa; --link: #0969da; --mark: #fff8c5; }
@media (prefers-color-scheme: dark) {
  :root { --fg: #e6e1d8; --muted: #9a9389; --bg: #16130f; --line: #3a342c;
    --soft: #201c17; --link: #d9a75f; --mark: #4a3b1c; }
}
* { box-sizing: border-box; }
html { background: var(--bg); color: var(--fg); }
body { margin: 0; font: 16px/1.6 "Segoe UI", system-ui, -apple-system, sans-serif; }
main { max-width: 48rem; margin: 0 auto; padding: 2rem 1.5rem 4rem; }
main.forge-csv { max-width: none; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.6em 0 0.6em; font-weight: 600; }
h1 { font-size: 2em; padding-bottom: 0.3em; border-bottom: 1px solid var(--line); }
h2 { font-size: 1.5em; padding-bottom: 0.3em; border-bottom: 1px solid var(--line); }
h3 { font-size: 1.25em; } h4 { font-size: 1em; } h5, h6 { font-size: 0.875em; color: var(--muted); }
main > :first-child { margin-top: 0; }
p, ul, ol, blockquote, pre, table { margin: 0 0 1em; }
a { color: var(--link); text-decoration: none; } a:hover { text-decoration: underline; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid var(--line); margin: 2em 0; }
blockquote { padding: 0 1em; color: var(--muted); border-left: 0.25em solid var(--line); }
ul, ol { padding-left: 2em; } li + li { margin-top: 0.25em; }
li.task { list-style: none; } li.task > input { margin: 0 0.5em 0 -1.4em; vertical-align: middle; }
code { font: 0.875em/1.5 "Cascadia Code", Consolas, ui-monospace, monospace;
  background: var(--soft); padding: 0.15em 0.35em; border-radius: 4px; }
pre { background: var(--soft); padding: 1em; border-radius: 6px; overflow: auto; }
pre code { background: none; padding: 0; font-size: 0.85em; }
pre.mermaid { background: none; text-align: center; }
table { border-collapse: collapse; display: block; overflow: auto; }
th, td { border: 1px solid var(--line); padding: 0.4em 0.8em; }
th { background: var(--soft); font-weight: 600; }
tr:nth-child(2n) td { background: var(--soft); }
main.forge-csv table { display: table; font-size: 0.9em; }
main.forge-csv th { cursor: pointer; user-select: none; position: sticky; top: 0; }
main.forge-csv th[data-sort="asc"]::after { content: " \25B4"; }
main.forge-csv th[data-sort="desc"]::after { content: " \25BE"; }
main.forge-csv td.num { text-align: right; font-variant-numeric: tabular-nums; }
mark { background: var(--mark); color: inherit; }
"#;
