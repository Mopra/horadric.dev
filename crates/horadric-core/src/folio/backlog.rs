//! What a session's host kept, read into a Folio after a reload.
//!
//! A host keeps the lines since its claude started, up to a cap. Past it
//! the oldest go, and the backlog starts somewhere in the middle of the
//! conversation. The transcript on disk still has the whole of it, so
//! such a Folio reads the transcript first and then the backlog, minus
//! what the transcript already holds.
//!
//! The two line up by uuid: the stream's `assistant` and `user` messages
//! carry the uuid their transcript line has (checked on 2.1.312 against
//! the recorded pairs in tests/fixtures/headless). The last backlog line
//! the transcript knows is the cut. Before it, the messages, their
//! streams, the turns' results and the asks already answered are the
//! transcript's, and only what they did to the session's state is kept
//! (its init, mode, tasks, context, whether a turn runs). After it, every
//! line goes in as it came.

use std::collections::HashSet;

use serde_json::Value;

use super::{Block, Folio};

/// One line a session's host kept, as it hands them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// claude wrote it to stdout.
    Out(String),
    /// A client sent it to claude's stdin.
    Sent(String),
}

/// What a backlog line is to the merge.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    /// The conversation itself: a message of the main thread, its stream,
    /// a prompt. The transcript has these.
    Message,
    /// A stream's start, which says which message the blocks after it
    /// belong to: kept even where the transcript has its blocks, so a
    /// block that streams on past the cut is still claimed by its copy.
    MessageStart,
    Prompt,
    /// A turn's end.
    Result,
    /// An ask, by request id.
    Ask(String),
    /// An answer to one, by request id.
    Answer(String),
    /// Anything else: state, a subagent's lane, bookkeeping.
    Other,
}

fn part(line: &Kept) -> (Part, Option<String>) {
    let (text, sent) = match line {
        Kept::Out(t) => (t, false),
        Kept::Sent(t) => (t, true),
    };
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
        return (Part::Other, None);
    };
    let id = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let kind = v["type"].as_str().unwrap_or_default();
    let part = match (sent, kind) {
        (true, "user") => Part::Prompt,
        (true, "control_response") => Part::Answer(id(&v["response"]["request_id"])),
        (true, _) => Part::Other,
        (false, _) if v["parent_tool_use_id"].is_string() => Part::Other,
        (false, "stream_event") if v["event"]["type"] == "message_start" => Part::MessageStart,
        (false, "assistant" | "user" | "stream_event") => Part::Message,
        (false, "result") => Part::Result,
        (false, "control_request") => Part::Ask(id(&v["request_id"])),
        (false, "control_cancel_request") => Part::Answer(id(&v["request_id"])),
        _ => Part::Other,
    };
    // A stream event's uuid is its own, never a transcript line's.
    let whole = matches!(part, Part::Prompt) || matches!(kind, "assistant" | "user");
    let uuid = (v["uuid"].as_str().filter(|_| whole)).map(str::to_string);
    (part, uuid)
}

impl Folio {
    /// Feeds what a host kept, in its order.
    pub fn feed_kept(&mut self, lines: &[Kept]) {
        for line in lines {
            match line {
                Kept::Out(t) => self.feed(t),
                Kept::Sent(t) => self.feed_sent(t),
            };
        }
    }

    /// The conversation in the transcript `jsonl`, up to the line `at` as
    /// [`Folio::from_transcript_at`] reads it, then the backlog `lines`
    /// that lost its start, minus what the transcript holds. None when
    /// the two do not line up: a backlog with messages and none of them
    /// in the transcript is not this transcript's, or a CLI whose uuids
    /// differ, and reading both would show its turns twice.
    pub fn from_transcript_and_backlog(
        jsonl: &str,
        at: Option<&str>,
        lines: &[Kept],
    ) -> Option<Folio> {
        let known: HashSet<String> = (jsonl.lines())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["uuid"].as_str().map(str::to_string))
            .collect();
        let parts: Vec<(Part, Option<String>)> = lines.iter().map(part).collect();
        let in_transcript = |(p, uuid): &(Part, Option<String>)| {
            matches!(p, Part::Message | Part::Prompt)
                && uuid.as_ref().is_some_and(|u| known.contains(u))
        };
        let cut = parts.iter().rposition(in_transcript);
        let has_messages = (parts.iter()).any(|(p, u)| *p == Part::Message && u.is_some());
        if cut.is_none() && has_messages {
            return None;
        }
        let mut f = Folio::new();
        f.replay_open(jsonl, false, at);
        // Whether the last turn still runs, as the backlog's last word on
        // it says. With none, the transcript's last turn is over.
        let running = (parts.iter().rev()).find_map(|(p, _)| match p {
            Part::Prompt | Part::Message => Some(true),
            Part::Result => Some(false),
            _ => None,
        });
        if running.is_none() {
            f.settle_turn();
        }
        let Some(cut) = cut else {
            f.feed_kept(lines);
            return Some(f);
        };
        let answered: HashSet<&str> = (parts.iter())
            .filter_map(|(p, _)| match p {
                Part::Answer(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // Prompts sent after the cut that the transcript wrote already:
        // ours carry no uuid to tell them by, but they are its last ones.
        let mut written = match (at, parts[cut].1.as_deref()) {
            (None, Some(u)) => prompts(&f).saturating_sub(prompts(
                &Folio::from_transcript_at(jsonl, Some(u)),
            )),
            _ => 0,
        };
        let mut skipped: HashSet<&str> = HashSet::new();
        for (i, (line, (p, _))) in lines.iter().zip(&parts).enumerate() {
            if i > cut {
                if *p == Part::Prompt && written > 0 {
                    written -= 1;
                    f.busy = true;
                    continue;
                }
                f.feed_kept(std::slice::from_ref(line));
                continue;
            }
            match p {
                Part::Message => {
                    f.busy = true;
                    continue;
                }
                Part::Prompt => {
                    f.busy = true;
                    continue;
                }
                Part::Ask(id) if answered.contains(id.as_str()) => {
                    skipped.insert(id);
                    continue;
                }
                Part::Answer(id) if skipped.contains(id.as_str()) => continue,
                _ => {}
            }
            // Its state, not its blocks: the transcript has those, or they
            // were notes of the moment (a turn's footer, a retry).
            let had = f.blocks.len();
            f.feed_kept(std::slice::from_ref(line));
            f.blocks.truncate(had);
        }
        Some(f)
    }
}

fn prompts(f: &Folio) -> usize {
    (f.blocks.iter())
        .filter(|b| matches!(b, Block::Prompt(_)))
        .count()
}

/// The conversation a backlog carries on at its end, as its latest line
/// that names one says. Nearly every line does.
pub fn conversation_in(lines: &[Kept]) -> Option<String> {
    lines.iter().rev().find_map(|l| match l {
        Kept::Out(t) if t.contains(r#""session_id""#) => serde_json::from_str::<Value>(t)
            .ok()
            .and_then(|v| v["session_id"].as_str().map(str::to_string))
            .filter(|s| !s.is_empty()),
        _ => None,
    })
}

/// Whether a `/clear` is in the backlog: what came before it is off the
/// page anyway, so a backlog that lost its start still holds all of what
/// shows.
pub fn cleared_in(lines: &[Kept]) -> bool {
    lines.iter().any(|l| match l {
        Kept::Out(t) if t.contains("conversation_reset") => {
            serde_json::from_str::<Value>(t).is_ok_and(|v| v["type"] == "conversation_reset")
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITS: &str = include_str!("../../tests/fixtures/headless/edits.jsonl");
    const TRANSCRIPT_EDITS: &str =
        include_str!("../../tests/fixtures/headless/transcript_edits.jsonl");
    const SUBAGENT: &str = include_str!("../../tests/fixtures/headless/subagent.jsonl");
    const TRANSCRIPT_SUBAGENT: &str =
        include_str!("../../tests/fixtures/headless/transcript_subagent.jsonl");

    fn out(stream: &str) -> Vec<Kept> {
        stream.lines().map(|l| Kept::Out(l.to_string())).collect()
    }

    /// The transcript as it stood when the stream ended: the recorded
    /// one goes on with a later resume.
    fn upto_stream(transcript: &str, stream: &str) -> String {
        let seen: HashSet<String> = (stream.lines())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["uuid"].as_str().map(str::to_string))
            .collect();
        let lines: Vec<&str> = transcript.lines().collect();
        let last = (lines.iter())
            .rposition(|l| {
                let v: Value = serde_json::from_str(l).unwrap_or_default();
                v["uuid"].as_str().is_some_and(|u| seen.contains(u))
            })
            .unwrap();
        lines[..=last].join("\n")
    }

    /// What a Folio that saw it all shows: the prompt the stream never
    /// echoes, then the stream.
    fn whole(transcript: &str, stream: &str) -> Folio {
        let prompt = Folio::from_transcript(transcript);
        let Some(Block::Prompt(p)) = prompt.blocks.first() else {
            panic!("no prompt first");
        };
        let mut f = Folio::new();
        f.push_prompt(p);
        f.feed_kept(&out(stream));
        f
    }

    /// The blocks but footers, the merged turn's own as well. A step's
    /// input from its whole copy or the transcript has the tool's defaults
    /// filled in, which the input streamed bit by bit lacks: the same step.
    ///
    /// A subagent's steps are only in its stream (the session's transcript
    /// leaves sidechains out), and so is how far it got: a backlog that lost
    /// them shows its lane without them, so they are left out here.
    fn shown(f: &Folio) -> String {
        let blocks: Vec<Block> = (f.blocks.iter())
            .filter(|b| !matches!(b, Block::Footer { .. }))
            .cloned()
            .map(|mut b| {
                if let Block::Tools(steps) = &mut b {
                    // A transcript knows when each step ran, a backlog not.
                    for s in steps.iter_mut() {
                        (s.began, s.ended) = (None, None);
                    }
                    for lane in steps.iter_mut().filter_map(|s| s.lane.as_mut()) {
                        lane.steps.clear();
                        lane.progress.clear();
                        lane.task_id.clear();
                        (lane.tokens, lane.tool_uses) = (0, 0);
                    }
                }
                b
            })
            .collect();
        format!("{blocks:?}").replace(r#", "replace_all": Bool(false)"#, "")
    }

    #[test]
    fn a_backlog_that_lost_its_start_reads_on_from_the_transcript() {
        for (stream, transcript) in [(EDITS, TRANSCRIPT_EDITS), (SUBAGENT, TRANSCRIPT_SUBAGENT)] {
            let transcript = upto_stream(transcript, stream);
            let want = whole(&transcript, stream);
            let all = out(stream);
            // Cut anywhere past the init, mid stream included.
            for lost in 9..all.len() {
                let f = Folio::from_transcript_and_backlog(&transcript, None, &all[lost..])
                    .unwrap_or_else(|| panic!("lined up with {lost} lost"));
                assert_eq!(shown(&f), shown(&want), "{lost} lost");
                assert!(!f.busy, "{lost} lost");
                assert_eq!(f.session_id, want.session_id);
            }
        }
    }

    #[test]
    fn a_turn_still_running_past_the_cut_stays_busy_and_streams_on() {
        // Up to a tool's input streaming, the step before it done.
        let stop = 45;
        let so_far = EDITS.lines().take(stop).collect::<Vec<_>>().join("\n");
        let transcript = upto_stream(TRANSCRIPT_EDITS, &so_far);
        let all = out(EDITS);
        let mut f =
            Folio::from_transcript_and_backlog(&transcript, None, &all[20..stop]).unwrap();
        let mut want = whole(&transcript, &so_far);
        assert_eq!(shown(&f), shown(&want));
        assert!(f.busy && want.busy);
        f.feed_kept(&all[stop..]);
        want.feed_kept(&all[stop..]);
        assert_eq!(shown(&f), shown(&want), "and it streams on");
        assert!(!f.busy);
    }

    #[test]
    fn a_prompt_after_the_cut_the_transcript_has_shows_once() {
        let transcript = upto_stream(TRANSCRIPT_EDITS, EDITS);
        let mut lines = out(EDITS)[40..].to_vec();
        let mut longer = transcript.clone();
        longer.push('\n');
        longer.push_str(
            &serde_json::json!({"type": "user", "uuid": "p2", "parentUuid": null,
                "message": {"role": "user", "content": "and again"}})
            .to_string(),
        );
        lines.push(Kept::Sent(crate::folio::send::prompt_line("and again")));
        let f = Folio::from_transcript_and_backlog(&longer, None, &lines).unwrap();
        let again = (f.blocks.iter())
            .filter(|b| matches!(b, Block::Prompt(p) if p == "and again"))
            .count();
        assert_eq!(again, 1);
        assert!(f.busy);
    }

    #[test]
    fn a_backlog_of_another_conversation_does_not_line_up() {
        let lines = out(EDITS)[20..].to_vec();
        assert!(Folio::from_transcript_and_backlog(TRANSCRIPT_SUBAGENT, None, &lines).is_none());
        let quiet = vec![Kept::Out(r#"{"type":"system","subtype":"status"}"#.into())];
        assert!(Folio::from_transcript_and_backlog(TRANSCRIPT_SUBAGENT, None, &quiet).is_some());
    }

    #[test]
    fn a_stream_without_its_finished_deltas_shows_the_same() {
        // What a host keeps once it let them go, see horadric-pty's LineLog.
        let pruned: String = (EDITS.lines())
            .filter(|l| !l.contains(r#""type":"content_block_delta""#))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = Folio::new();
        a.feed_kept(&out(EDITS));
        let mut b = Folio::new();
        b.feed_kept(&out(&pruned));
        assert_eq!(shown(&a), shown(&b));
    }

    #[test]
    fn the_conversation_and_a_clear_are_found_in_a_backlog() {
        assert_eq!(
            conversation_in(&out(EDITS)).as_deref(),
            Some("4792f003-15a6-47f5-8cdd-efd07abf2ea4")
        );
        assert_eq!(conversation_in(&[Kept::Sent("{}".into())]), None);
        assert!(!cleared_in(&out(EDITS)));
        let clear = include_str!("../../tests/fixtures/headless/clear.jsonl");
        assert!(cleared_in(&out(clear)));
    }
}
