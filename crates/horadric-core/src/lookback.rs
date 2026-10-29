//! Warriv's look back: while it drives, the first round after 04:00 also
//! reads the last day of the chronicle for what keeps going wrong. A
//! single conflict or a red check is a fix-up quest already; the same
//! thing twice is a pattern, which wants a quest that stops it happening
//! and a rule in Warriv's memory. This finds the patterns, so the round's
//! prompt carries them rather than the chronicle itself.

use std::collections::BTreeMap;

use crate::chronicle::{Command, Happened, Outcome, Record};
use crate::merge::Failure;
use crate::tasks::one_line;
use crate::warriv::Kind;

/// The time of day the look back falls due, in seconds after local
/// midnight: after the night's work, before the human is back.
pub const AT: u64 = 4 * 60 * 60;

/// How far back a look back reads at most, in seconds.
pub const DAY: u64 = 24 * 60 * 60;

/// Something that happened more than once since the last look back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Rebases that stopped on a conflict, `times` in all, on these quests.
    Conflicts { times: usize, quests: Vec<String> },
    /// The check `check` went red `times` in all, on these quests.
    Red {
        check: String,
        times: usize,
        quests: Vec<String>,
    },
    /// Sessions that stopped without reporting done or blocked, `times` in
    /// all, on these quests.
    Silent { times: usize, quests: Vec<String> },
    /// The quest `quest` came back from review `times` times.
    SentBack { quest: String, times: usize },
    /// The file `file` stopped merges `times` times in all, and its
    /// `shape` says what makes it stop.
    Hotspot {
        file: String,
        times: usize,
        shape: Shape,
    },
}

/// What kind of file keeps conflicting, which says what ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A log quests append to: `merge=union` keeps both sides' lines.
    Log,
    /// Code every feature adds an entry to, such as routes, env or a
    /// catalog: one file per feature, gathered by the shared one.
    Registry,
    /// Other prose: union if it is only appended to, else split.
    Doc,
}

/// How many hotspots a look back names at most, the worst first.
const HOTSPOTS: usize = 8;

/// The words in a file's name that make it a log.
const LOGS: [&str; 13] = [
    "plan",
    "log",
    "changelog",
    "changes",
    "history",
    "journal",
    "tracker",
    "progress",
    "notes",
    "decisions",
    "tasks",
    "todo",
    "status",
];

/// The shape of the file at `path`, from its name.
pub fn shape(path: &str) -> Shape {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
    // Whole words, so `catalog.md` or `explanation.md` is no log: union
    // would garble a document edited in place.
    let log = || stem.split(['-', '_', '.', ' ']).any(|w| LOGS.contains(&w));
    if !matches!(ext, "md" | "markdown" | "txt" | "") {
        Shape::Registry
    } else if log() {
        Shape::Log
    } else {
        Shape::Doc
    }
}

/// Whether `path` is a lockfile, which a conflict regenerates rather
/// than splits, so it is never a hotspot.
fn generated(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.ends_with(".lock") || name.ends_with("-lock.json") || name.ends_with("-lock.yaml")
}

/// Whether the project's `.gitattributes` text already merges `file` by
/// union, so naming it again would only repeat the fix. The last line
/// that sets `merge` for the file wins, as in git.
pub fn unioned(attributes: &str, file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let mut union = false;
    for line in attributes.lines() {
        let mut words = line.split_whitespace();
        let Some(pattern) = words.next().filter(|p| !p.starts_with('#')) else {
            continue;
        };
        let matches = match pattern.strip_prefix('/') {
            // Anchored: only the path from the root.
            Some(anchored) => anchored == file,
            None if pattern.contains('/') => pattern == file,
            None => match pattern.strip_prefix('*') {
                Some(tail) => name.ends_with(tail),
                None => pattern == name,
            },
        };
        if !matches {
            continue;
        }
        for w in words {
            if w == "merge=union" {
                union = true;
            } else if w.starts_with("merge=") || w == "-merge" || w == "!merge" || w == "merge" {
                union = false;
            }
        }
    }
    union
}

impl Pattern {
    /// The line the round's prompt gives it.
    pub fn line(&self) -> String {
        match self {
            Pattern::Conflicts { times, quests } => format!(
                "{times} merges stopped on a conflict, on {}: quests are stepping on the same \
                 files. Find out which, and order or split the quests that touch them.",
                list(quests)
            ),
            Pattern::Red {
                check,
                times,
                quests,
            } => format!(
                "The check `{check}` went red {times} times, on {}: it is flaky or something \
                 the quests share is broken. Find out which, and make it pass steadily.",
                list(quests)
            ),
            Pattern::Silent { times, quests } => format!(
                "{times} times a session stopped without reporting done or blocked, on {}: \
                 read what they ended with, and make the quests or the prompt clearer.",
                list(quests)
            ),
            Pattern::SentBack { quest, times } => format!(
                "\"{}\" came back from review {times} times: its quest says too little of \
                 what done means, or the work is harder than it looks.",
                one_line(quest)
            ),
            Pattern::Hotspot {
                file,
                times,
                shape: Shape::Log,
            } => format!(
                "`{file}` stopped {times} merges: it is a log quests append to. Add \
                 `{file} merge=union` to `.gitattributes` and commit it, so both sides' \
                 lines are kept."
            ),
            Pattern::Hotspot {
                file,
                times,
                shape: Shape::Registry,
            } => format!(
                "`{file}` stopped {times} merges: every feature adds its entry to it. File a \
                 quest in this project to split it into one file per feature, which the \
                 shared file only gathers, so parallel quests stop touching the same lines."
            ),
            Pattern::Hotspot {
                file,
                times,
                shape: Shape::Doc,
            } => format!(
                "`{file}` stopped {times} merges. If quests only append to it, add \
                 `{file} merge=union` to `.gitattributes`; if they edit it in place, file a \
                 quest to split it into one file per part."
            ),
        }
    }
}

fn list(quests: &[String]) -> String {
    quests
        .iter()
        .map(|q| format!("\"{}\"", one_line(q)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The start of the latest 04:00 local at or before `now`, `offset` being
/// how far the local clock is ahead of UTC.
fn last_four(now: u64, offset: i64) -> u64 {
    let local = now as i64 + offset - AT as i64;
    now.saturating_sub(local.rem_euclid(DAY as i64) as u64)
}

/// When the project's last look back was told, from the chronicle.
pub fn last(records: &[Record], project: &str) -> Option<u64> {
    records
        .iter()
        .filter(|r| r.project == project)
        .filter(|r| match &r.what {
            Happened::WarrivWoke { events, .. } => events.iter().any(|e| e.kind == Kind::LookBack),
            _ => false,
        })
        .map(|r| r.at)
        .max()
}

/// Whether a look back is due at `now`: none was told since the latest
/// 04:00. Kept in the chronicle, so a reload never brings a second one.
pub fn due(records: &[Record], project: &str, now: u64, offset: i64) -> bool {
    last(records, project).is_none_or(|at| at < last_four(now, offset))
}

/// Where a look back at `now` starts reading: the last one, but no more
/// than a [`DAY`] back.
pub fn since(records: &[Record], project: &str, now: u64) -> u64 {
    let day = now.saturating_sub(DAY);
    last(records, project).map_or(day, |at| at.max(day))
}

/// The patterns in the project's chronicle from `since` on: what happened
/// twice or more. `attributes` is the project's `.gitattributes`, so a
/// file it already merges by union is no hotspot.
pub fn patterns(records: &[Record], project: &str, since: u64, attributes: &str) -> Vec<Pattern> {
    let mine: Vec<&Record> = records.iter().filter(|r| r.project == project).collect();
    let recent = || mine.iter().copied().filter(|r| r.at >= since);
    let mut out = Vec::new();

    let mut conflicts = Tally::default();
    let mut red: BTreeMap<String, Tally> = BTreeMap::new();
    for r in recent() {
        if let Happened::NotMerged { failure, .. } = &r.what {
            match failure {
                Failure::Conflict => conflicts.add(&r.title),
                Failure::Red(check) => red.entry(check.clone()).or_default().add(&r.title),
                _ => {}
            }
        }
    }
    if conflicts.times >= 2 {
        out.push(Pattern::Conflicts {
            times: conflicts.times,
            quests: conflicts.quests,
        });
    }
    for (check, t) in red.into_iter().filter(|(_, t)| t.times >= 2) {
        out.push(Pattern::Red {
            check,
            times: t.times,
            quests: t.quests,
        });
    }

    let mut silent = Tally::default();
    for r in recent() {
        if let Happened::WarrivWoke { events, .. } = &r.what {
            for e in events.iter().filter(|e| e.kind == Kind::Asks) {
                silent.add(&e.quest);
            }
        }
    }
    if silent.times >= 2 {
        out.push(Pattern::Silent {
            times: silent.times,
            quests: silent.quests,
        });
    }

    for (quest, times) in sent_back(&mine, since) {
        out.push(Pattern::SentBack { quest, times });
    }
    out.extend(hotspots(&mine, since, attributes));
    out
}

/// The files that stopped two merges or more over the project's whole
/// story, the last of them since `since`: a hotspot shows slowly, a day
/// is too short to see it, and it stays one until its shape changes.
fn hotspots(mine: &[&Record], since: u64, attributes: &str) -> Vec<Pattern> {
    // Per file: how often, when last, and on which quests. One quest
    // landing twice on the same file is no hotspot, two quests are.
    let mut seen: BTreeMap<&str, (usize, u64, Vec<&str>)> = BTreeMap::new();
    for r in mine {
        if let Happened::NotMerged { files, .. } = &r.what {
            for f in files.iter().filter(|f| !generated(f)) {
                let e = seen.entry(f.as_str()).or_default();
                e.0 += 1;
                e.1 = e.1.max(r.at);
                if !e.2.contains(&r.title.as_str()) {
                    e.2.push(&r.title);
                }
            }
        }
    }
    let mut hot: Vec<(&str, usize)> = seen
        .into_iter()
        .filter(|(f, (_, at, quests))| quests.len() >= 2 && *at >= since && !unioned(attributes, f))
        .map(|(f, (times, ..))| (f, times))
        .collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    hot.into_iter()
        .take(HOTSPOTS)
        .map(|(f, times)| Pattern::Hotspot {
            file: f.to_string(),
            times,
            shape: shape(f),
        })
        .collect()
}

/// How often each quest came back from review, over its whole story, for
/// those that came back since `since` and twice or more. A review sends a
/// quest back with `quest fix`, which Warriv's reviewer runs; a session
/// that takes a quest again after review is one too, which is how the
/// human sends it back. A reviewer's fix to a live session is both, so a
/// quest counts the larger of the two.
fn sent_back(mine: &[&Record], since: u64) -> Vec<(String, usize)> {
    // Per title: fixes, retakes, and when it last came back.
    let mut seen: BTreeMap<&str, (usize, usize, u64)> = BTreeMap::new();
    let mut in_review: BTreeMap<&str, bool> = BTreeMap::new();
    for r in mine {
        let title = r.title.as_str();
        match &r.what {
            Happened::WarrivRan {
                command: Command::Fix,
                ..
            } => {
                let e = seen.entry(title).or_default();
                e.0 += 1;
                e.2 = e.2.max(r.at);
            }
            Happened::Marked { mark, .. } => {
                in_review.insert(title, *mark == Outcome::Review);
            }
            Happened::Accepted { .. } => {
                let reviewed = in_review.insert(title, false) == Some(true);
                if reviewed {
                    let e = seen.entry(title).or_default();
                    e.1 += 1;
                    e.2 = e.2.max(r.at);
                }
            }
            _ => {}
        }
    }
    seen.into_iter()
        .filter(|(t, _)| !t.is_empty())
        .map(|(t, (fixes, retakes, at))| (t, fixes.max(retakes), at))
        .filter(|&(_, times, at)| times >= 2 && at >= since)
        .map(|(t, times, _)| (t.to_string(), times))
        .collect()
}

/// How often something happened, and on which quests, in order.
#[derive(Default)]
struct Tally {
    times: usize,
    quests: Vec<String>,
}

impl Tally {
    fn add(&mut self, quest: &str) {
        self.times += 1;
        if !quest.is_empty() && !self.quests.iter().any(|q| q == quest) {
            self.quests.push(quest.to_string());
        }
    }
}

/// The day in a line, for the round's prompt beside the patterns: how
/// many quests were done and merged, merges that failed, and Warriv's
/// wakes.
pub fn day(records: &[Record], project: &str, since: u64) -> String {
    let recent: Vec<&Record> = records
        .iter()
        .filter(|r| r.project == project && r.at >= since)
        .collect();
    let count = |f: &dyn Fn(&Happened) -> bool| recent.iter().filter(|r| f(&r.what)).count();
    let done = count(&|h| {
        matches!(
            h,
            Happened::Marked {
                mark: Outcome::Done,
                ..
            }
        )
    });
    let merged = count(&|h| matches!(h, Happened::Merged { .. }));
    let failed = count(&|h| matches!(h, Happened::NotMerged { .. }));
    let mut wakes: Vec<&str> = recent
        .iter()
        .filter_map(|r| match &r.what {
            Happened::WarrivWoke { wake, .. } => Some(wake.as_str()),
            _ => None,
        })
        .collect();
    wakes.sort();
    wakes.dedup();
    format!(
        "{} completed, {merged} merged, {failed} did not merge by itself, {}",
        plural(done, "quest"),
        plural(wakes.len(), "Warriv session")
    )
}

/// The conversations Warriv and the errands ever held in the project, so
/// the day's dialogue the look back distills is the work's, not Warriv's
/// own talk about the quest log. Ever, not since the last look back: a
/// wake's conversation may be named only on a line before it, and its
/// talk still be stored since.
pub fn warriv_talk(records: &[Record], project: &str) -> Vec<String> {
    let mut out: Vec<String> = records
        .iter()
        .filter(|r| r.project == project)
        .filter_map(|r| match &r.what {
            Happened::WarrivWoke { conversation, .. }
            | Happened::WarrivRan { conversation, .. }
            | Happened::WarrivSlept { conversation, .. }
            | Happened::ErrandEnded { conversation, .. } => Some(conversation.clone()),
            _ => None,
        })
        .filter(|c| !c.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn plural(n: usize, word: &str) -> String {
    match n {
        1 => format!("1 {word}"),
        n => format!("{n} {word}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::Woken;

    fn rec(at: u64, title: &str, what: Happened) -> Record {
        Record {
            at,
            project: "p".into(),
            quest: format!("q-{title}"),
            title: title.into(),
            what,
        }
    }

    fn not_merged(at: u64, title: &str, failure: Failure) -> Record {
        rec(
            at,
            title,
            Happened::NotMerged {
                branch: title.into(),
                failure,
                files: Vec::new(),
            },
        )
    }

    fn conflicted(at: u64, title: &str, files: &[&str]) -> Record {
        rec(
            at,
            title,
            Happened::NotMerged {
                branch: title.into(),
                failure: Failure::Conflict,
                files: files.iter().map(|f| f.to_string()).collect(),
            },
        )
    }

    fn woke(at: u64, kinds: &[(Kind, &str)]) -> Record {
        rec(
            at,
            "",
            Happened::WarrivWoke {
                wake: format!("warriv-{at}"),
                conversation: String::new(),
                events: kinds
                    .iter()
                    .map(|(k, q)| Woken {
                        kind: *k,
                        quest: q.to_string(),
                    })
                    .collect(),
            },
        )
    }

    fn marked(at: u64, title: &str, mark: Outcome) -> Record {
        rec(
            at,
            title,
            Happened::Marked {
                mark,
                reason: String::new(),
            },
        )
    }

    fn accepted(at: u64, title: &str) -> Record {
        rec(at, title, Happened::Accepted { notes: Vec::new() })
    }

    fn fix(at: u64, title: &str) -> Record {
        rec(
            at,
            title,
            Happened::WarrivRan {
                wake: "warriv-review".into(),
                conversation: String::new(),
                command: Command::Fix,
                text: "no tests".into(),
            },
        )
    }

    #[test]
    fn due_once_a_day_from_four_in_the_morning() {
        // Local is UTC+2; 2026-10-05 00:00 UTC is 02:00 local.
        let midnight = 1_791_158_400;
        let offset = 2 * 3600;
        let at = |h: u64| midnight + h * 3600 - 2 * 3600;
        assert!(due(&[], "p", at(3), offset));
        let looked = [woke(at(4) + 60, &[(Kind::Round, ""), (Kind::LookBack, "")])];
        assert!(!due(&looked, "p", at(5), offset));
        assert!(!due(&looked, "p", at(27), offset), "03:00 the next day");
        assert!(due(&looked, "p", at(28), offset), "04:00 the next day");
        assert!(due(&looked, "other", at(5), offset));
        // A round alone is no look back.
        let round = [woke(at(5), &[(Kind::Round, "")])];
        assert!(due(&round, "p", at(6), offset));
    }

    #[test]
    fn reads_from_the_last_look_back_at_most_a_day() {
        assert_eq!(since(&[], "p", 100_000), 100_000 - DAY);
        let looked = [woke(90_000, &[(Kind::LookBack, "")])];
        assert_eq!(since(&looked, "p", 100_000), 90_000);
        assert_eq!(since(&looked, "p", 200_000), 200_000 - DAY);
    }

    #[test]
    fn one_of_anything_is_no_pattern() {
        let records = [
            not_merged(10, "A", Failure::Conflict),
            not_merged(11, "B", Failure::Red("cargo test".into())),
            woke(12, &[(Kind::Asks, "C")]),
            fix(13, "D"),
        ];
        assert!(patterns(&records, "p", 0, "").is_empty());
    }

    #[test]
    fn repeated_conflicts_and_a_check_red_twice() {
        let records = [
            not_merged(10, "A", Failure::Conflict),
            not_merged(11, "B", Failure::Conflict),
            not_merged(12, "A", Failure::Conflict),
            not_merged(13, "C", Failure::Red("cargo test".into())),
            not_merged(14, "D", Failure::Red("cargo test".into())),
            not_merged(15, "E", Failure::Red("cargo clippy".into())),
            not_merged(16, "F", Failure::Refused),
        ];
        assert_eq!(
            patterns(&records, "p", 0, ""),
            vec![
                Pattern::Conflicts {
                    times: 3,
                    quests: vec!["A".into(), "B".into()]
                },
                Pattern::Red {
                    check: "cargo test".into(),
                    times: 2,
                    quests: vec!["C".into(), "D".into()]
                },
            ]
        );
    }

    #[test]
    fn only_since_and_only_this_project() {
        let mut other = not_merged(20, "B", Failure::Conflict);
        other.project = "q".into();
        let records = [
            not_merged(5, "A", Failure::Conflict),
            not_merged(20, "A", Failure::Conflict),
            other,
        ];
        assert!(patterns(&records, "p", 10, "").is_empty());
        assert_eq!(patterns(&records, "p", 0, "").len(), 1);
    }

    #[test]
    fn sessions_that_stopped_without_reporting() {
        let records = [
            woke(10, &[(Kind::Asks, "A"), (Kind::Round, "")]),
            woke(20, &[(Kind::Asks, "B")]),
        ];
        assert_eq!(
            patterns(&records, "p", 0, ""),
            vec![Pattern::Silent {
                times: 2,
                quests: vec!["A".into(), "B".into()]
            }]
        );
    }

    #[test]
    fn a_quest_sent_back_twice() {
        // The reviewer's fixes, with a live session taking it again each time.
        let records = [
            marked(10, "A", Outcome::Review),
            fix(11, "A"),
            accepted(12, "A"),
            marked(13, "A", Outcome::Review),
            fix(14, "A"),
            accepted(15, "A"),
            // Taken again after a review, by the human, once only.
            marked(16, "B", Outcome::Review),
            accepted(17, "B"),
            // Blocked and taken again is no review.
            marked(18, "C", Outcome::Blocked),
            accepted(19, "C"),
            marked(20, "C", Outcome::Blocked),
            accepted(21, "C"),
        ];
        assert_eq!(
            patterns(&records, "p", 0, ""),
            vec![Pattern::SentBack {
                quest: "A".into(),
                times: 2
            }]
        );
        // Sent back before and once since still counts both.
        assert_eq!(patterns(&records, "p", 13, "").len(), 1);
        assert!(patterns(&records, "p", 16, "").is_empty());
    }

    #[test]
    fn retakes_by_the_human_count_too() {
        let records = [
            marked(10, "A", Outcome::Review),
            accepted(11, "A"),
            marked(12, "A", Outcome::Review),
            accepted(13, "A"),
        ];
        assert_eq!(
            patterns(&records, "p", 0, ""),
            vec![Pattern::SentBack {
                quest: "A".into(),
                times: 2
            }]
        );
    }

    #[test]
    fn files_that_keep_conflicting_are_hotspots() {
        let records = [
            conflicted(1, "A", &["docs/PLAN.md", "src/env.ts", "docs/SPEC.md"]),
            conflicted(2, "B", &["docs/PLAN.md", "src/env.ts"]),
            conflicted(3, "C", &["src/env.ts", "docs/SPEC.md", "once.ts"]),
            // An old pair that never came back is no hotspot today.
            conflicted(1, "D", &["old.ts"]),
            conflicted(1, "E", &["old.ts"]),
        ];
        let hot = |since: u64, attributes: &str| -> Vec<Pattern> {
            patterns(&records, "p", since, attributes)
                .into_iter()
                .filter(|p| matches!(p, Pattern::Hotspot { .. }))
                .collect()
        };
        let spot = |file: &str, times, shape| Pattern::Hotspot {
            file: file.into(),
            times,
            shape,
        };
        assert_eq!(
            hot(2, ""),
            [
                spot("src/env.ts", 3, Shape::Registry),
                spot("docs/PLAN.md", 2, Shape::Log),
                spot("docs/SPEC.md", 2, Shape::Doc),
            ]
        );
        assert_eq!(
            hot(2, "docs/PLAN.md merge=union\n*.ts text=auto\n"),
            [
                spot("src/env.ts", 3, Shape::Registry),
                spot("docs/SPEC.md", 2, Shape::Doc),
            ]
        );
        assert_eq!(hot(4, ""), []);
        assert_eq!(
            hot(2, "docs/PLAN.md merge=union\ndocs/PLAN.md -merge\n").len(),
            3,
            "the last line wins"
        );
        // One quest twice, or a lockfile, is no hotspot.
        let records = [
            conflicted(1, "A", &["src/a.ts", "Cargo.lock"]),
            conflicted(2, "A", &["src/a.ts"]),
            conflicted(3, "B", &["Cargo.lock"]),
        ];
        assert!(!patterns(&records, "p", 0, "")
            .iter()
            .any(|p| matches!(p, Pattern::Hotspot { .. })));
        assert!(spot("docs/PLAN.md", 2, Shape::Log)
            .line()
            .contains("`docs/PLAN.md merge=union`"));
    }

    #[test]
    fn a_files_shape_comes_from_its_name() {
        assert_eq!(shape("docs/PLAN.md"), Shape::Log);
        assert_eq!(shape("LOG.md"), Shape::Log);
        assert_eq!(shape("CHANGELOG.md"), Shape::Log);
        assert_eq!(shape("docs/bug-tracker.md"), Shape::Log);
        assert_eq!(shape("docs/SPEC.md"), Shape::Doc);
        assert_eq!(shape("docs/catalog.md"), Shape::Doc);
        assert_eq!(shape("explanation.md"), Shape::Doc);
        assert_eq!(shape("apps/server/src/env.ts"), Shape::Registry);
        assert_eq!(shape("content/pages/modules.ts"), Shape::Registry);
        assert_eq!(shape("src\\Footer.tsx"), Shape::Registry);
    }

    #[test]
    fn union_in_gitattributes_by_path_name_or_glob() {
        let attributes = "# logs\n/docs/PLAN.md merge=union\nLOG.md merge=union\n\
                          *.log merge=union\nSPEC.md text\n";
        assert!(unioned(attributes, "docs/PLAN.md"));
        assert!(!unioned("/PLAN.md merge=union", "docs/PLAN.md"), "anchored");
        assert!(unioned("/PLAN.md merge=union", "PLAN.md"));
        assert!(!unioned(attributes, "other/PLAN.md"));
        assert!(unioned(attributes, "a/b/LOG.md"));
        assert!(unioned(attributes, "build.log"));
        assert!(!unioned(attributes, "docs/SPEC.md"));
        assert!(!unioned("", "docs/PLAN.md"));
    }

    #[test]
    fn each_pattern_reads_as_a_line() {
        let red = Pattern::Red {
            check: "cargo test".into(),
            times: 2,
            quests: vec!["C".into(), "D".into()],
        };
        assert_eq!(
            red.line(),
            "The check `cargo test` went red 2 times, on \"C\", \"D\": it is flaky or something \
             the quests share is broken. Find out which, and make it pass steadily."
        );
        let back = Pattern::SentBack {
            quest: "A".into(),
            times: 2,
        };
        assert!(back
            .line()
            .starts_with("\"A\" came back from review 2 times"));
    }

    #[test]
    fn warrivs_own_conversations_ever() {
        let mut a = woke(10, &[(Kind::Round, "")]);
        if let Happened::WarrivWoke { conversation, .. } = &mut a.what {
            *conversation = "C:/t/w1.jsonl".into();
        }
        let mut b = fix(20, "A");
        if let Happened::WarrivRan { conversation, .. } = &mut b.what {
            *conversation = "C:/t/w2.jsonl".into();
        }
        let mut old = a.clone();
        old.at = 1;
        if let Happened::WarrivWoke { conversation, .. } = &mut old.what {
            *conversation = "C:/t/w0.jsonl".into();
        }
        let records = [a.clone(), a, b, old, woke(30, &[(Kind::Round, "")])];
        assert_eq!(
            warriv_talk(&records, "p"),
            ["C:/t/w0.jsonl", "C:/t/w1.jsonl", "C:/t/w2.jsonl"]
        );
        assert!(warriv_talk(&records, "q").is_empty());
    }

    #[test]
    fn the_day_in_a_line() {
        let records = [
            marked(10, "A", Outcome::Done),
            rec(
                11,
                "A",
                Happened::Merged {
                    branch: "a".into(),
                    checked: String::new(),
                },
            ),
            not_merged(12, "B", Failure::Conflict),
            woke(13, &[(Kind::Round, "")]),
            woke(14, &[(Kind::Asks, "C")]),
        ];
        assert_eq!(
            day(&records, "p", 0),
            "1 quest completed, 1 merged, 1 did not merge by itself, 2 Warriv sessions"
        );
    }
}
