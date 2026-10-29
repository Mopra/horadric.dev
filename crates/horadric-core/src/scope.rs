//! A quest filed for work another quest already covers.
//!
//! Quests are filed by different sessions at different times, and two of
//! them can build the same thing side by side: one lands, the other's
//! merge conflicts and its whole run is thrown away. `quest add` looks
//! at what is open, running and recently done in the same log, and says
//! so when the new quest covers the same ground, so the filer can merge
//! the two or have one wait for the other. It never refuses the quest.
//!
//! The first look is cheap: words the two share, weighed by how rare they
//! are in the log, so the words every quest uses count for little. Only a
//! pair that passes it goes to a model, which tells overlap from two
//! quests that merely touch the same part.

use std::collections::HashMap;

use crate::tasks::{after_line, Mark, Task};

/// How alike two quests have to read before the model is asked. Measured
/// on this project's own done file: a quest and its fix-up, or two built
/// for one window, score above it; neighbours in one area score below.
pub const ALIKE: f32 = 0.34;

/// The most quests named back, the closest first.
pub const MOST: usize = 3;

/// How many of the done file's last quests count as recently done. Older
/// ones are on `main` long since, where a session reads them anyway.
pub const RECENT_DONE: usize = 60;

/// How much of a quest's notes is read: the first lines say what it is,
/// the rest is what its session ran into.
const NOTES_READ: usize = 600;

/// A quest the new one reads like.
#[derive(Debug, Clone, PartialEq)]
pub struct Near {
    pub title: String,
    pub state: State,
    /// Cosine of the two quests' weighed words, from 0 to 1.
    pub score: f32,
    /// The words that make them alike, the weightiest first.
    pub shared: Vec<String>,
    /// Its notes, as much as was read.
    pub notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Running,
    Done,
}

impl State {
    fn of(mark: Mark) -> State {
        match mark {
            Mark::Open | Mark::Blocked => State::Open,
            Mark::Working | Mark::Review => State::Running,
            Mark::Done => State::Done,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Running => "running",
            State::Done => "done",
        }
    }
}

/// What the model made of one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The same work, or most of it: one quest should hold both.
    Same,
    /// The new one builds on the other and should wait for it.
    After,
    /// Neighbours, not overlap.
    Apart,
}

/// The quests in `log` and `done` the new one reads like, closest first,
/// at most [`MOST`]. `done` is the done file, of which only the last
/// [`RECENT_DONE`] count. A quest the new one already waits on is left
/// out: the filer has said how the two relate.
pub fn near(title: &str, notes: &str, log: &[Task], done: &[Task]) -> Vec<Near> {
    let waits: Vec<&str> = notes.lines().filter_map(after_line).collect();
    let recent = &done[done.len().saturating_sub(RECENT_DONE)..];
    let others: Vec<&Task> = log
        .iter()
        .chain(recent)
        .filter(|t| !waits.iter().any(|w| crate::aim::same(w, &t.title)))
        .collect();
    let new = bag(title, notes);
    let bags: Vec<HashMap<String, f32>> = others
        .iter()
        .map(|t| bag(&t.title, &t.notes.join("\n")))
        .collect();
    // How rare a word is across everything compared, the new quest too,
    // so a word only these two use weighs most.
    let mut df: HashMap<&str, f32> = HashMap::new();
    for b in bags.iter().chain([&new]) {
        for w in b.keys() {
            *df.entry(w).or_default() += 1.0;
        }
    }
    let n = (bags.len() + 1) as f32;
    let idf = |w: &str| (1.0 + n / df.get(w).copied().unwrap_or(1.0)).ln();
    let weigh = |b: &HashMap<String, f32>| -> HashMap<String, f32> {
        b.iter().map(|(w, c)| (w.clone(), c * idf(w))).collect()
    };
    let new_w = weigh(&new);
    let mut out: Vec<Near> = others
        .iter()
        .zip(&bags)
        .filter_map(|(t, b)| {
            let other = weigh(b);
            let score = cosine(&new_w, &other);
            if score < ALIKE {
                return None;
            }
            let mut shared: Vec<&String> =
                new_w.keys().filter(|w| other.contains_key(*w)).collect();
            let pull = |w: &String| new_w[w] * other[w];
            shared.sort_by(|a, b| pull(b).total_cmp(&pull(a)).then(a.cmp(b)));
            Some(Near {
                title: t.title.clone(),
                state: State::of(t.mark),
                score,
                shared: shared.into_iter().take(5).cloned().collect(),
                notes: read_notes(&t.notes),
            })
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(MOST);
    out
}

/// A quest's words and how often each comes, its title's twice over:
/// the title is what the quest is, the notes how.
fn bag(title: &str, notes: &str) -> HashMap<String, f32> {
    let mut b = HashMap::new();
    for w in words(title) {
        *b.entry(w).or_default() += 2.0;
    }
    for w in words(&read_notes_text(notes)) {
        *b.entry(w).or_default() += 1.0;
    }
    b
}

/// The notes lines that say what the quest is: not its `After:` lines,
/// nor the `Assumed:`, `Warriv:` and like lines sessions write under it.
fn read_notes_text(notes: &str) -> String {
    let mut out = String::new();
    for line in notes.lines().filter(|l| !is_meta(l)) {
        if out.len() >= NOTES_READ {
            break;
        }
        out.push_str(line.trim());
        out.push('\n');
    }
    out.chars().take(NOTES_READ).collect()
}

fn read_notes(notes: &[String]) -> String {
    read_notes_text(&notes.join("\n")).trim().to_string()
}

/// A line led by one word and a colon, as `After:` and `Assumed:` are.
fn is_meta(line: &str) -> bool {
    let line = line.trim();
    after_line(line).is_some()
        || line
            .split_once(':')
            .is_some_and(|(head, _)| !head.is_empty() && head.chars().all(char::is_alphabetic))
}

/// The words that can tell one quest from another: lower case, short
/// words and the commonest English ones dropped, a plural taken off so
/// "section" meets "sections".
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

fn stem(w: &str) -> String {
    if let Some(s) = w.strip_suffix("ies").filter(|s| s.len() >= 3) {
        return format!("{s}y");
    }
    w.strip_suffix('s')
        .filter(|s| s.len() >= 3 && !s.ends_with('s'))
        .unwrap_or(w)
        .to_string()
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "onto", "its", "it's", "are",
    "was", "were", "has", "have", "had", "not", "but", "all", "any", "one", "two", "each", "every",
    "when", "what", "which", "who", "where", "then", "than", "them", "they", "their", "there",
    "here", "out", "over", "under", "about", "only", "also", "too", "own", "can", "could", "would",
    "should", "will", "does", "did", "doe", "done", "make", "made", "same", "other", "more",
    "most", "some", "such", "like", "just", "yet", "now", "new", "see", "via", "per", "our", "you",
    "your", "his", "her", "how", "why", "use", "used", "way", "get", "gets", "set", "let", "lets",
];

fn cosine(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
    let dot: f32 = a.iter().filter_map(|(w, x)| b.get(w).map(|y| x * y)).sum();
    let len = |m: &HashMap<String, f32>| m.values().map(|x| x * x).sum::<f32>().sqrt();
    let (la, lb) = (len(a), len(b));
    if la == 0.0 || lb == 0.0 {
        0.0
    } else {
        dot / (la * lb)
    }
}

/// What the model is asked: one word per quest found, in order.
pub fn judge_prompt(title: &str, notes: &str, near: &[Near]) -> String {
    let mut s = String::from(
        "A quest was just filed in a project's quest log. Each quest is built by a \
         coding agent of its own, side by side with the others, so two quests that \
         build the same thing waste a whole run and conflict on merge.\n\n",
    );
    s.push_str(&format!("The new quest: {title}\n"));
    let notes = read_notes_text(notes);
    if !notes.trim().is_empty() {
        s.push_str(&format!("Its notes:\n{}\n", notes.trim()));
    }
    s.push_str("\nQuests already in the log that read like it:\n");
    for (i, n) in near.iter().enumerate() {
        s.push_str(&format!("{}. [{}] {}\n", i + 1, n.state.word(), n.title));
        if !n.notes.is_empty() {
            s.push_str(&format!("   Notes: {}\n", n.notes.replace('\n', " ")));
        }
    }
    s.push_str(
        "\nFor each numbered quest, answer on its own line with its number and one \
         word:\n\
         same: both would build the same thing, or most of it, so one quest should \
         hold both\n\
         after: the new quest adds to what that one builds (a part, a section, a \
         follow-up) and can not start until it is done\n\
         apart: they only touch the same area and can be built side by side\n\
         A quest that adds a piece to another is after, not same. Answer apart \
         unless you are sure. Write nothing else.\n",
    );
    s
}

/// The model's answer read back, one verdict per quest found, or `None`
/// when it did not answer every one.
pub fn verdicts(answer: &str, count: usize) -> Option<Vec<Verdict>> {
    let mut out = vec![None; count];
    for line in answer.lines() {
        let line = line.trim().to_lowercase();
        let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
        let Ok(i) = digits.parse::<usize>() else {
            continue;
        };
        let rest = line[digits.len()..].trim_start_matches(['.', ':', ')', ' ', '-']);
        let v = match rest.split_whitespace().next() {
            Some(w) if w.starts_with("same") => Verdict::Same,
            Some(w) if w.starts_with("after") => Verdict::After,
            Some(w) if w.starts_with("apart") => Verdict::Apart,
            _ => continue,
        };
        if let Some(slot) = i.checked_sub(1).and_then(|i| out.get_mut(i)) {
            *slot = Some(v);
        }
    }
    out.into_iter().collect()
}

/// What `quest add` says about the quests found, with what the model made
/// of each when it was asked. Nothing when the model called them all
/// apart. `horadric` is the command to name, `new` the quest just filed.
pub fn warning(horadric: &str, new: &str, found: &[(Near, Option<Verdict>)]) -> Option<String> {
    let kept: Vec<&(Near, Option<Verdict>)> = found
        .iter()
        .filter(|(_, v)| *v != Some(Verdict::Apart))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let mut s = String::from("This quest may cover ground another quest covers:\n");
    for (n, v) in &kept {
        let how = match v {
            Some(Verdict::Same) => "the same work",
            Some(Verdict::After) => "builds on it",
            _ => "shares the words",
        };
        s.push_str(&format!(
            "- \"{}\" ({}), {}: {}\n",
            n.title,
            n.state.word(),
            how,
            n.shared.join(", ")
        ));
    }
    let (n, v) = kept[0];
    match (n.state, v) {
        (State::Done, _) => s.push_str(&format!(
            "\"{}\" is done: check what `main` has before this runs, and narrow this \
             quest's notes to what is left, or take it out of the log.",
            n.title
        )),
        (_, Some(Verdict::After)) => s.push_str(&format!(
            "If this builds on it, have it wait: `{horadric} quest note \"{new}\" \"After: {}\"`.",
            n.title
        )),
        _ => s.push_str(&format!(
            "If it is the same work, merge the two: write what this one adds into \
             that quest with `{horadric} quest note \"{}\" \"<what it adds>\"` and take \
             this one out of the log. If this builds on it, have it wait instead: \
             `{horadric} quest note \"{new}\" \"After: {}\"`. If they are apart, \
             leave both; they run side by side.",
            n.title, n.title
        )),
    }
    Some(s)
}

/// What Warriv hears: the same, without the command lines it can not run
/// from a wake as written.
pub fn event_detail(found: &[(Near, Option<Verdict>)]) -> Option<String> {
    let kept: Vec<String> = found
        .iter()
        .filter(|(_, v)| *v != Some(Verdict::Apart))
        .map(|(n, v)| {
            let how = match v {
                Some(Verdict::Same) => "the same work as",
                Some(Verdict::After) => "builds on",
                _ => "reads like",
            };
            format!("{how} \"{}\" ({})", n.title, n.state.word())
        })
        .collect();
    (!kept.is_empty()).then(|| {
        format!(
            "It {}. Merge the two into one quest, give the new one an After: line, \
             or leave them if they are apart. Only real overlap waits; the rest runs \
             side by side.",
            kept.join("; ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::parse;
    use std::collections::BTreeSet;

    fn shared_words(a: &str, b: &str) -> BTreeSet<String> {
        let a: BTreeSet<String> = words(a).into_iter().collect();
        words(b).into_iter().filter(|w| a.contains(w)).collect()
    }

    /// Every pair in a real done file that scores as alike, to set
    /// [`ALIKE`] by: `SCOPE_LOG=<tasks-done.md> cargo test calibrate -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn calibrate() {
        let text = std::fs::read_to_string(std::env::var("SCOPE_LOG").unwrap()).unwrap();
        let all = parse(&text);
        for (i, t) in all.iter().enumerate() {
            let rest: Vec<Task> = all[..i].to_vec();
            for n in near(
                &t.title,
                &t.notes.join(
                    "
",
                ),
                &[],
                &rest,
            ) {
                println!(
                    "{:.2}  {}
      {}  [{}]",
                    n.score,
                    t.title,
                    n.title,
                    n.shared.join(",")
                );
            }
        }
    }

    const LOG: &str = "\
- [x] The Settings window, with the tray's settings moved into it
  A window of its own that closes, drawn like the usage window with the existing controls. Sections down the left; this quest builds the frame and Appearance, Notifications, Startup and updates and Privacy. The tray gains Settings and loses those lines.
- [/] Settings: the Projects section, opened from the project menu
  Every project in one list; picking one shows its colour, work mode, SSH hosts.
  After: The Settings window, with the tray's settings moved into it
- [ ] Show the browser page's zoom in its address bar
  Ctrl plus and minus zoom the page; the address bar says the zoom when it is not 100.
- [ ] Hide the browser pane's console capture from pages
  The capture script must not be visible to the page's own scripts.
- [ ] Sweep done quests into the done file
  Move done quests with their notes into tasks-done.md after a day.
";

    fn log() -> Vec<Task> {
        parse(LOG)
    }

    #[test]
    fn a_second_build_of_the_settings_window_is_found() {
        let found = near(
            "A Settings window, moving the tray's settings into it",
            "Its own window with sections down the left: Appearance, Notifications, Privacy. The tray loses those lines.",
            &log(),
            &[],
        );
        assert_eq!(
            found[0].title,
            "The Settings window, with the tray's settings moved into it"
        );
        assert_eq!(found[0].state, State::Done);
        assert!(
            found[0].shared.contains(&"tray".to_string()),
            "{:?}",
            found[0].shared
        );
    }

    #[test]
    fn a_quest_in_another_area_finds_nothing() {
        let found = near(
            "Speak the tile's state aloud for screen readers",
            "Use UI Automation so Narrator reads each tile.",
            &log(),
            &[],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn neighbours_in_one_area_are_not_overlap() {
        let found = near(
            "Let the browser pane print the page",
            "Ctrl P opens the print dialog of WebView2.",
            &log(),
            &[],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_quest_it_already_waits_on_is_left_out() {
        let found = near(
            "Settings: the Projects section, with SSH hosts",
            "Every project in one list, its colour and SSH hosts.\nAfter: Settings: the Projects section, opened from the project menu",
            &log(),
            &[],
        );
        assert!(found
            .iter()
            .all(|n| n.title != "Settings: the Projects section, opened from the project menu"));
    }

    #[test]
    fn only_recently_done_quests_count() {
        let old = parse(
            "- [x] Sweep done quests into the done file\n  Move done quests into tasks-done.md.\n",
        );
        let mut done = old.clone();
        let filler: String = (0..RECENT_DONE)
            .map(|i| format!("- [x] Filler number {i}\n"))
            .collect();
        done.extend(parse(&filler));
        let title = "Sweep the done quests into the done file";
        assert!(near(title, "", &[], &done).is_empty());
        assert_eq!(near(title, "", &[], &old).len(), 1);
    }

    #[test]
    fn running_and_open_states_are_told_apart() {
        assert_eq!(State::of(Mark::Review), State::Running);
        assert_eq!(State::of(Mark::Blocked), State::Open);
    }

    #[test]
    fn plurals_meet_their_singular() {
        assert_eq!(stem("sections"), "section");
        assert_eq!(stem("entries"), "entry");
        assert_eq!(stem("class"), "class");
        assert_eq!(
            shared_words("Settings sections", "the setting section"),
            ["section", "setting"].map(String::from).into()
        );
    }

    #[test]
    fn meta_lines_are_not_read() {
        let text =
            read_notes_text("After: X\nAssumed: a window\nThe real notes here.\nSee docs: plan");
        assert_eq!(text, "The real notes here.\nSee docs: plan\n");
    }

    #[test]
    fn verdicts_read_one_word_per_number() {
        assert_eq!(
            verdicts("1. same\n2: apart\n3 after, it waits", 3),
            Some(vec![Verdict::Same, Verdict::Apart, Verdict::After])
        );
        assert_eq!(verdicts("1 same", 2), None);
        assert_eq!(verdicts("Same\n1 maybe", 1), None);
    }

    fn a_near(state: State) -> Near {
        Near {
            title: "Old".into(),
            state,
            score: 0.5,
            shared: vec!["settings".into(), "tray".into()],
            notes: String::new(),
        }
    }

    #[test]
    fn apart_says_nothing() {
        let found = [(a_near(State::Open), Some(Verdict::Apart))];
        assert_eq!(warning("h", "New", &found), None);
        assert_eq!(event_detail(&found), None);
    }

    #[test]
    fn the_warning_suggests_a_merge_or_an_after_line() {
        let w = warning("h", "New", &[(a_near(State::Open), Some(Verdict::Same))]).unwrap();
        assert!(w.contains("\"Old\" (open), the same work: settings, tray"));
        assert!(w.contains("h quest note \"Old\""));
        assert!(w.contains("h quest note \"New\" \"After: Old\""));
        let w = warning(
            "h",
            "New",
            &[(a_near(State::Running), Some(Verdict::After))],
        )
        .unwrap();
        assert!(w.ends_with("`h quest note \"New\" \"After: Old\"`."));
        let w = warning("h", "New", &[(a_near(State::Done), None)]).unwrap();
        assert!(w.contains("\"Old\" is done: check what `main` has"));
    }

    #[test]
    fn the_prompt_numbers_each_quest() {
        let p = judge_prompt("New", "After: X\nWhat it does.", &[a_near(State::Running)]);
        assert!(p.contains("1. [running] Old"));
        assert!(p.contains("Its notes:\nWhat it does."));
        assert!(!p.contains("After: X"));
    }
}
