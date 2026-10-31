//! The name this machine goes by among its brothers and phones: "Laptop"
//! or "vps", as its human chose it, kept in `name.txt` in the state
//! folder. Only the machine itself can say it, since its roster entry is
//! signed by its own key (`crate::said`): a brother renamed from another
//! machine would be a word nobody could check. Without a chosen name it is
//! the computer's.
//!
//! `HORADRIC_PC_NAME` still outranks the chosen name, so two dev
//! instances on one machine can be told apart whatever is kept.

use std::path::Path;

/// Beside `brothers.json` in the state folder.
pub const FILE: &str = "name.txt";

/// The longest name kept, in characters. A roster bounds every name to
/// 256 bytes; this leaves room for any script and still fits a menu.
pub const LONGEST: usize = 40;

/// `typed` as a name to keep: trimmed, one line, not too long. None for
/// an empty one, which goes back to the computer's name.
pub fn clean(typed: &str) -> Result<Option<String>, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().any(char::is_control) {
        return Err("a name is one line of plain text".into());
    }
    if name.chars().count() > LONGEST {
        return Err(format!("a name is at most {LONGEST} characters"));
    }
    Ok(Some(name.to_string()))
}

/// The name kept in `dir`, None when none was chosen or it reads wrong.
pub fn chosen(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    clean(&text).ok().flatten()
}

/// Keeps `typed` as this machine's name in `dir`, or forgets the one kept
/// when it is empty. Gives the name kept.
pub fn choose(dir: &Path, typed: &str) -> Result<Option<String>, String> {
    let name = clean(typed)?;
    let path = dir.join(FILE);
    match &name {
        Some(n) => {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            std::fs::write(&path, n).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{}: {e}", path.display()));
            }
            _ => {}
        },
    }
    Ok(name)
}

/// The name to go by: the override, the chosen one, the computer's, then
/// `fallback`, the first that is not blank.
pub fn pick(env: Option<&str>, chosen: Option<&str>, computer: Option<&str>, fallback: &str) -> String {
    [env, chosen, computer]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|n| !n.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_one_short_line_and_blank_is_none() {
        assert_eq!(clean("  Laptop \n"), Ok(Some("Laptop".into())));
        assert_eq!(clean("   "), Ok(None));
        assert!(clean("two\nlines").is_err());
        assert!(clean(&"x".repeat(LONGEST + 1)).is_err());
        assert_eq!(clean(&"ø".repeat(LONGEST)), Ok(Some("ø".repeat(LONGEST))));
    }

    #[test]
    fn the_override_then_the_chosen_then_the_computer() {
        assert_eq!(pick(Some("dev"), Some("Laptop"), Some("DESKTOP-1"), "PC"), "dev");
        assert_eq!(pick(Some(" "), Some("Laptop"), Some("DESKTOP-1"), "PC"), "Laptop");
        assert_eq!(pick(None, None, Some("DESKTOP-1"), "PC"), "DESKTOP-1");
        assert_eq!(pick(None, None, None, "PC"), "PC");
    }

    #[test]
    fn a_chosen_name_is_kept_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("horadric-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(chosen(&dir), None);
        assert_eq!(choose(&dir, " Laptop "), Ok(Some("Laptop".into())));
        assert_eq!(chosen(&dir).as_deref(), Some("Laptop"));
        assert!(choose(&dir, "a\tb").is_err());
        assert_eq!(chosen(&dir).as_deref(), Some("Laptop"), "a refused name changes nothing");
        assert_eq!(choose(&dir, ""), Ok(None));
        assert_eq!(chosen(&dir), None);
        assert_eq!(choose(&dir, ""), Ok(None), "forgetting twice is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
