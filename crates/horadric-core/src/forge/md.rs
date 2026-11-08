//! Markdown to a whole HTML page for the Forge.
//!
//! Not `crate::markdown`: that one is shaped for Direct2D runs and drops
//! anything a chat reply should not open (relative links, images). A file
//! in the Forge is the human's own document shown in the web view, so it
//! keeps them, and every top-level block is tagged with the source lines
//! it came from, which is how a pin or a hand edit finds its way back.
//! No crate either, as with the chat's (decided 2026-10-25): the subset
//! here is what Claude and the human write, and anything else is text.

use std::collections::HashMap;

use super::html::{esc, esc_into, page};

/// How deep quotes and lists may nest before the rest is plain text, so a
/// hostile file cannot run the stack out.
const MAX_DEPTH: usize = 24;

/// A full HTML document for `src`. Every top-level block carries
/// `data-lines="a-b"`, the 1-based inclusive source lines it came from.
/// Fenced `mermaid` blocks become `<pre class="mermaid">` for the runtime to
/// draw. Relative image and link paths are kept as written, since the page
/// is served from the file's folder.
pub fn markdown_html(src: &str, title: &str) -> String {
    page(title, "forge-md", &markdown_body(src), "")
}

/// What [`markdown_html`] puts inside its `<main class="forge-md">`: the
/// blocks alone, for a page that shows a new text without loading again
/// (the editor's preview after a write).
pub fn markdown_body(src: &str) -> String {
    let lines: Vec<String> = src
        .strip_prefix('\u{feff}')
        .unwrap_or(src)
        .split('\n')
        .map(|l| expand_tabs(l.strip_suffix('\r').unwrap_or(l)))
        .collect();
    let lines = match lines.last() {
        Some(l) if l.is_empty() => &lines[..lines.len() - 1],
        _ => &lines[..],
    };
    let cx = Cx {
        refs: references(lines),
    };
    let mut body = String::new();
    let start = front_matter(lines);
    cx.blocks(&lines[start..], Some(start), false, 0, &mut body);
    body
}

/// Leading tabs to four spaces, so indentation is one number. Tabs inside
/// a line are left alone.
fn expand_tabs(line: &str) -> String {
    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
    if !line[..lead].contains('\t') {
        return line.to_string();
    }
    let mut out = String::new();
    for c in line[..lead].chars() {
        if c == '\t' {
            out.push_str(&" ".repeat(4 - out.len() % 4));
        } else {
            out.push(' ');
        }
    }
    out.push_str(&line[lead..]);
    out
}

/// The lines a YAML front matter takes, which are not shown.
fn front_matter(lines: &[String]) -> usize {
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return 0;
    }
    lines
        .iter()
        .skip(1)
        .position(|l| matches!(l.trim_end(), "---" | "..."))
        .map_or(0, |end| end + 2)
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

struct Cx {
    /// `[label]: url "title"` definitions, by lowercased label.
    refs: HashMap<String, (String, Option<String>)>,
}

/// A fence opening: its character, its length and its info string.
fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    if indent(line) > 3 {
        return None;
    }
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = t.len() - t.trim_start_matches(c).len();
    let info = t[n..].trim();
    (n >= 3 && !(c == '`' && info.contains('`'))).then_some((c, n, info))
}

fn fence_close(line: &str, c: char, n: usize) -> bool {
    let t = line.trim();
    indent(line) <= 3 && t.len() >= n && t.chars().all(|x| x == c)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    if indent(line) > 3 {
        return None;
    }
    let t = line.trim_start();
    let level = t.len() - t.trim_start_matches('#').len();
    let rest = &t[level..];
    if !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let rest = rest.trim();
    let closed = rest.trim_end_matches('#');
    let text = if closed.is_empty() || closed.ends_with(' ') {
        closed.trim_end()
    } else {
        rest
    };
    Some((level, text))
}

fn rule(line: &str) -> bool {
    if indent(line) > 3 {
        return false;
    }
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && ['-', '*', '_'].iter().any(|&c| t.chars().all(|x| x == c))
}

struct Marker {
    indent: usize,
    ordered: bool,
    /// The character after the number, or the bullet itself: a change of
    /// it starts a new list, as in CommonMark.
    kind: char,
    start: u64,
    /// Where the item's content starts, in columns.
    content: usize,
}

fn marker(line: &str) -> Option<Marker> {
    let ind = indent(line);
    if ind > 3 && ind >= line.len() {
        return None;
    }
    let t = &line[ind..];
    let (ordered, kind, start, width) = match t.chars().next()? {
        c @ ('-' | '*' | '+') => (false, c, 1, 1),
        c if c.is_ascii_digit() => {
            let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let delim = t[digits..].chars().next()?;
            if digits > 9 || !matches!(delim, '.' | ')') {
                return None;
            }
            (true, delim, t[..digits].parse().ok()?, digits + 1)
        }
        _ => return None,
    };
    let rest = &t[width..];
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let spaces = indent(rest);
    let gap = if rest.trim().is_empty() || spaces > 4 {
        1
    } else {
        spaces
    };
    Some(Marker {
        indent: ind,
        ordered,
        kind,
        start,
        content: ind + width + gap,
    })
}

fn quote_line(line: &str) -> bool {
    indent(line) <= 3 && line.trim_start().starts_with('>')
}

fn html_block(line: &str) -> bool {
    if indent(line) > 3 {
        return false;
    }
    let t = line.trim_start();
    let mut c = t.chars().skip(1);
    t.starts_with('<')
        && match c.next() {
            Some('/') => c.next().is_some_and(|c| c.is_ascii_alphabetic()),
            Some('!') => true,
            Some(x) => x.is_ascii_alphabetic(),
            None => false,
        }
}

fn table_cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") {
        &t[..t.len() - 1]
    } else {
        t
    };
    let mut cells = Vec::new();
    let mut from = 0;
    let mut code = false;
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'`' => code = !code,
            b'|' if !code => {
                cells.push(t[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    cells.push(t[from..].trim());
    cells
}

fn delimiter_row(line: &str) -> Option<Vec<&'static str>> {
    if !line.contains('-') {
        return None;
    }
    table_cells(line)
        .iter()
        .map(|c| {
            let inner = c.trim_start_matches(':').trim_end_matches(':');
            if inner.is_empty() || !inner.chars().all(|x| x == '-') {
                return None;
            }
            Some(match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => "center",
                (false, true) => "right",
                (true, false) => "left",
                _ => "",
            })
        })
        .collect()
}

fn ref_def(line: &str) -> Option<(String, String, Option<String>)> {
    if indent(line) > 3 {
        return None;
    }
    let t = line.trim();
    let close = t.strip_prefix('[')?.find("]:")? + 1;
    let label = t[1..close].trim();
    let rest = t[close + 2..].trim();
    if label.is_empty() || rest.is_empty() {
        return None;
    }
    let (url, after) = match rest.strip_prefix('<') {
        Some(r) => {
            let end = r.find('>')?;
            (&r[..end], r[end + 1..].trim())
        }
        None => {
            let end = rest.find(' ').unwrap_or(rest.len());
            (&rest[..end], rest[end..].trim())
        }
    };
    let title = match after.chars().next() {
        None => None,
        Some(q @ ('"' | '\'')) if after.len() >= 2 && after.ends_with(q) => {
            Some(after[1..after.len() - 1].to_string())
        }
        Some('(') if after.ends_with(')') => Some(after[1..after.len() - 1].to_string()),
        _ => return None,
    };
    Some((label.to_lowercase(), url.to_string(), title))
}

fn references(lines: &[String]) -> HashMap<String, (String, Option<String>)> {
    let mut refs = HashMap::new();
    let mut fence: Option<(char, usize)> = None;
    for line in lines {
        if let Some((c, n)) = fence {
            if fence_close(line, c, n) {
                fence = None;
            }
        } else if let Some((c, n, _)) = fence_open(line) {
            fence = Some((c, n));
        } else if let Some((label, url, title)) = ref_def(line) {
            refs.entry(label).or_insert((url, title));
        }
    }
    refs
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// A link target that may not run script when clicked in the page.
fn safe_url(url: &str) -> String {
    let lower: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") {
        "#".to_string()
    } else {
        esc(url)
    }
}

impl Cx {
    /// Renders `lines` as blocks. `top` is the file line index of
    /// `lines[0]` when these are the page's top-level blocks, which are the
    /// ones tagged; nested blocks are not, so a pin lands on the outer one.
    fn blocks(
        &self,
        lines: &[String],
        top: Option<usize>,
        tight: bool,
        depth: usize,
        out: &mut String,
    ) {
        if depth > MAX_DEPTH {
            out.push_str("<p>");
            esc_into(&lines.join("\n"), out);
            out.push_str("</p>\n");
            return;
        }
        let tag = |from: usize, to: usize| match top {
            Some(base) => format!(" data-lines=\"{}-{}\"", base + from + 1, base + to),
            None => String::new(),
        };
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].as_str();
            if blank(line) {
                i += 1;
                continue;
            }
            if let Some((c, n, info)) = fence_open(line) {
                let lead = indent(line);
                let mut j = i + 1;
                while j < lines.len() && !fence_close(&lines[j], c, n) {
                    j += 1;
                }
                let code: Vec<&str> = lines[i + 1..j.min(lines.len())]
                    .iter()
                    .map(|l| &l[indent(l).min(lead)..])
                    .collect();
                let end = (j + 1).min(lines.len());
                let lang = info.split_whitespace().next().unwrap_or("");
                let mut code = code.join("\n");
                if !code.is_empty() {
                    code.push('\n');
                }
                if lang.eq_ignore_ascii_case("mermaid") {
                    out.push_str(&format!("<pre class=\"mermaid\"{}>", tag(i, end)));
                    esc_into(&code, out);
                    out.push_str("</pre>\n");
                } else {
                    out.push_str(&format!("<pre{}><code", tag(i, end)));
                    if !lang.is_empty() {
                        out.push_str(&format!(" class=\"language-{}\"", esc(lang)));
                    }
                    out.push('>');
                    esc_into(&code, out);
                    out.push_str("</code></pre>\n");
                }
                i = end;
                continue;
            }
            if let Some((level, text)) = heading(line) {
                let id = slug(text);
                out.push_str(&format!("<h{level}"));
                if !id.is_empty() {
                    out.push_str(&format!(" id=\"{}\"", esc(&id)));
                }
                out.push_str(&format!(
                    "{}>{}</h{level}>\n",
                    tag(i, i + 1),
                    self.inline(text)
                ));
                i += 1;
                continue;
            }
            if rule(line) {
                out.push_str(&format!("<hr{}>\n", tag(i, i + 1)));
                i += 1;
                continue;
            }
            if ref_def(line).is_some() {
                i += 1;
                continue;
            }
            if line.contains('|') {
                if let Some(align) = lines.get(i + 1).and_then(|l| delimiter_row(l)) {
                    let header = table_cells(line);
                    if header.len() == align.len() {
                        let mut j = i + 2;
                        while j < lines.len() && !blank(&lines[j]) && lines[j].contains('|') {
                            j += 1;
                        }
                        self.table(&header, &align, &lines[i + 2..j], &tag(i, j), out);
                        i = j;
                        continue;
                    }
                }
            }
            if quote_line(line) {
                let mut inner = Vec::new();
                let mut j = i;
                while j < lines.len() && !blank(&lines[j]) {
                    let l = &lines[j];
                    if quote_line(l) {
                        let t = l.trim_start()[1..].to_string();
                        inner.push(t.strip_prefix(' ').map(str::to_string).unwrap_or(t));
                    } else if j > i && starts_block(l) {
                        break;
                    } else {
                        inner.push(l.clone());
                    }
                    j += 1;
                }
                out.push_str(&format!("<blockquote{}>\n", tag(i, j)));
                self.blocks(&inner, None, false, depth + 1, out);
                out.push_str("</blockquote>\n");
                i = j;
                continue;
            }
            if let Some(m) = marker(line).filter(|m| m.indent <= 3) {
                let end = self.list(lines, i, m, &tag, depth, out);
                i = end;
                continue;
            }
            if html_block(line) {
                let mut j = i;
                while j < lines.len() && !blank(&lines[j]) {
                    j += 1;
                }
                let raw = lines[i..j].join("\n");
                out.push_str(&with_attr(&raw, &tag(i, j)));
                out.push('\n');
                i = j;
                continue;
            }
            if indent(line) >= 4 {
                let mut j = i;
                let mut last = i;
                while j < lines.len() && (blank(&lines[j]) || indent(&lines[j]) >= 4) {
                    if !blank(&lines[j]) {
                        last = j;
                    }
                    j += 1;
                }
                let code: Vec<&str> = lines[i..=last]
                    .iter()
                    .map(|l| l.get(4..).unwrap_or(""))
                    .collect();
                out.push_str(&format!("<pre{}><code>", tag(i, last + 1)));
                esc_into(&code.join("\n"), out);
                out.push_str("\n</code></pre>\n");
                i = last + 1;
                continue;
            }
            // A paragraph, or a setext heading when underlined.
            let mut j = i + 1;
            let mut setext = 0;
            while j < lines.len() {
                let l = lines[j].as_str();
                if blank(l) {
                    break;
                }
                if indent(l) <= 3 {
                    let t = l.trim();
                    if !t.is_empty() && t.chars().all(|c| c == '=') {
                        setext = 1;
                        break;
                    }
                    if !t.is_empty() && t.chars().all(|c| c == '-') {
                        setext = 2;
                        break;
                    }
                }
                if starts_block(l) {
                    break;
                }
                j += 1;
            }
            let text: Vec<&str> = lines[i..j].iter().map(|l| l.trim_start()).collect();
            let text = text.join("\n");
            let text = text.trim_end();
            if setext > 0 {
                let id = slug(text);
                out.push_str(&format!(
                    "<h{setext} id=\"{}\"{}>{}</h{setext}>\n",
                    esc(&id),
                    tag(i, j + 1),
                    self.inline(text)
                ));
                i = j + 1;
            } else if tight && top.is_none() {
                out.push_str(&self.inline(text));
                out.push('\n');
                i = j;
            } else {
                out.push_str(&format!("<p{}>{}</p>\n", tag(i, j), self.inline(text)));
                i = j;
            }
        }
    }

    fn table(&self, header: &[&str], align: &[&str], rows: &[String], tag: &str, out: &mut String) {
        let style = |c: usize| match align.get(c) {
            Some(a) if !a.is_empty() => format!(" style=\"text-align:{a}\""),
            _ => String::new(),
        };
        out.push_str(&format!("<table{tag}>\n<thead><tr>"));
        for (c, cell) in header.iter().enumerate() {
            out.push_str(&format!("<th{}>{}</th>", style(c), self.inline(cell)));
        }
        out.push_str("</tr></thead>\n<tbody>\n");
        for row in rows {
            let cells = table_cells(row);
            out.push_str("<tr>");
            for c in 0..header.len() {
                let cell = cells.get(c).copied().unwrap_or("");
                out.push_str(&format!("<td{}>{}</td>", style(c), self.inline(cell)));
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody></table>\n");
    }

    /// Renders the list starting at `lines[i]` and returns the index after
    /// its last line.
    fn list(
        &self,
        lines: &[String],
        i: usize,
        first: Marker,
        tag: &dyn Fn(usize, usize) -> String,
        depth: usize,
        out: &mut String,
    ) -> usize {
        // Each item: the line it starts on and its content lines.
        let mut items: Vec<Vec<String>> = Vec::new();
        let mut content = first.content;
        let mut j = i;
        let mut last = i;
        let mut loose = false;
        let mut gap = false;
        while j < lines.len() {
            let line = lines[j].as_str();
            if blank(line) {
                gap = true;
                j += 1;
                continue;
            }
            let m = marker(line).filter(|m| m.indent < content && !rule(line));
            if let Some(m) = m {
                if m.ordered != first.ordered || m.kind != first.kind {
                    break;
                }
                if gap && !items.is_empty() {
                    loose = true;
                }
                content = m.content;
                items.push(vec![line.get(m.content..).unwrap_or("").to_string()]);
            } else if indent(line) >= content {
                let item = items.last_mut().expect("first line is a marker");
                if gap {
                    if item.iter().any(|l| !blank(l)) && !nested_list_gap(item) {
                        loose = true;
                    }
                    item.push(String::new());
                }
                item.push(line[content..].to_string());
            } else if !gap && !starts_block(line) && !rule(line) {
                // A lazy continuation of the item's paragraph.
                let item = items.last_mut().expect("first line is a marker");
                item.push(line.trim_start().to_string());
            } else {
                break;
            }
            gap = false;
            last = j;
            j += 1;
        }
        let end = last + 1;
        let open = if first.ordered {
            if first.start == 1 {
                "<ol".to_string()
            } else {
                format!("<ol start=\"{}\"", first.start)
            }
        } else {
            "<ul".to_string()
        };
        out.push_str(&format!("{open}{}>\n", tag(i, end)));
        for mut item in items {
            let head = item[0].clone();
            let task = if !first.ordered {
                match head.get(..4) {
                    Some("[ ] ") => Some(false),
                    Some("[x] " | "[X] ") => Some(true),
                    _ if matches!(head.as_str(), "[ ]" | "[x]" | "[X]") => Some(head != "[ ]"),
                    _ => None,
                }
            } else {
                None
            };
            match task {
                Some(done) => {
                    item[0] = head.get(4..).unwrap_or("").to_string();
                    let checked = if done { " checked" } else { "" };
                    out.push_str(&format!(
                        "<li class=\"task\"><input type=\"checkbox\" disabled{checked}>"
                    ));
                }
                None => out.push_str("<li>"),
            }
            self.blocks(&item, None, !loose, depth + 1, out);
            out.push_str("</li>\n");
        }
        out.push_str(if first.ordered { "</ol>\n" } else { "</ul>\n" });
        end
    }

    fn inline(&self, src: &str) -> String {
        let mut out = String::new();
        self.inline_into(src, 0, &mut out);
        out
    }

    fn inline_into(&self, s: &str, depth: usize, out: &mut String) {
        if depth > MAX_DEPTH {
            esc_into(s, out);
            return;
        }
        let b = s.as_bytes();
        let mut i = 0;
        while i < s.len() {
            let c = b[i];
            match c {
                b'\\' if i + 1 < s.len() && b[i + 1] == b'\n' => {
                    out.push_str("<br>\n");
                    i += 2;
                    continue;
                }
                b'\\' if i + 1 < s.len() && b[i + 1].is_ascii_punctuation() => {
                    esc_into(&s[i + 1..i + 2], out);
                    i += 2;
                    continue;
                }
                b'`' => {
                    let n = run(b, i, b'`');
                    if let Some(close) = find_run(s, i + n, b'`', n) {
                        let code = s[i + n..close].replace('\n', " ");
                        let code = if code.len() > 1
                            && code.starts_with(' ')
                            && code.ends_with(' ')
                            && code.trim() != ""
                        {
                            &code[1..code.len() - 1]
                        } else {
                            &code[..]
                        };
                        out.push_str("<code>");
                        esc_into(code, out);
                        out.push_str("</code>");
                        i = close + n;
                    } else {
                        out.push_str(&s[i..i + n]);
                        i += n;
                    }
                    continue;
                }
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    if let Some((text, url, title, end)) = self.link(s, i + 1) {
                        out.push_str(&format!(
                            "<img src=\"{}\" alt=\"{}\"",
                            safe_url(&url),
                            esc(&plain(&text))
                        ));
                        if let Some(t) = title {
                            out.push_str(&format!(" title=\"{}\"", esc(&t)));
                        }
                        out.push('>');
                        i = end;
                        continue;
                    }
                }
                b'[' => {
                    if let Some((text, url, title, end)) = self.link(s, i) {
                        out.push_str(&format!("<a href=\"{}\"", safe_url(&url)));
                        if let Some(t) = title {
                            out.push_str(&format!(" title=\"{}\"", esc(&t)));
                        }
                        out.push('>');
                        self.inline_into(&text, depth + 1, out);
                        out.push_str("</a>");
                        i = end;
                        continue;
                    }
                }
                b'<' => {
                    if let Some(end) = s[i..].find('>').map(|e| i + e) {
                        let inner = &s[i + 1..end];
                        if is_autolink(inner) {
                            let href = if inner.contains(':') {
                                inner.to_string()
                            } else {
                                format!("mailto:{inner}")
                            };
                            out.push_str(&format!("<a href=\"{}\">", safe_url(&href)));
                            esc_into(inner, out);
                            out.push_str("</a>");
                            i = end + 1;
                            continue;
                        }
                        if is_inline_tag(inner) {
                            out.push_str(&s[i..=end]);
                            i = end + 1;
                            continue;
                        }
                    }
                }
                b'&' => {
                    let entity = s[i + 1..]
                        .find(';')
                        .filter(|&e| (1..=32).contains(&e))
                        .filter(|&e| {
                            s[i + 1..i + 1 + e]
                                .bytes()
                                .all(|x| x.is_ascii_alphanumeric() || x == b'#')
                        });
                    if let Some(e) = entity {
                        out.push_str(&s[i..i + e + 2]);
                        i += e + 2;
                        continue;
                    }
                }
                b'*' | b'_' | b'~' => {
                    if let Some(end) = self.emphasis(s, i, depth, out) {
                        i = end;
                        continue;
                    }
                }
                b'h' if (i == 0 || !b[i - 1].is_ascii_alphanumeric())
                    && (s[i..].starts_with("https://") || s[i..].starts_with("http://")) =>
                {
                    let len = bare_url(&s[i..]);
                    if len > "https://".len() {
                        let url = &s[i..i + len];
                        out.push_str(&format!("<a href=\"{}\">", safe_url(url)));
                        esc_into(url, out);
                        out.push_str("</a>");
                        i += len;
                        continue;
                    }
                }
                b'\n' => {
                    if out.ends_with("  ") {
                        let kept = out.trim_end_matches(' ').len();
                        out.truncate(kept);
                        out.push_str("<br>\n");
                    } else {
                        let kept = out.trim_end_matches(' ').len();
                        out.truncate(kept);
                        out.push('\n');
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let ch = s[i..].chars().next().expect("i is on a char boundary");
            let mut buf = [0; 4];
            esc_into(ch.encode_utf8(&mut buf), out);
            i += ch.len_utf8();
        }
    }

    /// `*em*`, `**strong**`, `***both***`, the same with `_`, and
    /// `~~struck~~`. Returns where the text after the closer starts, or None
    /// when `s[i..]` opens nothing and is literal.
    fn emphasis(&self, s: &str, i: usize, depth: usize, out: &mut String) -> Option<usize> {
        let b = s.as_bytes();
        let c = b[i];
        let n = run(b, i, c);
        if c == b'~' && n != 2 {
            return None;
        }
        let n = n.min(3);
        let next = s[i + n..].chars().next()?;
        if next.is_whitespace() {
            return None;
        }
        let prev = s[..i].chars().next_back();
        if c == b'_' && prev.is_some_and(char::is_alphanumeric) {
            return None;
        }
        let close = self.closer(s, i + n, c, n)?;
        let (open, shut) = match (c, n) {
            (b'~', _) => ("<del>", "</del>"),
            (_, 1) => ("<em>", "</em>"),
            (_, 2) => ("<strong>", "</strong>"),
            _ => ("<strong><em>", "</em></strong>"),
        };
        out.push_str(open);
        self.inline_into(&s[i + n..close], depth + 1, out);
        out.push_str(shut);
        Some(close + n)
    }

    fn closer(&self, s: &str, from: usize, c: u8, n: usize) -> Option<usize> {
        let b = s.as_bytes();
        let mut j = from;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'`' => {
                    let m = run(b, j, b'`');
                    j = find_run(s, j + m, b'`', m).map_or(j + m, |e| e + m);
                }
                x if x == c => {
                    let m = run(b, j, c);
                    let before = s[..j].chars().next_back();
                    let after = s[j + m..].chars().next();
                    let flanks = j > from && before.is_some_and(|p| !p.is_whitespace());
                    let word_ok = c != b'_' || !after.is_some_and(char::is_alphanumeric);
                    if flanks && word_ok && m >= n {
                        return Some(j + m - n);
                    }
                    j += m;
                }
                _ => j += 1,
            }
        }
        None
    }

    /// A link or image body at `s[i] == '['`: inline `[t](url "title")`,
    /// full `[t][ref]`, collapsed `[t][]` or shortcut `[t]` with a matching
    /// definition. Returns its text, url, title and the end.
    fn link(&self, s: &str, i: usize) -> Option<(String, String, Option<String>, usize)> {
        let b = s.as_bytes();
        let mut depth = 0;
        let mut j = i;
        let close = loop {
            match b.get(j)? {
                b'\\' => j += 1,
                b'`' => {
                    let m = run(b, j, b'`');
                    j = find_run(s, j + m, b'`', m).map_or(j + m, |e| e + m) - 1;
                }
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        break j;
                    }
                }
                _ => {}
            }
            j += 1;
        };
        let text = s[i + 1..close].to_string();
        let after = &s[close + 1..];
        if let Some(rest) = after.strip_prefix('(') {
            let (url, title, used) = destination(rest)?;
            return Some((text, url, title, close + 2 + used));
        }
        let (label, end) = match after.strip_prefix('[') {
            Some(rest) => {
                let e = rest.find(']')?;
                let label = if e == 0 {
                    text.clone()
                } else {
                    rest[..e].to_string()
                };
                (label, close + 2 + e + 1)
            }
            None => (text.clone(), close + 1),
        };
        let (url, title) = self.refs.get(&label.trim().to_lowercase())?;
        Some((text, url.clone(), title.clone(), end))
    }
}

/// A link destination and title after `(`, and how many bytes of `rest`
/// they and the closing `)` took.
fn destination(rest: &str) -> Option<(String, Option<String>, usize)> {
    let lead = rest.len() - rest.trim_start().len();
    let r = &rest[lead..];
    let (url, mut at) = if let Some(inner) = r.strip_prefix('<') {
        let e = inner.find('>')?;
        (inner[..e].to_string(), lead + e + 2)
    } else {
        let mut parens = 0;
        let mut e = 0;
        for (k, c) in r.char_indices() {
            match c {
                '(' => parens += 1,
                ')' if parens == 0 => break,
                ')' => parens -= 1,
                c if c.is_whitespace() => break,
                _ => {}
            }
            e = k + c.len_utf8();
        }
        (r[..e].to_string(), lead + e)
    };
    at += rest[at..].len() - rest[at..].trim_start().len();
    let mut title = None;
    if let Some(q) = rest[at..]
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))
    {
        let e = rest[at + 1..].find(q)?;
        title = Some(rest[at + 1..at + 1 + e].to_string());
        at += e + 2;
        at += rest[at..].len() - rest[at..].trim_start().len();
    }
    rest[at..].starts_with(')').then_some((url, title, at + 1))
}

fn run(b: &[u8], i: usize, c: u8) -> usize {
    b[i..].iter().take_while(|&&x| x == c).count()
}

/// The start of the next run of exactly `n` of `c` from `from`.
fn find_run(s: &str, from: usize, c: u8, n: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut j = from;
    while j < b.len() {
        if b[j] == c {
            let m = run(b, j, c);
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

fn is_autolink(inner: &str) -> bool {
    if inner.is_empty() || inner.contains(char::is_whitespace) || inner.contains('<') {
        return false;
    }
    match inner.find(':') {
        Some(k) => {
            k >= 2
                && inner[..k]
                    .bytes()
                    .all(|x| x.is_ascii_alphanumeric() || b"+.-".contains(&x))
        }
        None => inner.contains('@') && !inner.starts_with('@') && !inner.ends_with('@'),
    }
}

/// `<kbd>`, `</span>`, `<br/>`, `<img src=x>`, `<!-- -->`: raw HTML a
/// markdown file may hold inline, passed through as CommonMark does.
fn is_inline_tag(inner: &str) -> bool {
    if inner.starts_with("!--") {
        return inner.ends_with("--");
    }
    let name = inner.strip_prefix('/').unwrap_or(inner);
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && !inner.contains('\n')
        && name
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// How long a bare `http(s)://` address is: to whitespace or `<`, less
/// trailing punctuation and an unbalanced closing paren.
fn bare_url(s: &str) -> usize {
    let mut end = s
        .find(|c: char| c.is_whitespace() || c == '<')
        .unwrap_or(s.len());
    loop {
        let t = &s[..end];
        match t.chars().next_back() {
            Some('.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' | '~' | '\'' | '"') => end -= 1,
            Some(')') if t.matches(')').count() > t.matches('(').count() => end -= 1,
            _ => return end,
        }
    }
}

/// The text of inline markdown with its markup dropped, for an image's alt.
fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '[' | ']'))
        .collect()
}

fn starts_block(line: &str) -> bool {
    fence_open(line).is_some()
        || heading(line).is_some()
        || quote_line(line)
        || html_block(line)
        || rule(line)
        || marker(line).is_some_and(|m| m.indent <= 3 && (!m.ordered || m.start == 1))
}

/// Whether a blank inside an item only separates its text from a nested
/// list, which keeps a list tight in practice.
fn nested_list_gap(item: &[String]) -> bool {
    item.last().is_some_and(|l| marker(l).is_some())
}

/// Puts `attr` into the first opening tag of a raw HTML block, so the
/// block keeps its own element and still maps back to its lines.
fn with_attr(raw: &str, attr: &str) -> String {
    let t = raw.trim_start();
    let lead = raw.len() - t.len();
    if attr.is_empty()
        || !t.starts_with('<')
        || !t[1..].starts_with(|c: char| c.is_ascii_alphabetic())
    {
        return raw.to_string();
    }
    let name_end = t[1..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .map_or(t.len(), |e| e + 1);
    format!(
        "{}{}{}{}",
        &raw[..lead],
        &t[..name_end],
        attr,
        &t[name_end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> String {
        let html = markdown_html(src, "t");
        let start =
            html.find("<main class=\"forge-md\">\n").unwrap() + "<main class=\"forge-md\">\n".len();
        let end = html.rfind("</main>").unwrap();
        html[start..end].to_string()
    }

    #[test]
    fn forge_md_line_ranges() {
        let src = "# Title\n\nOne\ntwo\n\n- a\n- b\n\n```rust\nfn x() {}\n```\n\n---\n";
        assert_eq!(
            body(src),
            "<h1 id=\"title\" data-lines=\"1-1\">Title</h1>\n\
             <p data-lines=\"3-4\">One\ntwo</p>\n\
             <ul data-lines=\"6-7\">\n<li>a\n</li>\n<li>b\n</li>\n</ul>\n\
             <pre data-lines=\"9-11\"><code class=\"language-rust\">fn x() {}\n</code></pre>\n\
             <hr data-lines=\"13-13\">\n"
        );
    }

    #[test]
    fn forge_md_crlf_and_front_matter() {
        let src = "---\r\ntitle: x\r\n---\r\n\r\nHello  \r\nworld\r\n";
        assert_eq!(body(src), "<p data-lines=\"5-6\">Hello<br>\nworld</p>\n");
    }

    #[test]
    fn forge_md_mermaid() {
        let html = body("Text\n\n```mermaid\ngraph TD\n  A-->B\n```\n");
        assert!(
            html.contains("<pre class=\"mermaid\" data-lines=\"3-6\">graph TD\n  A--&gt;B\n</pre>")
        );
    }

    #[test]
    fn forge_md_nested_untagged() {
        let html = body("> quote\n> - item\n\n1. one\n   more\n2. two\n   - deep\n");
        assert!(html.starts_with("<blockquote data-lines=\"1-2\">\n<p>quote</p>\n<ul>\n"));
        assert!(html.contains("<ol data-lines=\"4-7\">"));
        assert_eq!(html.matches("data-lines").count(), 2);
        assert!(html.contains("<li>one\nmore\n</li>"));
        assert!(html.contains("<li>two\n<ul>\n<li>deep\n</li>\n</ul>\n</li>"));
    }

    #[test]
    fn forge_md_tasks_tables_images() {
        let html =
            body("- [ ] todo\n- [x] done\n\n| a | b |\n|:--|--:|\n| `x|y` | ![p](img/a.png) |\n");
        assert!(html.contains("<li class=\"task\"><input type=\"checkbox\" disabled>todo"));
        assert!(html.contains("<input type=\"checkbox\" disabled checked>done"));
        assert!(html.contains("<table data-lines=\"4-6\">"));
        assert!(html.contains("<th style=\"text-align:left\">a</th>"));
        assert!(html.contains("<td style=\"text-align:left\"><code>x|y</code></td>"));
        assert!(html.contains("<img src=\"img/a.png\" alt=\"p\">"));
    }

    #[test]
    fn forge_md_inline() {
        let cx = Cx {
            refs: HashMap::new(),
        };
        assert_eq!(
            cx.inline("**bold** and *it* and ~~s~~"),
            "<strong>bold</strong> and <em>it</em> and <del>s</del>"
        );
        assert_eq!(cx.inline("snake_case_name"), "snake_case_name");
        assert_eq!(cx.inline("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(
            cx.inline("[a](../b.md \"T\")"),
            "<a href=\"../b.md\" title=\"T\">a</a>"
        );
        assert_eq!(cx.inline("[x](javascript:alert(1))"), "<a href=\"#\">x</a>");
        assert_eq!(
            cx.inline("see https://a.dk/x."),
            "see <a href=\"https://a.dk/x\">https://a.dk/x</a>."
        );
        assert_eq!(
            cx.inline("a <kbd>K</kbd> & b < c &amp;"),
            "a <kbd>K</kbd> &amp; b &lt; c &amp;"
        );
        assert_eq!(cx.inline("æ **ø** å"), "æ <strong>ø</strong> å");
        assert_eq!(cx.inline("`a ``b`` c`"), "<code>a ``b`` c</code>");
    }

    #[test]
    fn forge_md_reference_links_and_setext() {
        let html = body("See [the doc][d] and [d].\n\n[d]: https://x.dk \"X\"\n\nBig\n===\n");
        assert!(html.contains("<a href=\"https://x.dk\" title=\"X\">the doc</a>"));
        assert!(html.contains("and <a href=\"https://x.dk\" title=\"X\">d</a>."));
        assert!(html.contains("<h1 id=\"big\" data-lines=\"5-6\">Big</h1>"));
    }

    #[test]
    fn forge_md_html_block_keeps_its_element() {
        let html = body("<details>\n<summary>S</summary>\n</details>\n");
        assert_eq!(
            html,
            "<details data-lines=\"1-3\">\n<summary>S</summary>\n</details>\n"
        );
    }

    #[test]
    fn forge_md_unclosed_fence_and_loose_list() {
        let html = body("- a\n\n- b\n\n```\nopen");
        assert!(html.contains("<li><p>a</p>\n</li>"));
        assert!(html.contains("<pre data-lines=\"5-6\"><code>open\n</code></pre>"));
    }
}
