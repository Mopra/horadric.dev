//! What the mouse and keys do to the transcript: clicks on its buttons
//! and links, a drag that selects text across blocks, a double click that
//! takes a word, Ctrl+C, the find bar (Ctrl+F, Enter and Shift+Enter),
//! Alt+Up and Alt+Down between prompts, the fields on cards, and the
//! hover that lights a button.

use std::collections::BTreeSet;

use horadric_core::folio::{Block, Reply};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DELETE, VK_DOWN,
    VK_END, VK_F3, VK_HOME, VK_LEFT, VK_RIGHT, VK_UP,
};

use super::cards::picks_of;
use super::flow::{self, Pos};
use super::transcript::{deny_field, keep_field, other_field, Act, Find, State, FIND_FIELD, SHOWN};
use super::view::View;
use super::{answer_with, redraw, with, Chat};
use crate::app::{self, Input};
use crate::clipboard;
use crate::field::Field;
use crate::keys::Mods;

/// A click on one of the transcript's own buttons.
pub(super) fn act(key: &str, a: Act) {
    match a {
        Act::Copy(t) => clipboard::set_text(&t),
        Act::Link(url) => app::push(Input::Link(url)),
        Act::Path(path, line) => {
            // What a reply names is the model's word: only a file inside
            // the project opens.
            if let Some((project, dir)) = with(key, |c| (c.key.clone(), c.dir.clone())) {
                if let Some(rel) = flow::project_path(&dir, &path) {
                    app::push(Input::View(project, dir, rel, line.map(|n| n as usize)));
                }
            }
        }
        Act::Image(block, k) => {
            let image = with(key, |c| match c.folio.blocks.get(block) {
                Some(Block::Images(v)) => v.get(k).cloned(),
                _ => None,
            })
            .flatten();
            if let Some(img) = image {
                super::lightbox::open(key, &img.base64);
            }
        }
        Act::Always(id) => answer_with(key, |f| f.reply(&id, Reply::Always)),
        Act::Deny(id) => {
            let field = deny_field(&id);
            let reason = with(key, |c| {
                let st = &mut c.tr.state;
                if st.denying.insert(id.clone()) {
                    st.focus = Some(field.clone());
                    None
                } else {
                    st.denying.remove(&id);
                    st.focus = None;
                    Some(st.fields.remove(&field).map(|f| f.text).unwrap_or_default())
                }
            })
            .flatten();
            if let Some(reason) = reason {
                answer_with(key, |f| f.reply(&id, Reply::Deny(&reason)));
            }
        }
        Act::Keep(id) => {
            let field = keep_field(&id);
            let feedback = with(key, |c| {
                let st = &mut c.tr.state;
                if st.keeping.insert(id.clone()) {
                    st.focus = Some(field.clone());
                    None
                } else {
                    st.keeping.remove(&id);
                    st.focus = None;
                    Some(st.fields.remove(&field).map(|f| f.text).unwrap_or_default())
                }
            })
            .flatten();
            if let Some(feedback) = feedback {
                answer_with(key, |f| f.keep_planning(&id, feedback.trim()));
            }
        }
        Act::Cancel(id) => {
            with(key, |c| {
                let st = &mut c.tr.state;
                st.denying.remove(&id);
                st.keeping.remove(&id);
                st.focus = None;
            });
        }
        Act::Approve(id, mode) => answer_with(key, |f| f.approve_plan(&id, mode)),
        Act::Pick(id, q, o) => {
            let submit = with(key, |c| pick(c, &id, q, o)).unwrap_or(false);
            if submit {
                submit_question(key, &id);
            }
        }
        Act::Submit(id) => submit_question(key, &id),
        Act::Field(id) => {
            with(key, |c| {
                if id == FIND_FIELD && c.tr.state.find.is_none() {
                    return;
                }
                c.tr.state.focus = Some(id);
            });
        }
        Act::Step(id) => {
            with(key, |c| toggle(&mut c.tr.state.flipped, id));
        }
        Act::Agent(i, id) => {
            with(key, |c| {
                c.open.insert(i);
                if let Some(Block::Tools(steps)) = c.folio.blocks.get(i) {
                    super::transcript::open_path(&mut c.tr.state, steps, &[id]);
                }
                c.follow = false;
            });
        }
        Act::All(id) => {
            with(key, |c| toggle(&mut c.tr.state.all, id));
        }
        Act::Earlier => {
            with(key, |c| {
                let turns: Vec<usize> = (c.folio.blocks.iter().enumerate())
                    .filter(|(_, b)| matches!(b, Block::Prompt(_)))
                    .map(|(i, _)| i)
                    .collect();
                let from = c.tr.state.from.unwrap_or(0);
                c.tr.state.from = Some(flow::shown_from(&turns, from, SHOWN));
                c.follow = false;
            });
        }
        Act::Todos => {
            with(key, |c| c.tr.state.todos_open = !c.tr.state.todos_open);
        }
        Act::FindPrev => jump(key, false),
        Act::FindNext => jump(key, true),
        Act::FindClose => {
            with(key, |c| {
                c.tr.state.find = None;
                c.tr.state.focus = None;
            });
        }
    }
    redraw(key);
}

fn toggle(set: &mut std::collections::HashSet<String>, id: String) {
    if !set.remove(&id) {
        set.insert(id);
    }
}

/// Picks or unpicks an option. True when that answers the card: a single
/// question with one answer and nothing typed of one's own.
fn pick(c: &mut Chat, id: &str, q: usize, o: usize) -> bool {
    let Some((count, multi)) = c.folio.blocks.iter().find_map(|b| match b {
        Block::Question {
            request_id,
            questions,
            ..
        } if request_id == id => Some((
            questions.len(),
            questions.get(q).is_some_and(|q| q.multi_select),
        )),
        _ => None,
    }) else {
        return false;
    };
    let st = &mut c.tr.state;
    let picks = st
        .picks
        .entry(id.to_string())
        .or_insert_with(|| vec![BTreeSet::new(); count]);
    let Some(set) = picks.get_mut(q) else {
        return false;
    };
    if multi {
        if !set.remove(&o) {
            set.insert(o);
        }
        return false;
    }
    set.clear();
    set.insert(o);
    st.fields.remove(&other_field(id, q));
    count == 1
}

fn submit_question(key: &str, id: &str) {
    let picks = with(key, |c| {
        let b = c
            .folio
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Question { request_id, .. } if request_id == id))?;
        let picks = picks_of(b, &c.tr.state);
        picks.iter().all(|p| !p.is_empty()).then_some(picks)
    })
    .flatten();
    if let Some(picks) = picks {
        answer_with(key, |f| f.answer_question(id, &picks));
        with(key, |c| c.tr.state.focus = None);
    }
}

/// Any click takes the keyboard back from a card's field and ends the
/// selection; a click on a field or on text gives them again. With Shift
/// held the selection stays, for the click to extend it.
pub(super) fn unfocus(key: &str, extend: bool) {
    with(key, |c| {
        c.tr.state.focus = None;
        if !extend {
            c.tr.state.sel = None;
        }
    });
}

/// The text under a point, as a place in it.
fn pos_at(c: &Chat, x: f32, y: f32) -> Option<Pos> {
    let st = &c.tr.state;
    let rects: Vec<(f32, f32, f32, f32)> = (st.shown.iter())
        .map(|(r, _, _)| (r.x, r.y, r.w, r.h))
        .collect();
    let k = flow::nearest(&rects, x, y)?;
    let (r, block, unit) = st.shown[k];
    let u = c.tr.cache.unit(block, unit)?;
    let at = crate::render::offset_at(&u.layout, x - r.x, y - r.y);
    Some(Pos { block, unit, at })
}

/// A press on the conversation where no button is: starts a selection, or
/// with a double click takes the word. False when it is not over text.
pub(super) fn press(view: &View, x: f32, y: f32, double: bool, extend: bool) -> bool {
    let key = &view.key;
    let started = with(key, |c| {
        if !c.tr.state.area.contains(x, y) {
            c.tr.state.sel = None;
            return false;
        }
        let Some(p) = pos_at(c, x, y) else {
            c.tr.state.sel = None;
            return false;
        };
        let st = &mut c.tr.state;
        if double {
            let text = c.tr.cache.unit(p.block, p.unit).map(|u| u.text.clone());
            if let Some(text) = text {
                let w = flow::word_at(&text, p.at);
                st.sel = Some((Pos { at: w.start, ..p }, Pos { at: w.end, ..p }));
            }
            return true;
        }
        st.sel = match (extend, st.sel) {
            (true, Some((a, _))) => Some((a, p)),
            _ => Some((p, p)),
        };
        st.dragging = true;
        true
    })
    .unwrap_or(false);
    if started && !double {
        unsafe {
            SetCapture(view.hwnd());
        }
    }
    redraw(key);
    started
}

/// The pointer moved: the selection follows a drag, and what it is over
/// lights up. Redraws only when either changed.
pub(super) fn moved(view: &View, x: f32, y: f32) -> bool {
    let key = &view.key;
    // Told when the pointer leaves, so nothing stays lit.
    let mut track = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: view.hwnd(),
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut track);
    }
    let lit = view.hit_rect(x, y);
    let changed = with(key, |c| {
        let was_block = block_at(c, c.tr.state.mouse);
        let was_lit = c.tr.state.mouse.and_then(|(mx, my)| view.hit_rect(mx, my));
        c.tr.state.mouse = Some((x, y));
        let mut changed = was_lit != lit || was_block != block_at(c, Some((x, y)));
        if c.tr.state.dragging {
            if let (Some(p), Some((a, b))) = (pos_at(c, x, y), c.tr.state.sel) {
                if p != b {
                    c.tr.state.sel = Some((a, p));
                    changed = true;
                }
            }
            // Past the top or bottom, the conversation scrolls along.
            let area = c.tr.state.area;
            let by = if y < area.y {
                y - area.y
            } else if y > area.bottom() {
                y - area.bottom()
            } else {
                0.0
            };
            if by != 0.0 {
                c.scroll = crate::folio_look::clamp_scroll(c.scroll + by, c.content_h, c.view_h);
                c.follow = false;
                changed = true;
            }
        }
        changed
    })
    .unwrap_or(false);
    if changed {
        redraw(key);
    }
    with(key, |c| c.tr.state.dragging).unwrap_or(false)
}

fn block_at(c: &Chat, at: Option<(f32, f32)>) -> Option<usize> {
    let (_, y) = at?;
    let st = &c.tr.state;
    let top = st.area.y - c.scroll;
    let k = st.tops.partition_point(|t| top + t <= y);
    k.checked_sub(1)
}

/// The pointer left the window: nothing is lit.
pub(super) fn left(key: &str) {
    let had = with(key, |c| c.tr.state.mouse.take().is_some()).unwrap_or(false);
    if had {
        redraw(key);
    }
}

/// The button came up: a drag ends. True when one was going.
pub(super) fn release(key: &str) -> bool {
    let was = with(key, |c| {
        let st = &mut c.tr.state;
        // A click without a drag leaves nothing selected.
        if let Some((a, b)) = st.sel {
            if a == b {
                st.sel = None;
            }
        }
        std::mem::replace(&mut st.dragging, false)
    })
    .unwrap_or(false);
    if was {
        unsafe {
            let _ = ReleaseCapture();
        }
        redraw(key);
    }
    was
}

/// Whether text that can be selected is under the point, for the cursor.
pub(super) fn over_text(key: &str, x: f32, y: f32) -> bool {
    with(key, |c| {
        (c.tr.state.shown.iter()).any(|(r, _, _)| r.contains(x, y))
    })
    .unwrap_or(false)
}

/// The selection as text, if any.
fn selected(c: &Chat) -> Option<String> {
    let (a, b) = c.tr.state.sel?;
    let s = flow::copy_text(&c.tr.cache.texts(), a, b);
    (!s.is_empty()).then_some(s)
}

/// A typed character, before the composer gets it. True when the
/// transcript took it: Ctrl+F, Ctrl+C on a selection, or typing into the
/// find bar or a card's field.
pub(super) fn char(key: &str, c: char, mods: Mods) -> bool {
    if c == '\u{6}' {
        with(key, |chat| {
            let st = &mut chat.tr.state;
            if st.find.is_none() {
                st.find = Some(Find {
                    field: Field::new("", false, 500),
                    hits: Vec::new(),
                    at: 0,
                    for_: (String::new(), usize::MAX),
                });
            }
            st.focus = Some(FIND_FIELD.to_string());
            if let Some(f) = &mut st.find {
                f.field.select_all();
            }
        });
        redraw(key);
        return true;
    }
    let focus = with(key, |chat| chat.tr.state.focus.clone()).flatten();
    let Some(focus) = focus else {
        if c == '\u{3}' {
            // A selection in the conversation lasts until the next click,
            // so while there is one, Ctrl+C is the conversation's.
            let copied = with(key, |chat| match selected(chat) {
                Some(s) => {
                    clipboard::set_text(&s);
                    true
                }
                None => false,
            });
            return copied.unwrap_or(false);
        }
        if c == '\u{1b}' {
            // Esc closes the find bar or clears the selection before it
            // stops Claude: a second Esc stops it.
            let took = with(key, |chat| {
                let st = &mut chat.tr.state;
                st.find.take().is_some() || st.sel.take().is_some()
            })
            .unwrap_or(false);
            if took {
                redraw(key);
            }
            return took;
        }
        return false;
    };
    match c {
        '\r' => {
            let shift = mods.shift;
            submit(key, &focus, shift);
        }
        '\u{1b}' => {
            with(key, |chat| {
                let st = &mut chat.tr.state;
                if focus == FIND_FIELD {
                    st.find = None;
                } else if let Some(id) = focus.strip_prefix("deny:") {
                    st.denying.remove(id);
                } else if let Some(id) = focus.strip_prefix("keep:") {
                    st.keeping.remove(id);
                }
                st.focus = None;
            });
        }
        _ => {
            with(key, |chat| {
                let Some(f) = field_mut(chat, &focus) else {
                    return;
                };
                match c {
                    '\u{8}' => f.backspace(false),
                    '\u{7f}' => f.backspace(true),
                    '\u{1}' => f.select_all(),
                    '\u{3}' => {
                        if !f.selected().is_empty() {
                            clipboard::set_text(f.selected());
                        }
                    }
                    '\u{18}' => {
                        if !f.selected().is_empty() {
                            clipboard::set_text(&f.cut());
                        }
                    }
                    '\u{16}' => {
                        if let Some(t) = clipboard::get_text() {
                            f.insert(&t);
                        }
                    }
                    c if (c as u32) < 0x20 => {}
                    c => f.insert(c.encode_utf8(&mut [0; 4])),
                }
            });
            if focus == FIND_FIELD {
                refind(key);
                jump_to_current(key);
            }
        }
    }
    redraw(key);
    true
}

/// A card's field gives the keyboard back once its ask no longer waits,
/// answered elsewhere, withdrawn or lapsed at the turn's end, so typing
/// goes to the composer again.
pub(super) fn release_lapsed(chat: &mut Chat) {
    let st = &mut chat.tr.state;
    let Some(focus) = st.focus.as_deref() else {
        return;
    };
    if focus == FIND_FIELD {
        return;
    }
    let id = focus.split_once(':').map_or("", |(_, id)| id);
    if chat.folio.pending_asks().contains(&id) {
        return;
    }
    let id = id.to_string();
    st.denying.remove(&id);
    st.keeping.remove(&id);
    st.focus = None;
}

fn field_mut<'a>(chat: &'a mut Chat, id: &str) -> Option<&'a mut Field> {
    let st = &mut chat.tr.state;
    if id == FIND_FIELD {
        return st.find.as_mut().map(|f| &mut f.field);
    }
    Some(st.field(id))
}

/// Enter in a field: the find bar goes to the next match (Shift, the
/// one before), a card's field sends what it holds.
fn submit(key: &str, focus: &str, shift: bool) {
    if focus == FIND_FIELD {
        jump(key, !shift);
    } else if let Some(id) = focus.strip_prefix("deny:") {
        act(key, Act::Deny(id.to_string()));
    } else if let Some(id) = focus.strip_prefix("keep:") {
        act(key, Act::Keep(id.to_string()));
    } else if let Some((_, id)) = focus.split_once(':') {
        act(key, Act::Submit(id.to_string()));
    }
}

/// A key that makes no character, before the composer gets it. True
/// when the transcript took it.
pub(super) fn key(key: &str, vk: u16, mods: Mods) -> bool {
    if mods.alt && !mods.ctrl && (vk == VK_UP.0 || vk == VK_DOWN.0) {
        to_prompt(key, vk == VK_UP.0);
        return true;
    }
    let (find_open, focus) = with(key, |c| {
        (c.tr.state.find.is_some(), c.tr.state.focus.clone())
    })
    .unwrap_or_default();
    if vk == VK_F3.0 && find_open {
        jump(key, !mods.shift);
        return true;
    }
    let Some(focus) = focus else {
        return false;
    };
    if focus == FIND_FIELD && (vk == VK_UP.0 || vk == VK_DOWN.0) {
        jump(key, vk == VK_DOWN.0);
        return true;
    }
    let (word, extend) = (mods.ctrl, mods.shift);
    let done = with(key, |c| {
        let Some(f) = field_mut(c, &focus) else {
            return false;
        };
        match vk {
            v if v == VK_LEFT.0 => f.left(word, extend),
            v if v == VK_RIGHT.0 => f.right(word, extend),
            v if v == VK_HOME.0 => f.home(word, extend),
            v if v == VK_END.0 => f.end(word, extend),
            v if v == VK_DELETE.0 => f.delete(word),
            _ => return false,
        }
        true
    })
    .unwrap_or(false);
    if done {
        if focus == FIND_FIELD && vk == VK_DELETE.0 {
            refind(key);
        }
        redraw(key);
    }
    done
}

/// Finds the query again over the transcript's text.
pub(super) fn refind(key: &str) {
    with(key, |c| refind_in(&mut c.tr.state, &c.tr.cache));
}

/// Finds again when the query or the text changed since.
pub(super) fn refind_in(st: &mut State, cache: &super::transcript::Cache) {
    let Some(f) = &mut st.find else {
        return;
    };
    let texts = cache.texts();
    let size: usize = texts.iter().map(|t| t.text.len()).sum();
    let now = (f.field.text.clone(), size);
    if f.for_ == now {
        return;
    }
    let same_query = f.for_.0 == now.0;
    f.hits = flow::find_all(&texts, &f.field.text);
    if !same_query || f.at >= f.hits.len() {
        f.at = 0;
    }
    f.for_ = now;
}

/// The next match, or the one before, scrolled into view.
fn jump(key: &str, forward: bool) {
    refind(key);
    with(key, |c| {
        if let Some(f) = &mut c.tr.state.find {
            let n = f.hits.len();
            if n > 0 {
                f.at = if forward {
                    (f.at + 1) % n
                } else {
                    (f.at + n - 1) % n
                };
            }
        }
    });
    jump_to_current(key);
    redraw(key);
}

/// Scrolls so the current match sits a third down the view.
fn jump_to_current(key: &str) {
    with(key, |c| {
        let st = &c.tr.state;
        let Some(h) = st.find.as_ref().and_then(|f| f.hits.get(f.at)) else {
            return;
        };
        let Some(top) = st.tops.get(h.block) else {
            return;
        };
        let Some(u) = c.tr.cache.unit(h.block, h.unit) else {
            return;
        };
        let r = crate::render::range_rects(&u.layout, h.range.start, h.range.end - h.range.start);
        let y = top + u.y + r.first().map_or(0.0, |b| b.y);
        let target = y - c.view_h / 3.0;
        c.scroll = crate::folio_look::clamp_scroll(target, c.content_h, c.view_h);
        c.follow = false;
    });
}

/// Scrolls to the prompt above or below the top of the view.
fn to_prompt(key: &str, up: bool) {
    with(key, |c| {
        let st = &c.tr.state;
        let tops: Vec<f32> = (st.prompts.iter())
            .filter_map(|&i| st.tops.get(i).map(|t| t - 12.0))
            .collect();
        if let Some(t) = flow::next_prompt(&tops, c.scroll, up) {
            c.scroll = crate::folio_look::clamp_scroll(t, c.content_h, c.view_h);
            c.follow = crate::folio_look::at_bottom(c.scroll, c.content_h, c.view_h);
        } else if !up {
            c.scroll = (c.content_h - c.view_h).max(0.0);
            c.follow = true;
        }
    });
    redraw(key);
}
