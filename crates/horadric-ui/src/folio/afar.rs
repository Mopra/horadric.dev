//! A Folio as the Town Portal serves it, to a phone or a linked brother:
//! a screen of text lines, a raw stream that paints those lines, and
//! their typing taken in. The rules are `horadric_core::folio::afar`'s;
//! here is what each session keeps between their messages, and the CLI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use horadric_core::ask::Ask;
use horadric_core::folio::afar::{self as rules, Typed};

use super::{composer, redraw, select, with};

/// How long a rendered screen is used again: the raw streams look every
/// 20 ms, and a long conversation need not be laid out that often.
const FRESH_FOR: Duration = Duration::from_millis(200);
/// The size a Folio's screen says it has until a brother asks for one.
const SIZE: (u16, u16) = (100, 40);

/// What one Folio keeps for its viewers elsewhere.
#[derive(Default)]
struct Afar {
    /// Typed from afar, not yet sent with Enter.
    draft: String,
    /// The size a brother holds, drawing it in a grid of its own.
    size: Option<(u16, u16)>,
    /// The lines last rendered and when.
    shown: Option<(Instant, Vec<String>)>,
    /// Where the raw stream's paint of those lines starts and ends.
    /// Each new paint starts past the last one's end, so a stream that
    /// asks from the old end gets a replay.
    paint: (u64, u64),
    /// The fingerprint of the last paint.
    painted: u64,
}

thread_local! {
    static AFAR: RefCell<HashMap<String, Afar>> = RefCell::new(HashMap::new());
}

fn afar<T>(id: &str, f: impl FnOnce(&mut Afar) -> T) -> T {
    AFAR.with(|a| f(a.borrow_mut().entry(id.to_string()).or_default()))
}

/// What a phone or a brother typed into session `id`.
pub fn typed(id: &str, data: &str) {
    let mut draft = afar(id, |a| std::mem::take(&mut a.draft));
    with(id, |c| {
        if c.ended || c.to_terminal || c.hosted.is_none() || c.loading.is_some() {
            return;
        }
        for act in rules::typed(&mut c.folio, &mut draft, data) {
            let Some(h) = &c.hosted else {
                return;
            };
            match act {
                Typed::Write(line) => {
                    if let Err(e) = h.send_line(&line) {
                        c.error = Some(format!("That did not go through: {e}"));
                    }
                }
                Typed::Start(line) => {
                    if let Err(e) = h.send_line(&line) {
                        c.error = Some(format!("That did not go through: {e}"));
                        // No turn started, so none will end with a result.
                        c.folio.end();
                    }
                    c.follow = true;
                }
                Typed::Interrupt => {
                    let _ = h.interrupt();
                    composer::hold(c);
                }
            }
        }
        select::release_lapsed(c);
        c.note_busy();
    });
    afar(id, |a| {
        a.draft = draft;
        a.shown = None;
    });
    redraw(id);
}

/// The conversation as screen lines, with what was typed from afar.
pub fn lines(id: &str) -> Option<Vec<String>> {
    if let Some(lines) = afar(id, |a| {
        a.shown
            .as_ref()
            .filter(|(at, _)| at.elapsed() < FRESH_FOR)
            .map(|(_, l)| l.clone())
    }) {
        return Some(lines);
    }
    let draft = afar(id, |a| a.draft.clone());
    let lines = with(id, |c| rules::lines(&c.folio, &draft))?;
    afar(id, |a| a.shown = Some((Instant::now(), lines.clone())));
    Some(lines)
}

/// The request that waits on the human, as a phone's `ask`.
pub fn ask(id: &str, since: SystemTime) -> Option<Ask> {
    with(id, |c| rules::ask(&c.folio, since)).flatten()
}

pub fn size(id: &str) -> (u16, u16) {
    afar(id, |a| a.size).unwrap_or(SIZE)
}

pub fn hold_size(id: &str, size: Option<(u16, u16)>) {
    afar(id, |a| a.size = size);
}

/// The raw stream after `at`: nothing while the paint is the same, None
/// once it changed, for a replay.
pub fn output_since(id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
    let (_, end) = paint(id)?;
    (at == end).then_some((Vec::new(), end))
}

/// The paint of the screen as a terminal's output: cleared, then the
/// lines, with where it starts.
pub fn output_replay(id: &str) -> (u64, Vec<u8>) {
    let (Some((start, _)), Some(lines)) = (paint(id), lines(id)) else {
        return (0, Vec::new());
    };
    (start, vt(&lines))
}

/// Where the current paint starts and ends, moved on when it changed.
fn paint(id: &str) -> Option<(u64, u64)> {
    let lines = lines(id)?;
    let bytes = vt(&lines);
    let print = horadric_core::portal::fingerprint(&serde_json::json!(lines));
    Some(afar(id, |a| {
        if a.painted != print || a.paint == (0, 0) {
            a.painted = print;
            let start = a.paint.1 + 1;
            a.paint = (start, start + bytes.len() as u64);
        }
        a.paint
    }))
}

fn vt(lines: &[String]) -> Vec<u8> {
    let mut out = String::from("\u{1b}[H\u{1b}[2J");
    out.push_str(&lines.join("\r\n"));
    out.into_bytes()
}
