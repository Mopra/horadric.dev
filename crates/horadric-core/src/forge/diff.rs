//! A line diff of two versions of a text file, for the history's compare.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Removed(String),
    Added(String),
}

/// Past this many cells the middle of the diff is shown as all removed then
/// all added: an LCS table this size is already tens of megabytes, and two
/// versions that far apart read no better line by line.
const MAX_CELLS: usize = 4_000_000;

/// The lines of `a` and `b` in order, each kept, removed from `a` or added
/// in `b`. Within a change the removed lines come before the added ones.
/// CRLF and LF compare equal.
pub fn diff_lines(a: &str, b: &str) -> Vec<DiffLine> {
    let a = split(a);
    let b = split(b);
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut out: Vec<DiffLine> = a[..head].iter().map(|l| DiffLine::Same(l.to_string())).collect();
    if am.len().saturating_mul(bm.len()) > MAX_CELLS {
        out.extend(am.iter().map(|l| DiffLine::Removed(l.to_string())));
        out.extend(bm.iter().map(|l| DiffLine::Added(l.to_string())));
    } else {
        middle(am, bm, &mut out);
    }
    out.extend(a[a.len() - tail..].iter().map(|l| DiffLine::Same(l.to_string())));
    out
}

fn split(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

fn middle(a: &[&str], b: &[&str], out: &mut Vec<DiffLine>) {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    // lcs[i * w + j]: the longest common run of a[i..] and b[j..].
    let mut lcs = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * w + j] = if a[i] == b[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut added = Vec::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.append(&mut added);
            out.push(DiffLine::Same(a[i].to_string()));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            out.push(DiffLine::Removed(a[i].to_string()));
            i += 1;
        } else {
            added.push(DiffLine::Added(b[j].to_string()));
            j += 1;
        }
    }
    out.append(&mut added);
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiffLine::*;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn forge_diff_lines() {
        assert_eq!(
            diff_lines("a\nb\nc\nd\n", "a\nx\nc\nd\ne\n"),
            vec![Same(s("a")), Removed(s("b")), Added(s("x")), Same(s("c")), Same(s("d")), Added(s("e"))]
        );
        assert_eq!(diff_lines("a\r\nb\r\n", "a\nb"), vec![Same(s("a")), Same(s("b"))]);
        assert_eq!(diff_lines("", "x"), vec![Added(s("x"))]);
        assert_eq!(diff_lines("x\n", ""), vec![Removed(s("x"))]);
        assert_eq!(
            diff_lines("1\n2\n3\n4", "1\n9\n8\n4"),
            vec![Same(s("1")), Removed(s("2")), Removed(s("3")), Added(s("9")), Added(s("8")), Same(s("4"))]
        );
    }

    #[test]
    fn forge_diff_moves() {
        let d = diff_lines("a\nb\nc", "c\na\nb");
        let kept: Vec<_> = d.iter().filter(|l| matches!(l, Same(_))).collect();
        assert_eq!(kept.len(), 2);
        let back: Vec<String> = d
            .iter()
            .filter_map(|l| match l {
                Same(t) | Added(t) => Some(t.clone()),
                Removed(_) => None,
            })
            .collect();
        assert_eq!(back, ["c", "a", "b"]);
    }
}
