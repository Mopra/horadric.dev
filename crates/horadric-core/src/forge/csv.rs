//! CSV shown as a plain sortable table.

use super::html::{esc_into, page};

/// A full HTML page with the CSV as a table, its first record the header.
/// Every cell carries `data-row` (the record's index in the file, the
/// header 0) and `data-col` (0 based), which survive sorting, so a pin
/// names the cell in the file and not where it sorted to.
pub fn csv_html(src: &str, title: &str) -> String {
    let rows = parse(src);
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut body = String::from("<table>\n");
    if let Some(header) = rows.first() {
        body.push_str("<thead><tr data-row=\"0\">");
        for col in 0..width {
            cell(&mut body, "th", 0, col, header.get(col).map_or("", String::as_str));
        }
        body.push_str("</tr></thead>\n");
    }
    body.push_str("<tbody>\n");
    for (r, row) in rows.iter().enumerate().skip(1) {
        body.push_str(&format!("<tr data-row=\"{r}\">"));
        for col in 0..width {
            cell(&mut body, "td", r, col, row.get(col).map_or("", String::as_str));
        }
        body.push_str("</tr>\n");
    }
    body.push_str("</tbody></table>\n");
    page(title, "forge-csv", &body, SORT)
}

fn cell(out: &mut String, tag: &str, row: usize, col: usize, text: &str) {
    let num = tag == "td" && is_number(text);
    out.push_str(&format!("<{tag} data-row=\"{row}\" data-col=\"{col}\""));
    if num {
        out.push_str(" class=\"num\"");
    }
    out.push('>');
    esc_into(text, out);
    out.push_str(&format!("</{tag}>"));
}

/// Whether a cell reads as a number, a decimal comma allowed since the
/// human's spreadsheets are Danish as often as not.
fn is_number(s: &str) -> bool {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    !t.is_empty() && t.replacen(',', ".", 1).parse::<f64>().is_ok_and(f64::is_finite)
}

/// The records, quoted fields per RFC 4180: a quote inside one doubled, and
/// commas and line breaks inside one kept. The delimiter is whichever of
/// comma, semicolon or tab the first record has most of, since Excel in a
/// Danish locale writes semicolons.
pub(super) fn parse(src: &str) -> Vec<Vec<String>> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let delim = delimiter(src);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = src.chars().peekable();
    let mut started = false;
    while let Some(c) = chars.next() {
        started = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            started = false;
        } else {
            field.push(c);
        }
    }
    if started {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn delimiter(src: &str) -> char {
    let mut counts = [(',', 0), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in src.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(slot) = counts.iter_mut().find(|(d, _)| *d == c) {
                    slot.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts
        .iter()
        .fold((',', 0), |best, &(d, n)| if n > best.1 { (d, n) } else { best })
        .0
}

const SORT: &str = r#"
const num = s => { const t = s.replace(/\s/g, '').replace(',', '.'); return t !== '' && isFinite(t) ? +t : NaN; };
document.querySelectorAll('th').forEach(th => th.addEventListener('click', () => {
  const col = +th.dataset.col, body = document.querySelector('tbody');
  const dir = th.dataset.sort === 'asc' ? 'desc' : 'asc';
  document.querySelectorAll('th').forEach(h => delete h.dataset.sort);
  th.dataset.sort = dir;
  const key = tr => tr.children[col] ? tr.children[col].textContent : '';
  const rows = [...body.rows];
  rows.sort((a, b) => {
    const x = key(a), y = key(b), p = num(x), q = num(y);
    const c = !isNaN(p) && !isNaN(q) ? p - q : x.localeCompare(y, undefined, { numeric: true });
    return dir === 'asc' ? c : -c;
  });
  body.append(...rows);
}));
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(src: &str) -> Vec<Vec<&str>> {
        let parsed = parse(src);
        let leaked: &'static [Vec<String>] = Box::leak(parsed.into_boxed_slice());
        leaked
            .iter()
            .map(|r| r.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn forge_csv_quoting() {
        assert_eq!(
            rows("a,\"b,c\",\"say \"\"hi\"\"\"\r\n1,\"two\r\nlines\",3\r\n"),
            vec![vec!["a", "b,c", "say \"hi\""], vec!["1", "two\r\nlines", "3"]]
        );
    }

    #[test]
    fn forge_csv_edges() {
        assert_eq!(rows("a,b\n,\n"), vec![vec!["a", "b"], vec!["", ""]]);
        assert_eq!(rows("x"), vec![vec!["x"]]);
        assert!(rows("").is_empty());
        assert_eq!(rows("\u{feff}a;b;c\n1,5;2;3"), vec![vec!["a", "b", "c"], vec!["1,5", "2", "3"]]);
        assert_eq!(rows("a\tb\n1\t2"), vec![vec!["a", "b"], vec!["1", "2"]]);
        assert_eq!(rows("\"a;b\",c\n"), vec![vec!["a;b", "c"]]);
    }

    #[test]
    fn forge_csv_table() {
        let html = csv_html("name,n\n<b>,\"1,5\"\nx\n", "t");
        assert!(html.contains("<th data-row=\"0\" data-col=\"1\">n</th>"));
        assert!(html.contains("<td data-row=\"1\" data-col=\"0\">&lt;b&gt;</td>"));
        assert!(html.contains("<td data-row=\"1\" data-col=\"1\" class=\"num\">1,5</td>"));
        assert!(html.contains("<td data-row=\"2\" data-col=\"1\"></td>"));
    }
}
