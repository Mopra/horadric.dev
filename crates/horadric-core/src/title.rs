//! The title Claude Code gives a conversation, as its transcript records it.
//!
//! After the first prompt Claude Code writes an `ai-title` line into the
//! transcript and repeats it every few turns. `/rename` writes a
//! `custom-title` line instead, and from then on only that one. Both are
//! structured records Claude Code keeps for itself, not terminal output.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    /// Set by the human with `/rename`, not made up by Claude.
    pub custom: bool,
}

/// The newest title in a stretch of transcript, a `/rename` before
/// anything Claude made up. The stretch may start mid line: a line that is
/// not whole JSON is skipped.
pub fn latest(transcript: &[u8]) -> Option<Title> {
    let mut ai = None;
    let mut custom = None;
    for line in transcript.split(|&b| b == b'\n') {
        // Most lines are tool output, some of them megabytes. Only a line
        // that mentions a title is worth parsing.
        if !contains(line, b"-title\"") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let text = |key: &str| {
            v.get(key)
                .and_then(|t| t.as_str())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => ai = text("aiTitle").or(ai),
            Some("custom-title") => custom = text("customTitle").or(custom),
            _ => {}
        }
    }
    custom
        .map(|text| Title { text, custom: true })
        .or(ai.map(|text| Title {
            text,
            custom: false,
        }))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_ai_title_wins() {
        let t = br#"{"type":"user","message":"hi"}
{"type":"ai-title","aiTitle":"First guess","sessionId":"a"}
{"type":"assistant","message":"ok"}
{"type":"ai-title","aiTitle":"Session naming from chat","sessionId":"a"}
"#;
        assert_eq!(
            latest(t),
            Some(Title {
                text: "Session naming from chat".into(),
                custom: false
            })
        );
    }

    #[test]
    fn a_rename_beats_the_made_up_title() {
        let t = br#"{"type":"custom-title","customTitle":"Changelog","sessionId":"a"}
{"type":"ai-title","aiTitle":"Something else","sessionId":"a"}"#;
        assert_eq!(
            latest(t),
            Some(Title {
                text: "Changelog".into(),
                custom: true
            })
        );
    }

    #[test]
    fn a_cut_line_or_no_title_reads_as_none() {
        assert_eq!(latest(br#"Title":"half","sessionId":"a"}"#), None);
        assert_eq!(latest(b"{\"type\":\"user\"}\n"), None);
        assert_eq!(latest(br#"{"type":"ai-title","aiTitle":"  "}"#), None);
        assert_eq!(latest(b""), None);
    }

    #[test]
    fn a_prompt_that_mentions_titles_is_not_one() {
        let t = br#"{"type":"user","message":"grep for \"type\":\"ai-title\" please"}"#;
        assert_eq!(latest(t), None);
    }
}
