//! Reads a conversation's title out of its transcript file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use horadric_core::{title, Title};

/// Claude Code repeats the title every few turns, so the end of the file
/// nearly always has it. A transcript is megabytes by the end of a long day.
const TAIL: u64 = 256 * 1024;

/// A file bigger than this is not read whole when its tail has no title.
const WHOLE: u64 = 64 * 1024 * 1024;

/// The conversation's title, or `None` when it has none yet or the file
/// can not be read.
pub fn title(path: &str) -> Option<Title> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let found = read_from(&mut file, len.saturating_sub(TAIL)).and_then(|b| title::latest(&b));
    if found.is_some() || len <= TAIL || len > WHOLE {
        return found;
    }
    read_from(&mut file, 0).and_then(|b| title::latest(&b))
}

fn read_from(file: &mut File, at: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str, lines: &[String]) -> String {
        let path =
            std::env::temp_dir().join(format!("horadric-test-{name}-{}.jsonl", std::process::id()));
        let mut f = File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        path.to_string_lossy().to_string()
    }

    #[test]
    fn finds_a_title_far_behind_a_huge_tool_result() {
        let big = format!(
            r#"{{"type":"user","content":"{}"}}"#,
            "x".repeat(TAIL as usize * 2)
        );
        let path = scratch(
            "far",
            &[r#"{"type":"ai-title","aiTitle":"Early title"}"#.into(), big],
        );
        assert_eq!(title(&path).map(|t| t.text).as_deref(), Some("Early title"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_file_has_no_title() {
        assert_eq!(title("C:/nowhere/at/all.jsonl"), None);
        assert_eq!(title(""), None);
    }
}
