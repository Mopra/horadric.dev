//! What happened to sessions and items, for the quest log's window to fill
//! in what the chronicle never heard of. The app appends a line to
//! `journal.jsonl` for a turn finished and an item taken, sent for
//! review, blocked, finished or merged.

use serde::{Deserialize, Serialize};

use crate::tasks::{Mark, Task};

/// The file, beside `state.json`.
pub const FILE: &str = "journal.jsonl";

/// How long lines are kept, in seconds.
pub const KEEP: u64 = 7 * 86_400;

/// One thing worth telling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When, in Unix seconds.
    pub at: u64,
    /// The Horadric id of the session it is about, empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// The session's name as its tile showed it then.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The project key, empty for what is the account's.
    #[serde(default)]
    pub project: String,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum What {
    /// The session finished its turn, and its last message.
    Done {
        #[serde(default)]
        line: String,
    },
    /// A session took a task item.
    Started { title: String },
    /// An item's agent says it is done and wants a look.
    Review { title: String },
    Blocked {
        title: String,
        #[serde(default)]
        reason: String,
    },
    /// An item was marked done, and the commits made under it, newest
    /// first.
    Finished {
        title: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commits: Vec<Commit>,
    },
    /// A finished item's branch went into the main tree.
    Merged { branch: String, title: String },
}

impl Entry {
    /// The line the file holds for it, newline included.
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// Every line of the file that reads. A torn last line, from a crash mid
/// write, is left out rather than losing the rest.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The file without the lines older than [`KEEP`], or None when none are.
pub fn trimmed(text: &str, now: u64) -> Option<String> {
    let oldest = now.saturating_sub(KEEP);
    let entries = parse(text);
    let stale = entries.iter().any(|e| e.at < oldest);
    let torn = text.lines().filter(|l| !l.trim().is_empty()).count() != entries.len();
    (stale || torn).then(|| {
        entries
            .iter()
            .filter(|e| e.at >= oldest)
            .map(Entry::line)
            .collect()
    })
}

/// A commit, as `git log` told it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The short hash.
    pub hash: String,
    pub subject: String,
}

/// The format [`commits`] reads: `git log --format=` this.
pub const LOG_FORMAT: &str = "%h%x09%s";

/// The commits in what `git log --format=`[`LOG_FORMAT`] printed.
pub fn commits(log: &str) -> Vec<Commit> {
    log.lines()
        .filter_map(|l| {
            let (hash, subject) = l.split_once('\t')?;
            let hash = hash.trim();
            (!hash.is_empty()).then(|| Commit {
                hash: hash.to_string(),
                subject: subject.trim().to_string(),
            })
        })
        .collect()
}

/// When the item `title` of `project` was last taken, which is where its
/// commits begin. None when the journal never heard it taken.
pub fn started_at(entries: &[Entry], project: &str, title: &str) -> Option<u64> {
    entries
        .iter()
        .rev()
        .find(|e| {
            e.project == project && matches!(&e.what, What::Started { title: t } if t == title)
        })
        .map(|e| e.at)
}

/// The lines for what changed between two reads of a project's task list:
/// an item taken, sent for review, blocked or finished. Items are matched
/// by title, since lines move as the list is edited.
pub fn marks(project: &str, old: &[Task], new: &[Task], now: u64) -> Vec<Entry> {
    new.iter()
        .filter_map(|t| {
            let before = old.iter().find(|o| o.title == t.title)?;
            if before.mark == t.mark {
                return None;
            }
            let title = t.title.clone();
            let what = match t.mark {
                Mark::Working => What::Started { title },
                Mark::Review => What::Review { title },
                Mark::Blocked => What::Blocked {
                    title,
                    reason: t.reason.clone().unwrap_or_default(),
                },
                Mark::Done => What::Finished {
                    title,
                    commits: Vec::new(),
                },
                Mark::Open => return None,
            };
            Some(Entry {
                at: now,
                session: t
                    .holder
                    .clone()
                    .or_else(|| before.holder.clone())
                    .unwrap_or_default(),
                name: String::new(),
                project: project.to_string(),
                what,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, session: &str, project: &str, what: What) -> Entry {
        Entry {
            at,
            session: session.to_string(),
            name: session.to_uppercase(),
            project: project.to_string(),
            what,
        }
    }

    #[test]
    fn a_line_reads_back_and_a_torn_one_is_left_out() {
        let done = What::Done {
            line: "All green.".into(),
        };
        let e = entry(5, "a", "c:/p", done);
        let text = format!("{}{{\"at\":6,\"wha", e.line());
        assert_eq!(parse(&text), vec![e]);
        let merged = What::Merged {
            branch: "add-x".into(),
            title: "Add x".into(),
        };
        let merged = entry(7, "", "p", merged);
        assert!(!merged.line().contains("session"));
        assert_eq!(parse(&merged.line()), vec![merged]);
        let commit = Commit {
            hash: "1".into(),
            subject: "Add x".into(),
        };
        let finished = |commits: Vec<Commit>| What::Finished {
            title: "Add x".into(),
            commits,
        };
        let e = entry(1, "a", "p", finished(vec![commit]));
        assert_eq!(parse(&e.line()), vec![e]);
        assert!(!entry(1, "a", "p", finished(Vec::new()))
            .line()
            .contains("commits"));
    }

    #[test]
    fn trimming_drops_the_lines_older_than_a_week() {
        let now = KEEP + 100;
        let started = What::Started { title: "A".into() };
        let old = entry(50, "a", "p", started.clone());
        let new = entry(150, "b", "p", started);
        let text = old.line() + &new.line();
        assert_eq!(trimmed(&text, now), Some(new.line()));
        assert_eq!(trimmed(&new.line(), now), None);
        assert_eq!(trimmed(&(new.line() + "{torn"), now), Some(new.line()));
    }

    #[test]
    fn a_changed_mark_is_a_line_and_an_unchanged_or_new_item_is_not() {
        let task = |line: usize, mark: Mark, title: &str, holder: Option<&str>| Task {
            line,
            mark,
            title: title.to_string(),
            holder: holder.map(str::to_string),
            reason: (mark == Mark::Blocked).then(|| "needs keys".to_string()),
            wait: None,
            since: None,
            ask: None,
            notes: Vec::new(),
        };
        let old = [
            task(0, Mark::Open, "One", None),
            task(1, Mark::Working, "Two", Some("s2")),
            task(2, Mark::Working, "Three", Some("s3")),
            task(3, Mark::Open, "Four", None),
        ];
        let new = [
            task(0, Mark::Open, "New", None),
            task(1, Mark::Working, "One", Some("s1")),
            task(2, Mark::Blocked, "Two", Some("s2")),
            task(3, Mark::Done, "Three", None),
            task(4, Mark::Open, "Four", None),
        ];
        let got = marks("p", &old, &new, 9);
        let whats: Vec<(&str, &What)> = got.iter().map(|e| (e.session.as_str(), &e.what)).collect();
        assert_eq!(
            whats,
            [
                (
                    "s1",
                    &What::Started {
                        title: "One".into()
                    }
                ),
                (
                    "s2",
                    &What::Blocked {
                        title: "Two".into(),
                        reason: "needs keys".into()
                    }
                ),
                (
                    "s3",
                    &What::Finished {
                        title: "Three".into(),
                        commits: Vec::new(),
                    }
                ),
            ]
        );
        assert!(got.iter().all(|e| e.project == "p" && e.at == 9));
    }

    #[test]
    fn a_log_reads_into_commits_and_a_line_without_a_tab_is_left_out() {
        let log = "b8f81ef\tRound the panes\n\n9e439e8\tMerge: a\tb \ngarbage\n";
        let got = commits(log);
        assert_eq!(
            got,
            [
                Commit {
                    hash: "b8f81ef".into(),
                    subject: "Round the panes".into()
                },
                Commit {
                    hash: "9e439e8".into(),
                    subject: "Merge: a\tb".into()
                },
            ]
        );
    }

    #[test]
    fn an_item_starts_where_it_was_last_taken_in_its_own_project() {
        let started = |title: &str| What::Started {
            title: title.into(),
        };
        let entries = [
            entry(1, "a", "p", started("One")),
            entry(2, "b", "q", started("One")),
            entry(3, "c", "p", started("Two")),
            entry(4, "d", "p", started("One")),
        ];
        assert_eq!(started_at(&entries, "p", "One"), Some(4));
        assert_eq!(started_at(&entries, "q", "One"), Some(2));
        assert_eq!(started_at(&entries, "p", "Three"), None);
    }
}
