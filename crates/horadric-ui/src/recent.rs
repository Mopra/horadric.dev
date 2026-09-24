//! Projects sessions were started in, most recent first, so the tray menu
//! can start another one there with one click.
//!
//! Kept in the state file with the rest of what Horadric remembers.

/// How many the menu shows. More than this and it stops being quicker than
/// the folder picker.
pub const MAX: usize = 8;

/// Moves a path to the front, dropping an older entry for the same folder.
/// Windows paths compare without case and either slash.
pub fn remember(list: &mut Vec<String>, path: &str) {
    let path = path.trim_end_matches(['\\', '/']);
    if path.is_empty() {
        return;
    }
    let key = normalise(path);
    list.retain(|p| normalise(p) != key);
    list.insert(0, path.to_string());
    list.truncate(MAX);
}

fn normalise(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// A menu label: the folder name, then where it is.
pub fn label(path: &str) -> (String, String) {
    let clean = path.trim_end_matches(['\\', '/']);
    match clean.rfind(['\\', '/']) {
        Some(i) if i + 1 < clean.len() => (clean[i + 1..].to_string(), clean[..i].to_string()),
        _ => (clean.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates() {
        let mut l = vec![r"C:\a".to_string(), r"C:\b".to_string()];
        remember(&mut l, "c:/B/");
        assert_eq!(l, vec!["c:/B".to_string(), r"C:\a".to_string()]);
        remember(&mut l, "");
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn keeps_at_most_max() {
        let mut l = Vec::new();
        for i in 0..MAX + 3 {
            remember(&mut l, &format!(r"C:\p{i}"));
        }
        assert_eq!(l.len(), MAX);
        assert_eq!(l[0], format!(r"C:\p{}", MAX + 2));
    }

    #[test]
    fn labels_split_name_from_location() {
        assert_eq!(
            label(r"C:\Users\me\Github\horadric.ai\"),
            ("horadric.ai".to_string(), r"C:\Users\me\Github".to_string())
        );
        assert_eq!(label("C:"), ("C:".to_string(), String::new()));
    }
}
