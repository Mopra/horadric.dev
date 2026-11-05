//! A text edit made on the rendered page, put back into the source it was
//! rendered from without touching the markup around it.
//!
//! Both sides are reduced to the text a reader sees, each character of
//! the source's tied to the bytes it came from. When that text is the
//! page's text before the edit, the changed part is cut out of the source
//! by those ties. When it is not (a script wrote the text, or the markup
//! is more than this reads), the edit is not exact and becomes a pin.

/// A character the reader sees and the source bytes that make it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vis {
    pub ch: char,
    pub start: usize,
    pub end: usize,
    /// Inside code, where markdown's characters are literal.
    pub code: bool,
}

/// How replaced text is written into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Html,
    Markdown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The text did not change.
    Same,
    /// Replace these bytes of the source with this.
    Splice(usize, usize, String),
    /// The source does not read as the page did.
    NotExact,
}

fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.bytes().take(12).position(|b| b == b';')?;
    let name = &s[1..end];
    let ch = if let Some(num) = name.strip_prefix('#') {
        let n = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        char::from_u32(n)?
    } else {
        match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            "nbsp" => '\u{a0}',
            "copy" => '©',
            "reg" => '®',
            "trade" => '™',
            "hellip" => '…',
            "mdash" => '\u{2014}',
            "ndash" => '\u{2013}',
            "lsquo" => '‘',
            "rsquo" => '’',
            "ldquo" => '“',
            "rdquo" => '”',
            "laquo" => '«',
            "raquo" => '»',
            "times" => '×',
            "middot" => '·',
            "bull" => '•',
            "euro" => '€',
            "deg" => '°',
            _ => return None,
        }
    };
    Some((ch, end + 1))
}

/// Elements whose text the page leaves out of what it sends.
const UNSEEN: [&str; 4] = ["script", "style", "template", "noscript"];

/// The text of `src[from..to]`, the content of an HTML element.
pub fn html_visible(src: &str, from: usize, to: usize) -> Vec<Vis> {
    let to = to.min(src.len());
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        if !src.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &src[i..to];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(to, |p| i + p + 3);
            continue;
        }
        let next = b.get(i + 1).copied().unwrap_or(0);
        if b[i] == b'<' && (next.is_ascii_alphabetic() || next == b'/' || next == b'!') {
            let gt = rest.find('>').map_or(to, |p| i + p + 1);
            let name: String = src[i + 1..gt]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            i = gt;
            if UNSEEN.contains(&name.as_str()) {
                let close = src[i..to].to_ascii_lowercase().find(&format!("</{name}"));
                i = close.map_or(to, |p| {
                    let c = i + p;
                    src[c..to].find('>').map_or(to, |q| c + q + 1)
                });
            }
            continue;
        }
        if b[i] == b'&' {
            if let Some((ch, len)) = entity(rest) {
                out.push(Vis { ch, start: i, end: i + len, code: false });
                i += len;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or(' ');
        out.push(Vis { ch, start: i, end: i + ch.len_utf8(), code: false });
        i += ch.len_utf8();
    }
    out
}

/// Where the markdown line `line` (no line ending) begins its text: past
/// quote marks, a list marker, a task box and a heading's hashes.
fn line_prefix(line: &str) -> usize {
    let b = line.as_bytes();
    let mut i = 0;
    let spaces = |i: &mut usize| {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b'\t') {
            *i += 1;
        }
    };
    spaces(&mut i);
    while i < b.len() && b[i] == b'>' {
        i += 1;
        spaces(&mut i);
    }
    let after_space = |j: usize| j < b.len() && (b[j] == b' ' || b[j] == b'\t');
    if i < b.len() && b"-*+".contains(&b[i]) && after_space(i + 1) {
        i += 2;
    } else {
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..10).contains(&digits) && i + digits < b.len() && b".)".contains(&b[i + digits]) && after_space(i + digits + 1) {
            i += digits + 2;
        }
    }
    spaces(&mut i);
    if b.len() >= i + 3 && b[i] == b'[' && b"xX ".contains(&b[i + 1]) && b[i + 2] == b']' && (i + 3 == b.len() || after_space(i + 3)) {
        i += 3;
        spaces(&mut i);
    }
    let hashes = b[i..].iter().take_while(|&&c| c == b'#').count();
    if (1..=6).contains(&hashes) && (i + hashes == b.len() || after_space(i + hashes)) {
        i += hashes;
        spaces(&mut i);
    }
    i
}

/// Bytes of a line, past `prefix`, that end it as markup: a heading's
/// closing hashes.
fn line_suffix(line: &str, prefix: usize, heading: bool) -> usize {
    if !heading {
        return line.len();
    }
    let t = line.trim_end();
    let body = t.trim_end_matches('#');
    if body.len() < t.len() && (body.len() <= prefix || body.ends_with([' ', '\t'])) {
        body.trim_end().len().max(prefix)
    } else {
        t.len()
    }
}

fn underline(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && (t.bytes().all(|c| c == b'=') || t.bytes().all(|c| c == b'-'))
}

/// After `[` at `open`: the `]` that closes it, skipping escapes and
/// nested brackets.
fn close_bracket(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// After `(` at `open`: just past the `)` that closes it.
fn close_paren(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'\n' if i + 1 < b.len() && b[i + 1] == b'\n' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// For a link or image whose text opens at `open`: where its text closes
/// and just past its destination or reference.
fn link_end(b: &[u8], open: usize) -> Option<(usize, usize)> {
    let close = close_bracket(b, open)?;
    match b.get(close + 1) {
        Some(b'(') => Some((close, close_paren(b, close + 1)?)),
        Some(b'[') => Some((close, close_bracket(b, close + 1)? + 1)),
        _ => None,
    }
}

/// The text of one markdown block, `block` being its source lines.
pub fn markdown_visible(block: &str) -> Vec<Vis> {
    let b = block.as_bytes();
    let mut out = Vec::new();
    let lines: Vec<(usize, &str)> = {
        let mut at = 0;
        block
            .split_inclusive('\n')
            .map(|l| {
                let s = at;
                at += l.len();
                (s, l)
            })
            .collect()
    };
    let bare = |l: &str| l.trim_end_matches(['\n', '\r']).len();
    let first = lines.first().map_or("", |l| l.1);
    let lead = first.len() - first.trim_start().len();
    let fence = first.trim_start();
    let fenced = lead < 4 && (fence.starts_with("```") || fence.starts_with("~~~"));
    if fenced || first.starts_with("    ") || first.starts_with('\t') {
        // Code: every character as written.
        let c = fence.as_bytes().first().copied().unwrap_or(b'`');
        for (k, &(s, l)) in lines.iter().enumerate() {
            if fenced && (k == 0 || (k == lines.len() - 1 && l.trim_start().starts_with(c as char))) {
                continue;
            }
            let from = if fenced { 0 } else { (l.len() - l.trim_start_matches([' ', '\t']).len()).min(4) };
            for (j, ch) in l[from..].char_indices() {
                let at = s + from + j;
                out.push(Vis { ch, start: at, end: at + ch.len_utf8(), code: true });
            }
        }
        return out;
    }

    // Which bytes are block markup: prefixes, closing hashes, underlines.
    let mut skip = vec![false; b.len()];
    for (k, &(s, l)) in lines.iter().enumerate() {
        let n = bare(l);
        if k > 0 && underline(&l[..n]) {
            skip[s..s + n].iter_mut().for_each(|x| *x = true);
            continue;
        }
        let p = line_prefix(&l[..n]);
        let lead = l.len() - l.trim_start().len();
        let heading = l[lead.min(n)..n].starts_with('#');
        let end = line_suffix(&l[..n], p, heading);
        // Indentation is whitespace the reader sees as a space; the rest
        // of the prefix is markup.
        let ws = l[..p].len() - l[..p].trim_start().len();
        skip[s + ws..s + p].iter_mut().for_each(|x| *x = true);
        skip[s + end..s + n].iter_mut().for_each(|x| *x = true);
    }

    let mut closes: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if skip[i] {
            i += 1;
            continue;
        }
        if let Some(&(at, end)) = closes.last() {
            if at == i {
                closes.pop();
                i = end;
                continue;
            }
        }
        let ch = block[i..].chars().next().unwrap_or(' ');
        let len = ch.len_utf8();
        let prev = if i == 0 { b' ' } else { b[i - 1] };
        let next = b.get(i + 1).copied().unwrap_or(b' ');
        match ch {
            '\\' if next.is_ascii_punctuation() => {
                out.push(Vis { ch: next as char, start: i, end: i + 2, code: false });
                i += 2;
            }
            '\\' if next == b'\n' || next == b'\r' => {
                out.push(Vis { ch: ' ', start: i, end: i + 1, code: false });
                i += 1;
            }
            '`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                let mut j = i + n;
                let found = loop {
                    let Some(p) = block[j..].find('`') else { break None };
                    let at = j + p;
                    let m = b[at..].iter().take_while(|&&c| c == b'`').count();
                    if m == n {
                        break Some(at);
                    }
                    j = at + m;
                };
                match found {
                    Some(at) => {
                        for (k, c) in block[i + n..at].char_indices() {
                            let s = i + n + k;
                            if !skip[s] {
                                out.push(Vis { ch: c, start: s, end: s + c.len_utf8(), code: true });
                            }
                        }
                        i = at + n;
                    }
                    None => {
                        for k in 0..n {
                            out.push(Vis { ch: '`', start: i + k, end: i + k + 1, code: false });
                        }
                        i += n;
                    }
                }
            }
            '!' if next == b'[' => match link_end(b, i + 1) {
                // An image's words are its alt text, which a reader of the
                // page's text never sees.
                Some((_, end)) => i = end,
                None => {
                    out.push(Vis { ch, start: i, end: i + 1, code: false });
                    i += 1;
                }
            },
            '[' => {
                if let Some(c) = link_end(b, i) {
                    closes.push(c);
                } else {
                    out.push(Vis { ch, start: i, end: i + 1, code: false });
                }
                i += 1;
            }
            '<' => {
                let gt = block[i..].find(['>', '\n']).map(|p| i + p).filter(|&g| b[g] == b'>');
                let inner = gt.map(|g| &block[i + 1..g]).unwrap_or("");
                let tag = next.is_ascii_alphabetic() || next == b'/' || next == b'!';
                match gt {
                    Some(g) if !inner.contains(' ') && (inner.contains("://") || inner.contains('@')) => {
                        for (k, c) in inner.char_indices() {
                            let s = i + 1 + k;
                            out.push(Vis { ch: c, start: s, end: s + c.len_utf8(), code: false });
                        }
                        i = g + 1;
                    }
                    Some(g) if tag => i = g + 1,
                    _ => {
                        out.push(Vis { ch, start: i, end: i + 1, code: false });
                        i += 1;
                    }
                }
            }
            '*' | '_' | '~' => {
                let lone = prev.is_ascii_whitespace() && next.is_ascii_whitespace();
                let inword = ch == '_' && prev.is_ascii_alphanumeric() && next.is_ascii_alphanumeric();
                if lone || inword {
                    out.push(Vis { ch, start: i, end: i + 1, code: false });
                }
                i += 1;
            }
            '&' => match entity(&block[i..]) {
                Some((c, n)) => {
                    out.push(Vis { ch: c, start: i, end: i + n, code: false });
                    i += n;
                }
                None => {
                    out.push(Vis { ch, start: i, end: i + 1, code: false });
                    i += 1;
                }
            },
            _ => {
                out.push(Vis { ch, start: i, end: i + len, code: false });
                i += len;
            }
        }
    }
    out
}

/// A run of `vis` read as one character: whitespace runs fold to one space.
struct Folded {
    ch: char,
    /// The first and last of `vis` it stands for.
    first: usize,
    last: usize,
}

fn fold(vis: &[Vis]) -> Vec<Folded> {
    let mut out: Vec<Folded> = Vec::new();
    for (k, v) in vis.iter().enumerate() {
        if v.ch.is_whitespace() {
            match out.last_mut() {
                Some(f) if f.ch == ' ' => f.last = k,
                Some(_) => out.push(Folded { ch: ' ', first: k, last: k }),
                None => {}
            }
        } else {
            out.push(Folded { ch: v.ch, first: k, last: k });
        }
    }
    if out.last().is_some_and(|f| f.ch == ' ') {
        out.pop();
    }
    out
}

fn fold_str(s: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for c in s.chars() {
        if c.is_whitespace() {
            if out.last().is_some_and(|&l| l != ' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    if out.last() == Some(&' ') {
        out.pop();
    }
    out
}

fn escape(text: &str, lang: Lang, code: bool) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match (lang, c) {
            (Lang::Html, '&') => out.push_str("&amp;"),
            (Lang::Html, '<') => out.push_str("&lt;"),
            (Lang::Html, '>') => out.push_str("&gt;"),
            (Lang::Markdown, '`') if code => return None,
            (Lang::Markdown, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '~' | '#')
                if !code =>
            {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    Some(out)
}

/// The edit that turns the page's `old` text into `new` in the source
/// whose visible text is `vis`, offsets in the same terms as `vis`.
pub fn rewrite(vis: &[Vis], old: &str, new: &str, lang: Lang) -> Outcome {
    let seen = fold(vis);
    let o = fold_str(old);
    let n = fold_str(new);
    if seen.len() != o.len() || seen.iter().zip(&o).any(|(f, c)| f.ch != *c) {
        return Outcome::NotExact;
    }
    if o == n {
        return Outcome::Same;
    }
    let p = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let room = o.len().min(n.len()) - p;
    let s = o.iter().rev().zip(n.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let middle: String = n[p..n.len() - s].iter().collect();
    let (start, end, code) = if p < o.len() - s {
        let (a, z) = (&seen[p], &seen[o.len() - s - 1]);
        // Markup between the changed characters (a bold word's stars, a
        // link's address) would go with them: not exact.
        if vis[a.first..=z.last].windows(2).any(|w| w[0].end != w[1].start) {
            return Outcome::NotExact;
        }
        let code = vis[a.first].code;
        if vis[a.first..=z.last].iter().any(|v| v.code != code) {
            return Outcome::NotExact;
        }
        (vis[a.first].start, vis[z.last].end, code)
    } else if p > 0 {
        let v = &vis[seen[p - 1].last];
        (v.end, v.end, v.code)
    } else if let Some(f) = seen.first() {
        let v = &vis[f.first];
        (v.start, v.start, v.code)
    } else {
        return Outcome::NotExact;
    };
    match escape(&middle, lang, code) {
        Some(text) => Outcome::Splice(start, end, text),
        None => Outcome::NotExact,
    }
}

/// `src` after an edit of the content of an HTML element, `inner` its
/// byte range there.
pub fn edit_html(src: &str, inner: (usize, usize), old: &str, new: &str) -> Result<Option<String>, ()> {
    let vis = html_visible(src, inner.0, inner.1);
    match rewrite(&vis, old, new, Lang::Html) {
        Outcome::Same => Ok(None),
        Outcome::Splice(a, b, text) => Ok(Some(format!("{}{text}{}", &src[..a], &src[b..]))),
        Outcome::NotExact => Err(()),
    }
}

/// The byte range of lines `a` to `b` (1 based, inclusive) of `src`,
/// without the last one's line ending.
pub fn line_range(src: &str, a: usize, b: usize) -> Option<(usize, usize)> {
    if a == 0 || b < a {
        return None;
    }
    let mut at = 0;
    let mut start = None;
    for (k, line) in src.split_inclusive('\n').enumerate() {
        let n = k + 1;
        if n == a {
            start = Some(at);
        }
        if n == b {
            let body = line.trim_end_matches(['\n', '\r']).len();
            return Some((start?, at + body));
        }
        at += line.len();
    }
    None
}

/// `src` after an edit of the markdown block on lines `a` to `b`.
pub fn edit_markdown(src: &str, a: usize, b: usize, old: &str, new: &str) -> Result<Option<String>, ()> {
    let (from, to) = line_range(src, a, b).ok_or(())?;
    let vis = markdown_visible(&src[from..to]);
    match rewrite(&vis, old, new, Lang::Markdown) {
        Outcome::Same => Ok(None),
        Outcome::Splice(s, e, text) => {
            Ok(Some(format!("{}{text}{}", &src[..from + s], &src[from + e..])))
        }
        Outcome::NotExact => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(src: &str, a: usize, b: usize, old: &str, new: &str) -> Option<String> {
        edit_markdown(src, a, b, old, new).ok().flatten()
    }

    #[test]
    fn a_plain_paragraph_is_rewritten_exactly() {
        let src = "# Title\n\nOne two\nthree.\n";
        assert_eq!(md(src, 3, 4, "One two\nthree.", "One 2\nthree.").unwrap(), "# Title\n\nOne 2\nthree.\n");
    }

    #[test]
    fn a_heading_keeps_its_hashes() {
        let src = "## Old title ##\r\n\r\nx\r\n";
        assert_eq!(md(src, 1, 1, "Old title", "New title").unwrap(), "## New title ##\r\n\r\nx\r\n");
    }

    #[test]
    fn formatting_outside_the_change_stays() {
        let src = "Hello **bold** and [a link](https://x.dev/a_b) here.";
        let old = "Hello bold and a link here.";
        assert_eq!(
            md(src, 1, 1, old, "Hello bold and a link right here.").unwrap(),
            "Hello **bold** and [a link](https://x.dev/a_b) right here."
        );
        assert_eq!(
            md(src, 1, 1, old, "Hello bolder and a link here.").unwrap(),
            "Hello **bolder** and [a link](https://x.dev/a_b) here."
        );
        // Changing across the bold word's stars cannot be exact.
        assert!(edit_markdown(src, 1, 1, old, "Hi there and a link here.").is_err());
    }

    #[test]
    fn typed_markdown_characters_are_escaped() {
        assert_eq!(md("a b", 1, 1, "a b", "a *b").unwrap(), "a \\*b");
        assert_eq!(md("use `x_y` now", 1, 1, "use x_y now", "use x_z now").unwrap(), "use `x_z` now");
    }

    #[test]
    fn list_items_and_quotes() {
        let src = "- one\n- two\n";
        assert_eq!(md(src, 1, 2, "\none\ntwo\n", "one\ntwos").unwrap(), "- one\n- twos\n");
        let q = "> said\n> this";
        assert_eq!(md(q, 1, 2, "said this", "said that").unwrap(), "> said\n> that");
    }

    #[test]
    fn text_that_does_not_match_is_not_exact() {
        assert!(edit_markdown("Some text", 1, 1, "Other text", "x").is_err());
        assert_eq!(edit_markdown("Same", 1, 1, "Same", " Same ").unwrap(), None);
    }

    #[test]
    fn html_content_with_entities_and_tags() {
        let src = "<p>Tom &amp; <em>Jerry</em> &lt;3</p>";
        let inner = (3, src.len() - 4);
        let out = edit_html(src, inner, "Tom & Jerry <3", "Tom & Jerry <4 & more").unwrap().unwrap();
        assert_eq!(out, "<p>Tom &amp; <em>Jerry</em> &lt;4 &amp; more</p>");
        let out = edit_html(src, inner, "Tom & Jerry <3", "Tim & Jerry <3").unwrap().unwrap();
        assert_eq!(out, "<p>Tim &amp; <em>Jerry</em> &lt;3</p>");
        let out = edit_html(src, inner, "Tom & Jerry <3", "Jerry <3").unwrap().unwrap();
        assert_eq!(out, "<p><em>Jerry</em> &lt;3</p>");
        // Across the emphasis tags: not exact.
        assert!(edit_html(src, inner, "Tom & Jerry <3", "Tom Ferry <3").is_err());
    }

    #[test]
    fn script_text_is_not_read() {
        let src = "<div>Hi<script>var a = '<b>';</script> there</div>";
        let inner = (5, src.len() - 6);
        let out = edit_html(src, inner, "Hi there", "Hi you").unwrap().unwrap();
        assert_eq!(out, "<div>Hi<script>var a = '<b>';</script> you</div>");
    }

    #[test]
    fn utf8_is_kept_whole() {
        let src = "<h1>Café ☕ ok</h1>";
        let out = edit_html(src, (4, src.len() - 5), "Café ☕ ok", "Café ☕ fine").unwrap().unwrap();
        assert_eq!(out, "<h1>Café ☕ fine</h1>");
    }

    /// Every block the Forge's renderer tags reads, as the page sends it,
    /// the same as this reads its source; tables are the one known miss
    /// (their cells run together on the page) and become pins.
    #[test]
    fn the_renderer_and_the_reader_agree() {
        let src = "---\ntitle: x\n---\n# Quarterly *review*\n\nThis is **bold**, _em_, `code_x`, a [link](http://a.b/c_d) and ![img](p.png) here.\nSecond line with snake_case and 2 * 3 & co &amp; <span>raw</span>.\n\n- one **x**\n- two\n  continued\n1. first\n2. second\n\n> quoted\n> more\n\nSetext\n======\n\n```rust\nfn x() { a*b }\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nAuto <https://x.dev> and https://bare.dev ok.\nEscaped \\*stars\\* and hard  \nbreak.\n";
        let page = horadric_core::forge::markdown_html(src, "t");
        let mut blocks = 0;
        for e in super::super::html::elements(&page) {
            let tag = &page[e.start..e.inner.0];
            let Some(at) = tag.find(" data-lines=\"") else { continue };
            let v = &tag[at + 13..];
            let v = &v[..v.find('"').unwrap()];
            let (a, b) = v.split_once('-').unwrap();
            let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
            let seen: String = html_visible(&page, e.inner.0, e.inner.1).iter().map(|v| v.ch).collect();
            let read = edit_markdown(src, a, b, &seen, &seen);
            assert_eq!(read.is_ok(), e.name != "table", "lines {v}: {seen:?}");
            blocks += 1;
        }
        assert_eq!(blocks, 9);
    }
}
