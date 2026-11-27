//! The trail: a rolling log of what the human worked on, kept seven
//! days in the state folder, sealed for this Windows user.
//!
//! One file per day, named by its date (UTC). Each entry is sealed by
//! itself and added at the end as a record of its own, so a day is never
//! written again as a whole and a crash costs one entry at most. The
//! time of an entry stands outside its seal: it is no secret, and the
//! last minutes can then be read without unsealing the whole day.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::vault::store;

/// How long a line of the trail is kept.
pub const KEEP_DAYS: u64 = 7;

/// One thing seen: the program in front, its window's title, and the
/// text of it that was new since the last reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Seconds since 1970.
    pub at: u64,
    /// The program's file name without its folder, as `msedge.exe`.
    pub program: String,
    pub title: String,
    /// Empty when only the window in front changed.
    pub text: String,
}

const DAY: u64 = 86_400;

/// Starts every record, so a reader finds the next whole one after a
/// record a crash left half written.
const MAGIC: [u8; 4] = *b"HTr1";

/// The magic, the time and the length of what follows.
const HEAD: usize = 4 + 8 + 4;

/// No sealed entry is this long. A length above it is damage, not a
/// record to wait for.
const RECORD_MOST: usize = 16 * 1024 * 1024;

const FILE_END: &str = ".trail";

const ASKED_FILE: &str = "asked.log";

/// The vault is read from disk for [`write`], so its values are kept
/// this long between entries.
const KNOWN_FOR: Duration = Duration::from_secs(5 * 60);

/// Old days are looked for this often, not at every entry.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// A window's lines remembered as seen. Past this the memory starts
/// over, so a window open all day cannot grow it without end.
const SEEN_MOST: usize = 4_000;

/// What `asked.log` keeps of the question itself.
const ASKED_MOST: usize = 300;

/// Where the trail is kept: `state_dir()/trail`.
pub fn dir() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join("trail"))
}

/// Writes one entry at the end of today's file, secrets cut out, and
/// deletes the files older than [`KEEP_DAYS`].
pub fn write(entry: &Entry) -> Result<(), String> {
    let dir = dir().ok_or("no state folder")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let known = known();
    let cut = |s: &str| horadric_memory::transcripts::redact_known(s, &known);
    let plain = plain_of(&entry.program, &cut(&entry.title), &cut(&entry.text));
    let sealed = store::seal(&plain).ok_or("DPAPI would not seal it")?;
    let record = record_of(entry.at, &sealed);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(day_file(entry.at / DAY)))
        .map_err(|e| e.to_string())?;
    // One call, so a record is whole or absent unless the disk fills.
    file.write_all(&record).map_err(|e| e.to_string())?;
    prune_now_and_then(&dir);
    Ok(())
}

/// The entries of the last `minutes`, oldest first.
pub fn recent(minutes: u64, now: u64) -> Vec<Entry> {
    read_between(now.saturating_sub(minutes.saturating_mul(60)), now)
}

/// The entries that fit `query` best by its words, over all that is
/// kept, best first, at most `most`.
pub fn find(query: &str, most: usize, now: u64) -> Vec<Entry> {
    rank(
        read_between(now.saturating_sub(KEEP_DAYS * DAY), now),
        query,
        most,
    )
}

/// The entries as text for a prompt, each with its age, program and
/// title, the newest kept when all of it is more than `most_chars`.
pub fn render(entries: &[Entry], now: u64, most_chars: usize) -> String {
    let mut blocks: Vec<Option<String>> = entries.iter().map(|_| None).collect();
    // Newest first decides what fits, the order given decides how it
    // reads: `find` hands its best first and that must stay so.
    let mut newest: Vec<usize> = (0..entries.len()).collect();
    newest.sort_by(|a, b| entries[*b].at.cmp(&entries[*a].at).then(b.cmp(a)));
    let mut left = most_chars;
    for (nth, i) in newest.into_iter().enumerate() {
        let block = block_of(&entries[i], now);
        let chars = block.chars().count();
        if chars <= left {
            left -= chars;
            blocks[i] = Some(block);
            continue;
        }
        // The newest alone is too long: better its start than nothing.
        if nth == 0 {
            blocks[i] = Some(block.chars().take(left).collect());
        }
        break;
    }
    blocks.into_iter().flatten().collect()
}

/// Writes down that the trail was asked: who, what for and what was
/// asked, in `asked.log` beside the trail, where the human can read it.
pub fn asked(who: &str, why: &str, what: &str, now: u64) {
    let Some(dir) = dir() else { return };
    let line = asked_line(who, why, what, now);
    let written = fs::create_dir_all(&dir).and_then(|()| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(ASKED_FILE))?
            .write_all(line.as_bytes())
    });
    if let Err(e) = written {
        eprintln!("horadric: the trail could not note who asked it: {e}");
    }
}

/// Deletes the day files older than [`KEEP_DAYS`]. [`write`] does this
/// itself now and then; the app does it as it starts and when the
/// companion is turned off, when nothing may be written for days.
pub fn prune(now: u64) {
    if let Some(dir) = dir() {
        prune_in(&dir, now / DAY);
    }
}

/// The lines of one window already written to the trail, so a reading
/// adds only what is new.
#[derive(Default)]
pub struct Seen {
    lines: HashSet<u64>,
}

impl Seen {
    /// The lines of `text` not seen in this window before, in their
    /// order, and remembers them.
    pub fn fresh(&mut self, text: &str) -> String {
        let lines: Vec<(u64, &str)> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| (line_key(l), l))
            .collect();
        let mut out = String::new();
        for (key, line) in &lines {
            if self.lines.insert(*key) {
                out.push_str(line);
                out.push('\n');
            }
        }
        if self.lines.len() > SEEN_MOST {
            // What is on screen now stays known, or the next reading
            // would write all of it once more.
            self.lines = lines.iter().map(|(key, _)| *key).collect();
        }
        out.truncate(out.trim_end().len());
        out
    }
}

fn line_key(line: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
}

/// The vault's values, read again every few minutes.
fn known() -> Vec<String> {
    static KNOWN: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
    let mut kept = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, values)) = kept.as_ref() {
        if at.elapsed() < KNOWN_FOR {
            return values.clone();
        }
    }
    let values = vault_values();
    *kept = Some((Instant::now(), values.clone()));
    values
}

/// Every value the vault keeps, from the file as it is now.
/// `store::values` answers from what this process read at its first
/// question, and the watcher lives for days while the app adds secrets
/// in another process: one added after its start would never be cut.
fn vault_values() -> Vec<String> {
    let fresh = horadric_hooks::state_dir()
        .map(|d| d.join("secrets.dat"))
        .and_then(|p| store::read_sealed(&p).ok().flatten())
        .and_then(|b| serde_json::from_slice::<store::Vault>(&b).ok());
    let Some(v) = fresh else {
        return store::values();
    };
    v.secrets
        .iter()
        .map(|s| s.value.clone())
        .chain(v.logins.iter().map(|l| l.password.clone()))
        .chain(v.logins.iter().filter_map(|l| l.totp.clone()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn prune_now_and_then(dir: &Path) {
    static PRUNED: Mutex<Option<Instant>> = Mutex::new(None);
    let mut pruned = PRUNED.lock().unwrap_or_else(|e| e.into_inner());
    if pruned.is_some_and(|at| at.elapsed() < PRUNE_EVERY) {
        return;
    }
    *pruned = Some(Instant::now());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    prune_in(dir, now / DAY);
}

fn prune_in(dir: &Path, today: u64) {
    let Ok(files) = fs::read_dir(dir) else { return };
    for file in files.flatten() {
        let old = file
            .file_name()
            .to_str()
            .and_then(day_of_file)
            .is_some_and(|day| too_old(day, today));
        if old {
            if let Err(e) = fs::remove_file(file.path()) {
                eprintln!("horadric: an old day of the trail would not go: {e}");
            }
        }
    }
}

/// Whether the file of `day` holds nothing young enough to keep. The
/// day that is exactly [`KEEP_DAYS`] back still does, in its later hours.
fn too_old(day: u64, today: u64) -> bool {
    today.saturating_sub(day) > KEEP_DAYS
}

/// The entries from `since` to `until`, oldest first.
fn read_between(since: u64, until: u64) -> Vec<Entry> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for day in since / DAY..=until / DAY {
        let Ok(bytes) = fs::read(dir.join(day_file(day))) else {
            continue;
        };
        for (at, sealed) in records(&bytes) {
            if at < since || at > until {
                continue;
            }
            let entry = store::unseal(sealed).and_then(|plain| entry_of(at, &plain));
            out.extend(entry);
        }
    }
    // A clock set back can put a later record before an earlier one.
    out.sort_by_key(|e| e.at);
    out
}

fn plain_of(program: &str, title: &str, text: &str) -> Vec<u8> {
    serde_json::json!({ "p": program, "t": title, "x": text })
        .to_string()
        .into_bytes()
}

fn entry_of(at: u64, plain: &[u8]) -> Option<Entry> {
    let v: serde_json::Value = serde_json::from_slice(plain).ok()?;
    let part = |key: &str| v.get(key).and_then(|s| s.as_str()).map(str::to_string);
    Some(Entry {
        at,
        program: part("p")?,
        title: part("t")?,
        text: part("x")?,
    })
}

fn record_of(at: u64, sealed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEAD + sealed.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
    out.extend_from_slice(sealed);
    out
}

/// The whole records in a day's bytes, each with its time. What a crash
/// left half written is stepped over to the next record's start.
fn records(bytes: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + HEAD <= bytes.len() {
        if let Some((at, sealed)) = record_at(&bytes[i..]) {
            out.push((at, sealed));
            i += HEAD + sealed.len();
            continue;
        }
        let Some(next) = bytes[i + 1..]
            .windows(MAGIC.len())
            .position(|w| w == MAGIC)
        else {
            break;
        };
        i += 1 + next;
    }
    out
}

fn record_at(bytes: &[u8]) -> Option<(u64, &[u8])> {
    if !bytes.starts_with(&MAGIC) {
        return None;
    }
    let at = u64::from_le_bytes(bytes.get(4..12)?.try_into().ok()?);
    let len = u32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?) as usize;
    if len > RECORD_MOST {
        return None;
    }
    let sealed = bytes.get(HEAD..HEAD + len)?;
    // A record cut short borrows its missing end from the one after
    // it, and would take that one down with it. A whole record is
    // followed by another's start or by nothing.
    let after = &bytes[HEAD + len..];
    let whole = if after.len() >= MAGIC.len() {
        after.starts_with(&MAGIC)
    } else {
        MAGIC.starts_with(after)
    };
    whole.then_some((at, sealed))
}

/// The year, month and day of a day counted from 1970.
fn civil(day: u64) -> (i64, i64, i64) {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The day counted from 1970 of a date, None for one before it.
fn day_of(y: i64, m: i64, d: i64) -> Option<u64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from(era * 146_097 + doe - 719_468).ok()
}

fn day_file(day: u64) -> String {
    let (y, m, d) = civil(day);
    format!("{y:04}-{m:02}-{d:02}{FILE_END}")
}

/// The day a file of the trail is for, None for any other file there.
fn day_of_file(name: &str) -> Option<u64> {
    let mut parts = name.strip_suffix(FILE_END)?.split('-');
    let mut next = || parts.next()?.parse::<i64>().ok();
    let (y, m, d) = (next()?, next()?, next()?);
    let day = day_of(y, m, d)?;
    // Only a name this module would have written: `2026-2-30` is not.
    (parts.next().is_none() && day_file(day) == name).then_some(day)
}

/// The words a question is ranked by, each once.
fn words(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
    {
        if !out.iter().any(|w| w == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// How well an entry fits: how many of the words it has at all, then
/// how strongly. A word in the title says what the window was about, so
/// it weighs more than one in the text, and a word said over and over
/// counts three times at most.
fn score(entry: &Entry, words: &[String]) -> (u32, u32) {
    let title = entry.title.to_lowercase();
    let program = entry.program.to_lowercase();
    let text = entry.text.to_lowercase();
    let mut matched = 0;
    let mut weight = 0;
    for word in words {
        let w = 3 * u32::from(title.contains(word.as_str()))
            + 2 * u32::from(program.contains(word.as_str()))
            + text.matches(word.as_str()).take(3).count() as u32;
        matched += u32::from(w > 0);
        weight += w;
    }
    (matched, weight)
}

/// The entries that have any of the question's words, best first, the
/// newest first among equals.
fn rank(entries: Vec<Entry>, query: &str, most: usize) -> Vec<Entry> {
    let words = words(query);
    let mut scored: Vec<((u32, u32), Entry)> = entries
        .into_iter()
        .map(|e| (score(&e, &words), e))
        .filter(|(s, _)| s.0 > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.at.cmp(&a.1.at)));
    scored.into_iter().take(most).map(|(_, e)| e).collect()
}

fn age(secs: u64) -> String {
    let (mins, hours, days) = (secs / 60, secs / 3_600, secs / DAY);
    match (days, hours, mins) {
        (0, 0, 0) => "just now".to_string(),
        (0, 0, m) => format!("{m} min ago"),
        (0, h, m) if m % 60 == 0 => format!("{h} h ago"),
        (0, h, m) => format!("{h} h {} min ago", m % 60),
        (1, ..) => "1 day ago".to_string(),
        (d, ..) => format!("{d} days ago"),
    }
}

fn block_of(entry: &Entry, now: u64) -> String {
    let mut out = format!(
        "{}, {}, {}\n",
        age(now.saturating_sub(entry.at)),
        entry.program,
        entry.title
    );
    for line in entry.text.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One line of `asked.log`. Line breaks in what a session sent would
/// let it write lines that look like other askings, so they go.
fn asked_line(who: &str, why: &str, what: &str, now: u64) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (y, m, d) = civil(now / DAY);
    let s = now % DAY;
    // Both are a session's own words, as long as it cared to make them.
    let capped = |s: &str| flat(s).chars().take(ASKED_MOST).collect::<String>();
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC, {}, for: {}, asked: {}\n",
        s / 3_600,
        s % 3_600 / 60,
        s % 60,
        flat(who),
        capped(why),
        capped(what)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, program: &str, title: &str, text: &str) -> Entry {
        Entry {
            at,
            program: program.into(),
            title: title.into(),
            text: text.into(),
        }
    }

    #[test]
    fn records_come_back_as_written() {
        let mut bytes = record_of(10, b"first");
        bytes.extend(record_of(20, b""));
        bytes.extend(record_of(30, b"third"));
        assert_eq!(
            records(&bytes),
            vec![(10, &b"first"[..]), (20, &b""[..]), (30, &b"third"[..])]
        );
    }

    #[test]
    fn a_half_written_record_costs_only_itself() {
        let mut bytes = record_of(10, b"first");
        let torn = record_of(20, b"never finished");
        bytes.extend(&torn[..torn.len() - 4]);
        bytes.extend(record_of(30, b"third"));
        assert_eq!(
            records(&bytes),
            vec![(10, &b"first"[..]), (30, &b"third"[..])]
        );
        // And a torn end is simply not there.
        let mut bytes = record_of(10, b"first");
        bytes.extend(&torn[..7]);
        assert_eq!(records(&bytes), vec![(10, &b"first"[..])]);
        assert!(records(b"junk that is no trail at all").is_empty());
    }

    #[test]
    fn an_entry_survives_its_plain_form() {
        let plain = plain_of("msedge.exe", "A \"quoted\" title", "line one\nline two");
        assert_eq!(
            entry_of(7, &plain),
            Some(entry(
                7,
                "msedge.exe",
                "A \"quoted\" title",
                "line one\nline two"
            ))
        );
        assert_eq!(entry_of(7, b"not json"), None);
    }

    #[test]
    fn day_files_are_named_by_date() {
        assert_eq!(day_file(0), "1970-01-01.trail");
        assert_eq!(day_file(11_017), "2000-03-01.trail");
        assert_eq!(day_file(19_782), "2024-02-29.trail");
        for day in [0, 58, 59, 365, 11_016, 11_017, 19_782, 20_784, 40_000] {
            assert_eq!(day_of_file(&day_file(day)), Some(day));
        }
        assert_eq!(day_of_file("asked.log"), None);
        assert_eq!(day_of_file("2026-02-30.trail"), None);
        assert_eq!(day_of_file("2026-2-03.trail"), None);
        assert_eq!(day_of_file("2026-11-27-1.trail"), None);
        assert_eq!(day_of_file("2026-11-27.dat"), None);
    }

    #[test]
    fn a_day_goes_after_seven() {
        assert!(!too_old(100, 100));
        assert!(!too_old(93, 100));
        assert!(too_old(92, 100));
        // A file dated after today, from a clock that was wrong, stays.
        assert!(!too_old(101, 100));
    }

    #[test]
    fn only_new_lines_are_fresh() {
        let mut seen = Seen::default();
        assert_eq!(seen.fresh("Hi Anna\n\n  Thanks  \n"), "Hi Anna\nThanks");
        assert_eq!(seen.fresh("Hi Anna\nThanks"), "");
        assert_eq!(
            seen.fresh("Hi Anna\nThanks\nSee you Friday\nThanks"),
            "See you Friday"
        );
        // Scrolled away and back: still known.
        assert_eq!(seen.fresh("Other page"), "Other page");
        assert_eq!(seen.fresh("Hi Anna"), "");
    }

    #[test]
    fn what_is_seen_starts_over_but_keeps_the_screen() {
        let mut seen = Seen::default();
        let first: String = (0..SEEN_MOST).map(|i| format!("line {i}\n")).collect();
        seen.fresh(&first);
        assert_eq!(seen.fresh("new a\nnew b"), "new a\nnew b");
        assert_eq!(seen.fresh("new a\nnew b"), "");
        assert_eq!(seen.fresh("line 1"), "line 1");
    }

    #[test]
    fn ranking_goes_by_words_then_weight_then_age() {
        let entries = vec![
            entry(100, "winword.exe", "Budget 2027", "numbers"),
            entry(200, "msedge.exe", "News", "the budget was mentioned once"),
            entry(300, "outlook.exe", "Inbox", "nothing here"),
            entry(400, "msedge.exe", "Budget talk", "the offer and the budget"),
            entry(500, "msedge.exe", "News", "the budget was mentioned once"),
        ];
        let found = rank(entries.clone(), "Budget offer", 10);
        let at: Vec<u64> = found.iter().map(|e| e.at).collect();
        // Both words beat one, the title beats the text, newest wins a tie.
        assert_eq!(at, vec![400, 100, 500, 200]);
        assert_eq!(rank(entries.clone(), "budget", 2).len(), 2);
        assert!(rank(entries.clone(), "a ?", 10).is_empty());
        assert_eq!(rank(entries, "OUTLOOK", 10)[0].at, 300);
    }

    #[test]
    fn a_word_said_often_counts_three_times() {
        let words = words("offer");
        let often = entry(1, "a.exe", "", "offer offer offer offer offer");
        let titled = entry(1, "a.exe", "The offer", "offer");
        assert_eq!(score(&often, &words), (1, 3));
        assert_eq!(score(&titled, &words), (1, 4));
    }

    #[test]
    fn ages_read_plainly() {
        assert_eq!(age(0), "just now");
        assert_eq!(age(59), "just now");
        assert_eq!(age(12 * 60 + 5), "12 min ago");
        assert_eq!(age(2 * 3_600), "2 h ago");
        assert_eq!(age(2 * 3_600 + 180), "2 h 3 min ago");
        assert_eq!(age(DAY + 5), "1 day ago");
        assert_eq!(age(3 * DAY), "3 days ago");
    }

    #[test]
    fn rendering_shows_age_program_title_then_the_text() {
        let now = 10_000;
        let entries = [
            entry(now - 720, "OUTLOOK.EXE", "Inbox", ""),
            entry(now - 30, "msedge.exe", "Offer", "Dear Anna\nThe price is 12"),
        ];
        assert_eq!(
            render(&entries, now, 10_000),
            "12 min ago, OUTLOOK.EXE, Inbox\n\
             just now, msedge.exe, Offer\n  Dear Anna\n  The price is 12\n"
        );
    }

    #[test]
    fn rendering_keeps_the_newest_in_the_order_given() {
        let now = 10_000;
        let entries = [
            entry(now - 60, "b.exe", "Newest", "bbbb"),
            entry(now - 600, "a.exe", "Oldest", "aaaa"),
            entry(now - 120, "c.exe", "Middle", "cccc"),
        ];
        let one = block_of(&entries[0], now).chars().count();
        let two = one + block_of(&entries[2], now).chars().count();
        let out = render(&entries, now, two);
        assert!(out.contains("Newest") && out.contains("Middle") && !out.contains("Oldest"));
        assert!(out.find("Newest") < out.find("Middle"));
        // Too little for even the newest: its start, and no more.
        let cut = render(&entries, now, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.starts_with("1 min ago"));
        assert_eq!(render(&entries, now, 0), "");
    }

    #[test]
    fn an_asking_is_one_line() {
        assert_eq!(
            asked_line(
                "session abc",
                "the human asked",
                "last 10\nminutes",
                DAY + 3_723
            ),
            "1970-01-02 01:02:03 UTC, session abc, for: the human asked, asked: last 10 minutes\n"
        );
        let long = "x".repeat(ASKED_MOST * 2);
        let line = asked_line("a", "b\r\nfake line", &long, 0);
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.chars().count() < ASKED_MOST + 80);
        let line = asked_line("a", &long, "the last minute", 0);
        assert!(line.chars().count() < ASKED_MOST + 80);
        assert!(line.ends_with("asked: the last minute\n"));
    }
}
