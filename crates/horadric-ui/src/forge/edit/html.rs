//! Where each element of an HTML file starts and ends in its source, so
//! a hand edit on the page lands on the right bytes of the file.
//!
//! Not a full HTML parser: it finds tags as written. An element the
//! browser closes by itself (a `<p>` or `<li>` left open) gets no range,
//! and an edit to it becomes a pin, which is the safe way to be wrong.

/// An element with both tags in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Lower case.
    pub name: String,
    /// Where the start tag begins.
    pub start: usize,
    /// Just after the tag name in the start tag, where an attribute can go.
    pub name_end: usize,
    /// Between the start tag's `>` and the end tag's `<`.
    pub inner: (usize, usize),
    /// Just after the end tag's `>`.
    pub end: usize,
    /// The `class` attribute as written, empty without one.
    pub class: String,
}

impl Element {
    pub fn has_class(&self, c: &str) -> bool {
        self.class.split_ascii_whitespace().any(|x| x == c)
    }

    pub fn contains(&self, other: &Element) -> bool {
        self.start <= other.start && other.end <= self.end && self != other
    }
}

const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose content is text up to their own end tag, never tags.
pub const RAW: [&str; 4] = ["script", "style", "textarea", "title"];

fn find_ci(src: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = src.as_bytes();
    let n = needle.as_bytes();
    (from..=hay.len().saturating_sub(n.len()))
        .find(|&i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn name_at(b: &[u8], mut i: usize) -> usize {
    while i < b.len()
        && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':' || b[i] == b'_')
    {
        i += 1;
    }
    i
}

/// A start tag's attributes from `i` (just after its name): the index just
/// past its `>`, whether it closed itself with `/>`, and its class.
fn attributes(src: &str, mut i: usize) -> (usize, bool, String) {
    let b = src.as_bytes();
    let mut class = String::new();
    while i < b.len() {
        match b[i] {
            b'>' => return (i + 1, i > 0 && b[i - 1] == b'/', class),
            c if c.is_ascii_whitespace() || c == b'/' => i += 1,
            _ => {
                let name_start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !b"=>/".contains(&b[i]) {
                    i += 1;
                }
                let name = &src[name_start..i];
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < b.len() && b[i] == b'=' {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    let (v0, v1) = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                        let q = b[i];
                        let s = i + 1;
                        let e = b[s..]
                            .iter()
                            .position(|&c| c == q)
                            .map_or(b.len(), |p| s + p);
                        i = (e + 1).min(b.len());
                        (s, e)
                    } else {
                        let s = i;
                        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                            i += 1;
                        }
                        (s, i)
                    };
                    if name.eq_ignore_ascii_case("class") {
                        class = src[v0..v1].to_string();
                    }
                }
            }
        }
    }
    (b.len(), false, class)
}

/// Every element of `src` whose start and end tags are both written, in
/// the order their start tags come.
pub fn elements(src: &str) -> Vec<Element> {
    let b = src.as_bytes();
    let mut out: Vec<Element> = Vec::new();
    // (name, start, name_end, inner start, class)
    let mut open: Vec<(String, usize, usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if src[i..].starts_with("<!--") {
            i = src[i + 4..].find("-->").map_or(b.len(), |p| i + 4 + p + 3);
            continue;
        }
        let next = b.get(i + 1).copied().unwrap_or(0);
        if next == b'!' || next == b'?' {
            i = b[i..]
                .iter()
                .position(|&c| c == b'>')
                .map_or(b.len(), |p| i + p + 1);
            continue;
        }
        if next == b'/' {
            let n_end = name_at(b, i + 2);
            let name = src[i + 2..n_end].to_ascii_lowercase();
            let gt = b[n_end..]
                .iter()
                .position(|&c| c == b'>')
                .map_or(b.len(), |p| n_end + p + 1);
            if let Some(at) = open.iter().rposition(|o| o.0 == name) {
                // Anything still open above it was closed by the browser.
                open.truncate(at + 1);
                let (name, start, name_end, inner, class) = open.pop().unwrap_or_default();
                out.push(Element {
                    name,
                    start,
                    name_end,
                    inner: (inner, i),
                    end: gt,
                    class,
                });
            }
            i = gt;
            continue;
        }
        if !next.is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name_end = name_at(b, i + 1);
        let name = src[i + 1..name_end].to_ascii_lowercase();
        let (after, self_closed, class) = attributes(src, name_end);
        if RAW.contains(&name.as_str()) {
            let close = find_ci(src, after, &format!("</{name}")).unwrap_or(b.len());
            let gt = b[close..]
                .iter()
                .position(|&c| c == b'>')
                .map_or(b.len(), |p| close + p + 1);
            out.push(Element {
                name,
                start: i,
                name_end,
                inner: (after, close),
                end: gt,
                class,
            });
            i = gt;
            continue;
        }
        if !self_closed && !VOID.contains(&name.as_str()) {
            open.push((name, i, name_end, after, class));
        }
        i = after;
    }
    out.sort_by_key(|e| e.start);
    out
}

/// The attribute a served page's elements carry: the byte range of their
/// content in the file, which the page sends back with a hand edit.
pub const AT: &str = "data-forge-at";

/// `src` as served to a file tab: every element with both tags marked
/// with where its content is in `src`. Only the served copy, never the
/// file.
pub fn annotate(src: &str) -> String {
    let els = elements(src);
    let mut out = String::with_capacity(src.len() + els.len() * 28);
    let mut from = 0;
    for e in &els {
        if matches!(e.name.as_str(), "html" | "head" | "body") || RAW.contains(&e.name.as_str()) {
            continue;
        }
        out.push_str(&src[from..e.name_end]);
        out.push_str(&format!(" {AT}=\"{}-{}\"", e.inner.0, e.inner.1));
        from = e.name_end;
    }
    out.push_str(&src[from..]);
    out
}

/// The slides of a deck: each `<section class="slide">` not inside
/// another, in order.
pub fn slides(src: &str) -> Vec<Element> {
    let all: Vec<Element> = elements(src)
        .into_iter()
        .filter(|e| e.name == "section" && e.has_class("slide"))
        .collect();
    all.iter()
        .filter(|e| !all.iter().any(|o| o.contains(e)))
        .cloned()
        .collect()
}

/// Where the whitespace before byte `at` begins, so a slide moved or
/// removed takes its own line and indent with it.
fn lead(src: &str, at: usize) -> usize {
    src[..at].trim_end_matches([' ', '\t', '\r', '\n']).len()
}

/// The deck `src` with its slides made `order`, a list of the current
/// slides by number (1 based). A permutation moves them; one number left
/// out deletes that slide; one number given twice in a row duplicates it.
/// None for anything else, or numbers that do not fit.
pub fn reorder(src: &str, order: &[usize]) -> Option<String> {
    let s = slides(src);
    let n = s.len();
    if n == 0 || order.is_empty() || order.iter().any(|&k| k == 0 || k > n) {
        return None;
    }
    let text = |k: usize| &src[s[k - 1].start..s[k - 1].end];
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    if sorted == (1..=n).collect::<Vec<_>>() {
        // Each slide's place keeps what stood between it and the next, so
        // wrappers and comments between slides stay where they were.
        let mut out = String::with_capacity(src.len());
        out.push_str(&src[..s[0].start]);
        for (place, &k) in order.iter().enumerate() {
            out.push_str(text(k));
            let gap_end = s.get(place + 1).map_or(src.len(), |next| next.start);
            out.push_str(&src[s[place].end..gap_end]);
        }
        return Some(out);
    }
    if order.len() + 1 == n {
        let gone = (1..=n).find(|k| !order.contains(k))?;
        let kept: Vec<usize> = (1..=n).filter(|&k| k != gone).collect();
        if kept != order {
            return None;
        }
        let e = &s[gone - 1];
        let from = lead(src, e.start);
        return Some(format!("{}{}", &src[..from], &src[e.end..]));
    }
    if order.len() == n + 1 {
        let twice = order.windows(2).position(|w| w[0] == w[1])?;
        let k = order[twice];
        let mut rest = order.to_vec();
        rest.remove(twice);
        if rest != (1..=n).collect::<Vec<_>>() {
            return None;
        }
        let e = &s[k - 1];
        let mut sep = &src[lead(src, e.start)..e.start];
        if sep.is_empty() {
            sep = "\n";
        }
        return Some(format!(
            "{}{sep}{}{}",
            &src[..e.end],
            text(k),
            &src[e.end..]
        ));
    }
    None
}

/// One slide's source as a new empty slide of the same layout: each box
/// of words says only what it is for ("Title" in a heading, "Point" in a
/// list, "Text" elsewhere), so it can be filled in with a double click,
/// and its speaker notes are empty. Pictures, styles and scripts stay.
pub fn blank(slide: &str) -> String {
    const BOXES: [&str; 12] = [
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "p",
        "li",
        "td",
        "th",
        "figcaption",
        "blockquote",
    ];
    let els = elements(slide);
    let mut picked: Vec<&Element> = Vec::new();
    for e in &els {
        if RAW.contains(&e.name.as_str()) || picked.iter().any(|p| p.contains(e)) {
            continue;
        }
        let inner = &slide[e.inner.0..e.inner.1];
        let notes = e.name == "aside" && e.has_class("notes");
        let words = !inner.contains('<') && !inner.trim().is_empty();
        if notes || words || BOXES.contains(&e.name.as_str()) {
            picked.push(e);
        }
    }
    let mut out = String::with_capacity(slide.len());
    let mut from = 0;
    for e in picked {
        let say = match e.name.as_str() {
            "aside" => "",
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "Title",
            "li" => "Point",
            _ => "Text",
        };
        out.push_str(&slide[from..e.inner.0]);
        out.push_str(say);
        from = e.inner.1;
    }
    out.push_str(&slide[from..]);
    out
}

/// `src` after `reorder` with the copy a duplicating `order` makes
/// blanked: a new slide in the layout of the one it follows.
pub fn add_after(src: &str, order: &[usize]) -> Option<String> {
    let at = order.windows(2).position(|w| w[0] == w[1])? + 1;
    let out = reorder(src, order)?;
    let copy = slides(&out).into_iter().nth(at)?;
    Some(format!(
        "{}{}{}",
        &out[..copy.start],
        blank(&out[copy.start..copy.end]),
        &out[copy.end..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_find_their_content() {
        let src = "<!doctype html><p class=\"a b\">Hi <b>there</b><br></p><!-- <p>no</p> -->";
        let els = elements(src);
        assert_eq!(els.len(), 2);
        let p = &els[0];
        assert_eq!(p.name, "p");
        assert!(p.has_class("b"));
        assert_eq!(&src[p.inner.0..p.inner.1], "Hi <b>there</b><br>");
        assert_eq!(&src[els[1].inner.0..els[1].inner.1], "there");
    }

    #[test]
    fn a_tag_left_open_gets_no_range() {
        let src = "<ul><li>one<li>two</li></ul>";
        let els = elements(src);
        let names: Vec<_> = els.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["ul", "li"]);
        assert_eq!(&src[els[1].inner.0..els[1].inner.1], "two");
    }

    #[test]
    fn script_content_is_not_tags() {
        let src = "<script>if (a<b) document.write('</p>')</script><p>x</p>";
        let els = elements(src);
        assert_eq!(els[0].name, "script");
        assert_eq!(&src[els[1].inner.0..els[1].inner.1], "x");
    }

    #[test]
    fn annotate_marks_only_the_served_copy() {
        let src = "<body><h1 id=x>Café</h1></body>";
        let out = annotate(src);
        assert_eq!(
            out,
            "<body><h1 data-forge-at=\"15-20\" id=x>Café</h1></body>"
        );
        assert_eq!(&src[15..20], "Café");
    }

    const DECK: &str = "<body>\n  <section class=\"slide\">A<section class=\"slide\">inner</section></section>\n  <section class=\"slide dark\">B</section>\n  <section class=\"slide\">C</section>\n</body>";

    #[test]
    fn slides_are_the_outer_sections() {
        let s = slides(DECK);
        assert_eq!(s.len(), 3);
        assert!(DECK[s[0].start..s[0].end].contains("inner"));
    }

    #[test]
    fn slides_move() {
        let out = reorder(DECK, &[3, 1, 2]).unwrap();
        let s = slides(&out);
        let texts: Vec<_> = s.iter().map(|e| &out[e.inner.0..e.inner.0 + 1]).collect();
        assert_eq!(texts, ["C", "A", "B"]);
        assert!(out.starts_with("<body>\n  <section class=\"slide\">C</section>\n  <section"));
        assert!(out.ends_with("</section>\n</body>"));
    }

    #[test]
    fn a_slide_is_deleted_with_its_line() {
        let out = reorder(DECK, &[1, 3]).unwrap();
        assert!(!out.contains(">B<"));
        assert!(out.contains("</section></section>\n  <section class=\"slide\">C"));
        assert!(reorder(DECK, &[3, 1]).is_none());
    }

    #[test]
    fn a_slide_is_duplicated_after_itself() {
        let out = reorder(DECK, &[1, 2, 2, 3]).unwrap();
        assert_eq!(out.matches(">B<").count(), 2);
        assert!(out.contains("B</section>\n  <section class=\"slide dark\">B</section>\n  <section class=\"slide\">C"));
        assert!(reorder(DECK, &[2, 1, 2, 3]).is_none());
        assert!(reorder(DECK, &[1, 2, 4]).is_none());
    }

    #[test]
    fn a_slide_is_added_empty_in_the_layout_before_it() {
        let src = "<body><section class=\"slide\"><h2 class=\"t\">Why <b>now</b></h2>            <div class=\"cols\"><div><p>Left</p></div><div>Right</div></div><ul><li>A</li><li>B</li></ul>            <img src=\"a.png\"><style>p{}</style><aside class=\"notes\">Say it</aside></section></body>";
        let out = add_after(src, &[1, 1]).unwrap();
        let copy = &out[slides(&out)[1].start..];
        assert!(copy.starts_with("<section class=\"slide\"><h2 class=\"t\">Title</h2>"));
        assert!(copy.contains("<div><p>Text</p></div><div>Text</div>"));
        assert!(copy.contains("<li>Point</li><li>Point</li>"));
        assert!(
            copy.contains("<img src=\"a.png\"><style>p{}</style><aside class=\"notes\"></aside>")
        );
        assert!(out[..slides(&out)[1].start].contains("Why <b>now</b>"));
        assert!(add_after(src, &[1]).is_none());
    }
}
