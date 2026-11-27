//! The universal search: every conversation Claude Code kept for any
//! project, found by what it was about, so one can be carried on without
//! knowing which project or day it was.
//!
//! The first search reads every transcript once, gigabytes of tool output
//! for a few megabytes of what was said, and keeps only that, lowercased
//! once. Later searches read only what was appended since, as Claude Code
//! only ever appends, so a keystroke costs a folder listing and a scan of
//! text already in memory. Nothing is written to disk.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use horadric_core::{retitle, title, Title};
use serde_json::Value;

use crate::transcript::{self, Listed, Past};

/// One conversation a search found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub past: Past,
    /// The folder the conversation ran in, read from its transcript, which
    /// is where a resume has to start it again.
    pub cwd: PathBuf,
    /// The project's short name, for the list.
    pub project: String,
    /// The words around the best match, one line, empty when only the
    /// title matched.
    pub snippet: String,
    pub score: f32,
    /// The best place the query was found, for a hint beside the row.
    pub matched: Matched,
}

/// Where a query was found, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Matched {
    /// The query was empty: the conversation is listed for being recent.
    Nothing,
    /// In one of Claude's replies.
    Reply,
    /// In something the human typed.
    Prompt,
    /// In the project's name or the git branch.
    Project,
    Title,
}

/// The conversations that best match `query`, best first, at most `limit`.
/// Slow the first time (it reads transcripts), so never on the UI thread.
/// An empty query lists the newest.
pub fn search(query: &str, limit: usize) -> Vec<Found> {
    static INDEX: OnceLock<Mutex<Index>> = OnceLock::new();
    let index = INDEX.get_or_init(Default::default);
    let Ok(mut index) = index.lock() else {
        return Vec::new();
    };
    index.refresh(transcript::listed());
    index.find(
        &Query::new(query),
        limit,
        &transcript::model_titles(),
        SystemTime::now(),
    )
}

/// The most of one prompt or reply kept: a pasted log or a long answer is
/// found by its start as well as by all of it.
const MESSAGE: usize = 4 * 1024;

/// The most of a conversation's prompts kept, which come before replies.
const PROMPTS: usize = 64 * 1024;

/// The most of a conversation's replies kept. A day long session says a
/// few hundred kilobytes, most conversations a few.
const REPLIES: usize = 192 * 1024;

/// Every transcript read so far, by path.
#[derive(Default)]
struct Index {
    entries: HashMap<PathBuf, Entry>,
}

impl Index {
    /// Brings the index in line with the folders: new and grown files read,
    /// gone ones dropped. The first time that is every file, so the reading
    /// is spread over threads.
    fn refresh(&mut self, listed: Vec<Listed>) {
        let mut seen = HashSet::with_capacity(listed.len());
        let mut work = Vec::new();
        for file in listed {
            if !resumable(&file.path) {
                continue;
            }
            seen.insert(file.path.clone());
            let old = self.entries.remove(&file.path);
            match old {
                Some(e) if (e.modified, e.len) == (file.modified, file.len) => {
                    self.entries.insert(file.path, e);
                }
                // Grown in place: only the new lines are read. A file
                // that shrank was written over, and is read from the top.
                Some(mut e) if file.len >= e.read => {
                    e.modified = file.modified;
                    e.len = file.len;
                    work.push(e);
                }
                _ => work.push(Entry::new(file)),
            }
        }
        self.entries.retain(|path, _| seen.contains(path));
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, 8);
        let share = work.len().div_ceil(threads).max(1);
        std::thread::scope(|s| {
            for chunk in work.chunks_mut(share) {
                s.spawn(move || {
                    for e in chunk {
                        e.read_on();
                    }
                });
            }
        });
        for e in work {
            self.entries.insert(e.path.clone(), e);
        }
    }

    fn find(
        &self,
        query: &Query,
        limit: usize,
        made: &retitle::Titles,
        now: SystemTime,
    ) -> Vec<Found> {
        let mut hits: Vec<(f32, &Entry, Hit, Title, bool)> = self
            .entries
            .values()
            .filter(|e| !e.cwd.is_empty())
            .filter_map(|e| {
                let (title, retitled) = e.title(made)?;
                let fields = Fields {
                    title: &fold(&title.text),
                    meta: &e.meta,
                    prompts: &e.prompts.text,
                    replies: &e.replies.text,
                };
                let hit = score(query, &fields)?;
                let age = now.duration_since(e.modified).unwrap_or_default();
                let total = hit.score + recency(age.as_secs_f64());
                Some((total, e, hit, title, retitled))
            })
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.modified.cmp(&a.1.modified)));
        // A conversation filed twice, under both spellings of a drive
        // letter, is listed once.
        let mut ids = HashSet::new();
        hits.into_iter()
            .filter(|(_, e, ..)| ids.insert(e.id.as_str()))
            .take(limit)
            .map(|(score, e, hit, title, retitled)| Found {
                snippet: hit
                    .at
                    .map(|(part, at, len)| e.part(part).snippet(at, len))
                    .unwrap_or_default(),
                past: Past {
                    id: e.id.clone(),
                    title,
                    asked: e.asked.clone(),
                    retitled,
                    path: e.path.clone(),
                    len: e.len,
                    modified: e.modified,
                    started: e.started,
                },
                cwd: PathBuf::from(&e.cwd),
                project: project(&e.cwd).to_string(),
                score,
                matched: hit.matched,
            })
            .collect()
    }
}

/// Whether `path` is a conversation one can resume. Old versions of
/// Claude Code kept subagents beside their parent as `agent-<id>.jsonl`.
fn resumable(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| !s.starts_with("agent-"))
}

/// What one transcript says, as far as it has been read.
#[derive(Debug)]
struct Entry {
    id: String,
    path: PathBuf,
    /// The name of the folder it is filed in, which says which of the
    /// working directories it records is the one it was started in.
    folder: String,
    modified: SystemTime,
    len: u64,
    started: SystemTime,
    /// How far it has been read: up to the end of its last whole line.
    read: u64,
    cwd: String,
    /// Whether `cwd` is the one the transcript is filed under, not only
    /// the first one seen.
    sure: bool,
    branch: String,
    /// The project's name and the branch, folded.
    meta: String,
    ai: Option<String>,
    custom: Option<String>,
    first: Option<String>,
    asked: String,
    prompts: Folded,
    replies: Folded,
}

impl Default for Entry {
    fn default() -> Entry {
        Entry {
            id: String::new(),
            path: PathBuf::new(),
            folder: String::new(),
            modified: SystemTime::UNIX_EPOCH,
            len: 0,
            started: SystemTime::UNIX_EPOCH,
            read: 0,
            cwd: String::new(),
            sure: false,
            branch: String::new(),
            meta: String::new(),
            ai: None,
            custom: None,
            first: None,
            asked: String::new(),
            prompts: Folded::default(),
            replies: Folded::default(),
        }
    }
}

impl Entry {
    fn new(file: Listed) -> Entry {
        let id = file
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        let folder = (file.path.parent())
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned());
        Entry {
            id: id.unwrap_or_default(),
            folder: folder.unwrap_or_default(),
            path: file.path,
            modified: file.modified,
            len: file.len,
            started: file.started,
            ..Entry::default()
        }
    }

    /// Reads the lines written since the last read. A line still being
    /// written is left for the next.
    fn read_on(&mut self) {
        let Ok(mut file) = File::open(&self.path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.read)).is_err() {
            return;
        }
        let mut reader = BufReader::with_capacity(256 * 1024, file);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(n) if n > 0 && line.ends_with(b"\n") => {
                    self.read += n as u64;
                    if let Ok(text) = std::str::from_utf8(&line) {
                        self.feed(text);
                    }
                }
                _ => break,
            }
        }
        // Text grows by doubling, which would leave up to half of what
        // the index holds unused.
        self.prompts.text.shrink_to_fit();
        self.replies.text.shrink_to_fit();
    }

    /// Takes in one transcript line.
    fn feed(&mut self, line: &str) {
        let said = said(line);
        if let Some(cwd) = said.cwd {
            if !self.sure {
                self.sure = transcript::folder_name(&cwd).eq_ignore_ascii_case(&self.folder);
                if self.sure || self.cwd.is_empty() {
                    self.cwd = cwd;
                    self.meta = fold(&format!("{} {}", project(&self.cwd), self.branch));
                }
            }
        }
        if let Some(branch) = said.branch.filter(|b| *b != self.branch) {
            self.branch = branch;
            self.meta = fold(&format!("{} {}", project(&self.cwd), self.branch));
        }
        match said.what {
            What::Title(t) if t.custom => self.custom = Some(t.text),
            What::Title(t) => self.ai = Some(t.text),
            What::Prompt(text) => {
                // Claude Code's own reading of a prompt names the
                // conversation, so the history list and this one agree.
                let bytes = line.as_bytes();
                if self.first.is_none() {
                    self.first = title::first_prompt(bytes).or_else(|| Some(cut(&text, 48)));
                }
                self.asked = title::last_prompt(bytes).unwrap_or_else(|| cut(&text, 140));
                self.prompts.push(&text, PROMPTS);
            }
            What::Reply(text) => self.replies.push(&text, REPLIES),
            What::Nothing => {}
        }
    }

    /// The conversation's title and whether a model wrote it: a /rename,
    /// else a model's, else Claude Code's, else its first prompt. None for
    /// one that never got going.
    fn title(&self, made: &retitle::Titles) -> Option<(Title, bool)> {
        if let Some(text) = &self.custom {
            return Some((
                Title {
                    text: text.clone(),
                    custom: true,
                },
                false,
            ));
        }
        if let Some(text) = made.title(&self.id) {
            return Some((
                Title {
                    text: text.to_string(),
                    custom: false,
                },
                true,
            ));
        }
        let text = self.ai.as_ref().or(self.first.as_ref())?;
        Some((
            Title {
                text: text.clone(),
                custom: false,
            },
            false,
        ))
    }

    fn part(&self, part: Part) -> &Folded {
        match part {
            Part::Prompts => &self.prompts,
            Part::Replies => &self.replies,
        }
    }
}

/// What one transcript line holds for a search.
#[derive(Debug, Default, PartialEq)]
struct Said {
    what: What,
    cwd: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
enum What {
    #[default]
    Nothing,
    Title(Title),
    /// Something the human typed, cleaned.
    Prompt(String),
    /// What Claude said back, cleaned, without its thinking or tool calls.
    Reply(String),
}

/// What `line` of a transcript holds. Most lines are tool calls and their
/// output, some megabytes long, and are passed over unparsed.
fn said(line: &str) -> Said {
    if line.contains("-title\"") {
        let what = title::latest(line.as_bytes()).map_or(What::Nothing, What::Title);
        return Said {
            what,
            ..Said::default()
        };
    }
    let user = line.contains(r#""type":"user""#) && !line.contains(r#""tool_result""#);
    let reply =
        !user && line.contains(r#""type":"assistant""#) && line.contains(r#""type":"text""#);
    if !user && !reply {
        return Said::default();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Said::default();
    };
    let field = |key: &str| {
        v[key]
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let mut said = Said {
        what: What::Nothing,
        cwd: field("cwd"),
        branch: field("gitBranch"),
    };
    if v["isSidechain"] == true || v["isMeta"] == true {
        return said;
    }
    let content = &v["message"]["content"];
    let text = match content.as_str() {
        Some(t) => t.to_string(),
        None => (content.as_array().into_iter().flatten())
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
    };
    let text = clean(&text);
    // Commands, their output and task notifications are tags, not typing.
    if text.is_empty() || (user && text.starts_with('<')) {
        return said;
    }
    said.what = match v["type"].as_str() {
        // A compacted conversation's summary is Claude's words.
        Some("user") if v["isCompactSummary"] == true => What::Reply(text),
        Some("user") => What::Prompt(text),
        Some("assistant") => What::Reply(text),
        _ => What::Nothing,
    };
    said
}

/// `text` without the reminders Claude Code slips into prompts, on one
/// line, at most [`MESSAGE`] bytes.
fn clean(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut kept = String::with_capacity(text.len().min(MESSAGE));
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        kept.push_str(&rest[..at]);
        kept.push(' ');
        rest = match rest[at..].find(CLOSE) {
            Some(end) => &rest[at + end + CLOSE.len()..],
            None => "",
        };
    }
    kept.push_str(rest);
    let mut out = String::with_capacity(kept.len().min(MESSAGE));
    for word in kept.split_whitespace() {
        if out.len() + word.len() + 1 > MESSAGE {
            let room = MESSAGE.saturating_sub(out.len() + 1);
            if room > 0 {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&word[..floor(word, room)]);
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The largest char boundary in `s` at or below `at`.
fn floor(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// At most `most` characters of `text`, with an ellipsis when it was cut:
/// a title for a prompt Claude Code's own reading passes over.
fn cut(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}\u{2026}", text[..at].trim_end()),
        None => text.to_string(),
    }
}

/// The short name of the project in `cwd`: its last folder.
fn project(cwd: &str) -> &str {
    cwd.split(['\\', '/'])
        .rfind(|p| !p.is_empty())
        .unwrap_or(cwd)
}

/// `c` in lower case, when that is one character as long as `c` in UTF-8
/// and turns back into `c`. Keeping the length keeps every byte offset of
/// a folded text that of the original, so a snippet can be cut from the
/// folded text and its capitals put back from one bit per byte.
fn lower(c: char) -> Option<char> {
    if c.is_ascii() {
        return c.is_ascii_uppercase().then(|| c.to_ascii_lowercase());
    }
    let low = only(c.to_lowercase())?;
    let back = only(low.to_uppercase())?;
    (low != c && back == c && low.len_utf8() == c.len_utf8()).then_some(low)
}

/// The one character `chars` holds, None when it holds more.
fn only(mut chars: impl Iterator<Item = char>) -> Option<char> {
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

/// `text` as a search compares it: in lower case, each character keeping
/// its length.
fn fold(text: &str) -> String {
    text.chars().map(|c| lower(c).unwrap_or(c)).collect()
}

/// Text kept folded, with the capitals it had remembered one bit per byte,
/// half the memory of keeping both.
#[derive(Debug, Default, Clone, PartialEq)]
struct Folded {
    text: String,
    capitals: Vec<u64>,
}

impl Folded {
    /// Adds `text` after a space, as much as fits within `most` bytes.
    fn push(&mut self, text: &str, most: usize) {
        let gap = usize::from(!self.text.is_empty());
        let room = most.saturating_sub(self.text.len() + gap);
        if room == 0 || text.is_empty() {
            return;
        }
        if gap == 1 {
            self.text.push(' ');
        }
        let text = &text[..floor(text, room)];
        for c in text.chars() {
            let at = self.text.len();
            match lower(c) {
                Some(low) => {
                    if self.capitals.len() <= at / 64 {
                        self.capitals.resize(at / 64 + 1, 0);
                    }
                    self.capitals[at / 64] |= 1 << (at % 64);
                    self.text.push(low);
                }
                None => self.text.push(c),
            }
        }
    }

    fn capital(&self, at: usize) -> bool {
        self.capitals
            .get(at / 64)
            .is_some_and(|w| w & (1 << (at % 64)) != 0)
    }

    /// `range` of the text as it was written.
    fn original(&self, range: std::ops::Range<usize>) -> String {
        let start = range.start;
        self.text[range]
            .char_indices()
            .map(|(i, c)| match self.capital(start + i) {
                true => c.to_uppercase().next().unwrap_or(c),
                false => c,
            })
            .collect()
    }

    /// About [`SNIPPET`] characters around the match at `at`, `len` bytes
    /// long, cut at words, in the case they were written.
    fn snippet(&self, at: usize, len: usize) -> String {
        let text = &self.text;
        let before = SNIPPET / 3;
        let mut start = text[..at]
            .char_indices()
            .rev()
            .nth(before - 1)
            .map_or(0, |(i, _)| i);
        if start > 0 {
            // Start at a word, unless that would hide the match.
            if let Some(space) = text[start..at].find(' ') {
                start += space + 1;
            }
        }
        let shown_before = text[start..at].chars().count();
        let after = SNIPPET.saturating_sub(shown_before);
        let mut end = text[at..]
            .char_indices()
            .nth(after)
            .map_or(text.len(), |(i, _)| at + i)
            .max(at + len);
        if end < text.len() {
            if let Some(space) = text[at + len..end].rfind(' ') {
                end = at + len + space;
            }
        }
        let mut out = String::new();
        if start > 0 {
            out.push('\u{2026}');
        }
        out.push_str(self.original(start..end).trim());
        if end < text.len() {
            out.push('\u{2026}');
        }
        out
    }
}

/// How many characters a snippet shows, about.
const SNIPPET: usize = 100;

/// A query as typed, folded into its words.
#[derive(Debug, Default, PartialEq)]
struct Query {
    words: Vec<String>,
    /// The words with single spaces, when there is more than one: found
    /// together they say more than found apart.
    phrase: Option<String>,
}

impl Query {
    fn new(typed: &str) -> Query {
        let folded = fold(typed);
        let mut words: Vec<String> = Vec::new();
        for w in folded.split_whitespace() {
            if !words.iter().any(|k| k == w) {
                words.push(w.to_string());
            }
        }
        let phrase =
            (words.len() > 1).then(|| folded.split_whitespace().collect::<Vec<_>>().join(" "));
        Query { words, phrase }
    }
}

/// A conversation's searchable parts, folded.
struct Fields<'a> {
    title: &'a str,
    meta: &'a str,
    prompts: &'a str,
    replies: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Prompts,
    Replies,
}

/// How well a conversation matched, before recency.
#[derive(Debug, PartialEq)]
struct Hit {
    score: f32,
    matched: Matched,
    /// Where to cut a snippet: the part, the byte offset and the length.
    at: Option<(Part, usize, usize)>,
}

const TITLE: f32 = 8.0;
const PROJECT: f32 = 4.0;
const PROMPT: f32 = 2.0;
const REPLY: f32 = 1.0;
const PHRASE_IN_TITLE: f32 = 6.0;
const PHRASE: f32 = 2.0;

/// How well `fields` match `query`, or None when a word is found nowhere.
/// Each word scores in every part it is in, by the part's weight and by
/// how whole a word it was found as. An empty query matches everything.
fn score(query: &Query, fields: &Fields) -> Option<Hit> {
    if query.words.is_empty() {
        return Some(Hit {
            score: 0.0,
            matched: Matched::Nothing,
            at: None,
        });
    }
    let mut total = 0.0;
    let mut matched = Matched::Nothing;
    let mut at: Option<(Part, usize, usize)> = None;
    let last = query.words.len() - 1;
    // The longest word placed the snippet: it is the least common.
    let mut placed_by = 0;
    for (i, word) in query.words.iter().enumerate() {
        let mid_typing = i == last;
        let mut any = false;
        let parts = [
            (fields.title, TITLE, Matched::Title, None),
            (fields.meta, PROJECT, Matched::Project, None),
            (fields.prompts, PROMPT, Matched::Prompt, Some(Part::Prompts)),
            (fields.replies, REPLY, Matched::Reply, Some(Part::Replies)),
        ];
        let mut placed = false;
        for (hay, weight, kind, part) in parts {
            let Some((pos, how)) = best(hay, word, mid_typing) else {
                continue;
            };
            any = true;
            total += weight * how;
            matched = matched.max(kind);
            if let (Some(part), false) = (part, placed) {
                placed = true;
                if word.len() > placed_by {
                    placed_by = word.len();
                    at = Some((part, pos, word.len()));
                }
            }
        }
        if !any {
            return None;
        }
    }
    if let Some(phrase) = &query.phrase {
        if fields.title.contains(phrase.as_str()) {
            total += PHRASE_IN_TITLE;
        }
        let found = [
            (fields.prompts, Part::Prompts),
            (fields.replies, Part::Replies),
        ]
        .into_iter()
        .find_map(|(hay, part)| hay.find(phrase.as_str()).map(|p| (part, p)));
        if let Some((part, pos)) = found {
            total += PHRASE;
            at = Some((part, pos, phrase.len()));
        }
    }
    Some(Hit {
        score: total,
        matched,
        at,
    })
}

/// How many of a word's places in one part are weighed: the first whole
/// one is usually among them, and a common word is in thousands.
const PLACES: usize = 8;

/// The best place `word` is in `hay` and how good it is: 1 as a whole
/// word, or as the start of one when it is still being typed, less as the
/// start of a longer word, least inside one.
fn best(hay: &str, word: &str, mid_typing: bool) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (at, _) in hay.match_indices(word).take(PLACES) {
        let starts = hay[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let ends = (hay[at + word.len()..].chars().next()).is_none_or(|c| !c.is_alphanumeric());
        let how = match (starts, ends || mid_typing) {
            (true, true) => 1.0,
            (true, false) => 0.75,
            _ => 0.4,
        };
        if best.is_none_or(|(_, b)| how > b) {
            best = Some((at, how));
        }
        if how == 1.0 {
            break;
        }
    }
    best
}

/// The most recency adds: a fresh conversation found in a reply ranks
/// with an old one found in a prompt, never with one found by its title.
const RECENT: f32 = 3.0;

/// What `age_secs` since the last change adds to a score: halving every
/// two weeks.
fn recency(age_secs: f64) -> f32 {
    const HALF: f64 = 14.0 * 24.0 * 3600.0;
    (RECENT as f64 * 0.5f64.powf(age_secs.max(0.0) / HALF)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(title: &'a str, meta: &'a str, prompts: &'a str, replies: &'a str) -> Fields<'a> {
        Fields {
            title,
            meta,
            prompts,
            replies,
        }
    }

    #[test]
    fn folding_lowers_case_and_keeps_every_byte_where_it_was() {
        assert_eq!(fold("Hello WORLD"), "hello world");
        assert_eq!(fold("ÆBLE Øl Åen"), "æble øl åen");
        assert_eq!(fold("ÉTÉ Über"), "été über");
        // The Kelvin sign lowers to a shorter k, and is left as it is.
        assert_eq!(fold("\u{212A}"), "\u{212A}");
        for s in ["Straße ÆØÅ", "İstanbul", "ǅ", "ΣΑΣ"] {
            assert_eq!(fold(s).len(), s.len(), "{s}");
        }
    }

    #[test]
    fn folded_text_gives_back_its_capitals() {
        let mut f = Folded::default();
        f.push("Hej Ærlige VERDEN", 1000);
        f.push("Über alles", 1000);
        assert_eq!(f.text, "hej ærlige verden über alles");
        assert_eq!(f.original(0..f.text.len()), "Hej Ærlige VERDEN Über alles");
        assert_eq!(f.original(4..11), "Ærlige");
    }

    #[test]
    fn folded_text_stops_at_its_cap_on_a_char_boundary() {
        let mut f = Folded::default();
        f.push("abc", 6);
        f.push("ææææ", 6);
        assert_eq!(f.text, "abc æ");
        f.push("more", 6);
        assert_eq!(f.text, "abc æ");
    }

    #[test]
    fn a_query_is_folded_words_with_a_phrase() {
        let q = Query::new("  Universal   SEARCH search ");
        assert_eq!(q.words, vec!["universal", "search"]);
        assert_eq!(q.phrase.as_deref(), Some("universal search search"));
        assert_eq!(Query::new("one").phrase, None);
        assert!(Query::new("   ").words.is_empty());
    }

    #[test]
    fn a_reminder_is_cleaned_out_and_whitespace_folded() {
        assert_eq!(
            clean("Fix it\n\n<system-reminder>\nsecret\n</system-reminder>  now"),
            "Fix it now"
        );
        assert_eq!(clean("Hi <system-reminder>never closed"), "Hi");
        let long = format!("{} tail", "ø".repeat(MESSAGE));
        let cleaned = clean(&long);
        assert!(cleaned.len() <= MESSAGE);
        assert!(cleaned.chars().all(|c| c == 'ø'));
    }

    #[test]
    fn a_prompt_line_is_said_with_its_folder_and_branch() {
        let line = r#"{"type":"user","cwd":"C:\\code\\app","gitBranch":"main","message":{"role":"user","content":"Make the  search fast"}}"#;
        assert_eq!(
            said(line),
            Said {
                what: What::Prompt("Make the search fast".into()),
                cwd: Some(r"C:\code\app".into()),
                branch: Some("main".into()),
            }
        );
    }

    #[test]
    fn a_reply_keeps_its_text_and_drops_thinking_and_tools() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Done, it is fast."},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(said(line).what, What::Reply("Done, it is fast.".into()));
        let tools = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(said(tools).what, What::Nothing);
    }

    #[test]
    fn tool_results_commands_meta_and_sidechains_are_not_said() {
        let lines = [
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"Caveat"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"Agent prompt"}}"#,
            r#"{"type":"user","message":{"content":"<system-reminder>only this</system-reminder>"}}"#,
            r#"{"type":"attachment","content":"anything"}"#,
            "not json",
        ];
        for l in lines {
            assert_eq!(said(l).what, What::Nothing, "{l}");
        }
    }

    #[test]
    fn titles_and_compact_summaries_are_said_for_what_they_are() {
        let ai = r#"{"type":"ai-title","aiTitle":"Fast search"}"#;
        assert_eq!(
            said(ai).what,
            What::Title(Title {
                text: "Fast search".into(),
                custom: false
            })
        );
        let renamed = r#"{"type":"custom-title","customTitle":"Mine"}"#;
        assert!(matches!(said(renamed).what, What::Title(t) if t.custom));
        let summary = r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#;
        assert_eq!(
            said(summary).what,
            What::Reply("This session is being continued".into())
        );
    }

    #[test]
    fn an_entry_takes_the_folder_it_is_filed_under_and_the_newest_title() {
        let mut e = Entry {
            id: "a1".into(),
            folder: "C--code-app".into(),
            ..Entry::default()
        };
        for l in [
            r#"{"type":"user","cwd":"C:\\code\\app\\sub","gitBranch":"main","message":{"content":"First thing"}}"#,
            r#"{"type":"user","cwd":"C:\\code\\app","gitBranch":"feature","message":{"content":"Second THING"}}"#,
            r#"{"type":"assistant","cwd":"C:\\code\\other","message":{"content":[{"type":"text","text":"Answered"}]}}"#,
            r#"{"type":"ai-title","aiTitle":"Things"}"#,
        ] {
            e.feed(l);
        }
        assert_eq!(e.cwd, r"C:\code\app");
        assert_eq!(e.branch, "feature");
        assert_eq!(e.meta, "app feature");
        assert_eq!(e.prompts.text, "first thing second thing");
        assert_eq!(e.replies.text, "answered");
        assert_eq!(e.first.as_deref(), Some("First thing"));
        assert_eq!(e.asked, "Second THING");
        let none = retitle::Titles::default();
        assert_eq!(e.title(&none).unwrap().0.text, "Things");
        e.feed(r#"{"type":"custom-title","customTitle":"Named"}"#);
        assert_eq!(
            e.title(&none),
            Some((
                Title {
                    text: "Named".into(),
                    custom: true
                },
                false
            ))
        );
    }

    #[test]
    fn an_entry_with_no_prompt_or_title_has_no_title() {
        let e = Entry::default();
        assert_eq!(e.title(&retitle::Titles::default()), None);
    }

    #[test]
    fn a_project_is_the_last_folder() {
        assert_eq!(project(r"C:\code\horadric.dev"), "horadric.dev");
        assert_eq!(project("/home/x/app/"), "app");
        assert_eq!(project(""), "");
    }

    #[test]
    fn a_long_prompt_is_cut_with_an_ellipsis() {
        assert_eq!(cut("short", 48), "short");
        assert_eq!(cut("æøå æøå", 4), "æøå\u{2026}");
    }

    #[test]
    fn only_conversations_one_can_resume_are_searched() {
        assert!(resumable(Path::new("C:/p/C--app/1234.jsonl")));
        assert!(!resumable(Path::new("C:/p/C--app/agent-a1.jsonl")));
    }

    #[test]
    fn a_whole_word_beats_a_start_beats_an_inside() {
        assert_eq!(best("the search box", "search", false), Some((4, 1.0)));
        assert_eq!(
            best("researcher searches", "search", false),
            Some((11, 0.75))
        );
        assert_eq!(best("researcher", "search", false), Some((2, 0.4)));
        assert_eq!(best("the searches", "search", true), Some((4, 1.0)));
        assert_eq!(best("nothing", "search", false), None);
    }

    #[test]
    fn every_word_must_be_found_somewhere() {
        let q = Query::new("search æble");
        let f = fields("fast search", "app main", "an æble a day", "");
        assert!(score(&q, &f).is_some());
        let f = fields("fast search", "app main", "a pear a day", "");
        assert_eq!(score(&q, &f), None);
    }

    #[test]
    fn title_beats_project_beats_prompt_beats_reply() {
        let q = Query::new("runes");
        let at = |f: Fields| score(&q, &f).unwrap();
        let title = at(fields("runes", "", "", ""));
        let project = at(fields("", "runes main", "", ""));
        let prompt = at(fields("", "", "draw runes", ""));
        let reply = at(fields("", "", "", "drew runes"));
        assert!(title.score > project.score);
        assert!(project.score > prompt.score);
        assert!(prompt.score > reply.score);
        assert_eq!(title.matched, Matched::Title);
        assert_eq!(reply.matched, Matched::Reply);
        assert_eq!(title.at, None);
        assert_eq!(prompt.at, Some((Part::Prompts, 5, 5)));
        assert_eq!(reply.at, Some((Part::Replies, 5, 5)));
    }

    #[test]
    fn words_together_beat_words_apart() {
        let q = Query::new("fast search");
        let together = score(&q, &fields("", "", "make search fast search", "")).unwrap();
        let apart = score(&q, &fields("", "", "search is not fast", "")).unwrap();
        assert!(together.score > apart.score);
        assert_eq!(together.at, Some((Part::Prompts, 12, 11)));
    }

    #[test]
    fn an_empty_query_matches_everything_with_no_snippet() {
        let hit = score(&Query::new(""), &fields("", "", "", "")).unwrap();
        assert_eq!(hit.matched, Matched::Nothing);
        assert_eq!(hit.at, None);
    }

    #[test]
    fn recency_halves_every_two_weeks() {
        let day = 24.0 * 3600.0;
        assert!((recency(0.0) - RECENT).abs() < 1e-6);
        assert!((recency(14.0 * day) - RECENT / 2.0).abs() < 1e-4);
        assert!(recency(365.0 * day) < 0.01);
        assert!(recency(-5.0) <= RECENT);
    }

    #[test]
    fn a_snippet_is_cut_at_words_around_the_match_in_its_own_case() {
        let mut f = Folded::default();
        let words: Vec<String> = (0..60).map(|i| format!("Word{i}")).collect();
        f.push(&words.join(" "), 100_000);
        let at = f.text.find("word30").unwrap();
        let s = f.snippet(at, 6);
        assert!(s.starts_with('\u{2026}'), "{s}");
        assert!(s.ends_with('\u{2026}'), "{s}");
        assert!(s.contains("Word30"), "{s}");
        let inner = s.trim_matches('\u{2026}');
        assert!(inner.starts_with("Word"), "{s}");
        assert!(inner.chars().last().unwrap().is_ascii_digit(), "{s}");
        assert!(s.chars().count() <= SNIPPET + 12, "{s}");
    }

    #[test]
    fn a_short_text_is_its_own_snippet() {
        let mut f = Folded::default();
        f.push("Søg efter Æbler", 1000);
        let at = f.text.find("æbler").unwrap();
        assert_eq!(f.snippet(at, "æbler".len()), "Søg efter Æbler");
    }

    #[test]
    fn a_snippet_of_multibyte_text_stays_on_char_boundaries() {
        let mut f = Folded::default();
        f.push(&"æøå ".repeat(200), 100_000);
        let at = f.text.len() / 2;
        let at = floor(&f.text, at);
        let s = f.snippet(at, 2);
        assert!(!s.is_empty());
    }

    #[test]
    fn the_index_finds_by_title_and_lists_newest_without_a_query() {
        let mut index = Index::default();
        let now = SystemTime::now();
        let entry = |id: &str, title: &str, prompt: &str, mins: u64| {
            let mut e = Entry {
                id: id.into(),
                path: PathBuf::from(format!("C:/p/C--app/{id}.jsonl")),
                folder: "C--app".into(),
                modified: now - std::time::Duration::from_secs(mins * 60),
                ..Entry::default()
            };
            e.feed(&format!(
                r#"{{"type":"user","cwd":"C:\\app","message":{{"content":"{prompt}"}}}}"#
            ));
            e.feed(&format!(r#"{{"type":"ai-title","aiTitle":"{title}"}}"#));
            e
        };
        for e in [
            entry("old", "Draw the runes", "Runes please", 600),
            entry("new", "Fix the stage", "Something about Runes", 1),
            entry("none", "Unrelated", "Nothing here", 0),
        ] {
            index.entries.insert(e.path.clone(), e);
        }
        let made = retitle::Titles::default();
        let found = index.find(&Query::new("rune"), 10, &made, now);
        let ids: Vec<&str> = found.iter().map(|f| f.past.id.as_str()).collect();
        assert_eq!(ids, vec!["old", "new"]);
        assert_eq!(found[0].matched, Matched::Title);
        assert_eq!(found[0].cwd, PathBuf::from(r"C:\app"));
        assert_eq!(found[0].project, "app");
        assert_eq!(found[1].snippet, "Something about Runes");
        let newest = index.find(&Query::new(""), 2, &made, now);
        let ids: Vec<&str> = newest.iter().map(|f| f.past.id.as_str()).collect();
        assert_eq!(ids, vec!["none", "new"]);
    }

    #[test]
    fn a_grown_transcript_is_read_on_from_where_it_stopped() {
        let dir = std::env::temp_dir().join(format!("horadric-test-search-{}", std::process::id()));
        let folder = dir.join("C--app");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("s1.jsonl");
        let first = r#"{"type":"user","cwd":"C:\\app","message":{"content":"Alpha"}}"#;
        let second = r#"{"type":"user","cwd":"C:\\app","message":{"content":"Beta"}}"#;
        std::fs::write(&path, format!("{first}\n{}", &second[..10])).unwrap();
        let listed = |len| Listed {
            path: path.clone(),
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(len),
            len,
            started: SystemTime::UNIX_EPOCH,
        };
        let mut index = Index::default();
        let len = std::fs::metadata(&path).unwrap().len();
        index.refresh(vec![listed(len)]);
        assert_eq!(index.entries[&path].prompts.text, "alpha");
        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        index.refresh(vec![listed(len)]);
        assert_eq!(index.entries[&path].prompts.text, "alpha beta");
        assert_eq!(index.entries[&path].read, len);
        index.refresh(Vec::new());
        assert!(index.entries.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
