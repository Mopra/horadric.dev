//! MCP in the Folio: the line above the composer naming a server that
//! needs the human, the list of all of them on `/mcp` with sign in,
//! reconnect, turn off and remove, and the card an MCP server's question
//! (an elicitation) becomes. The
//! status is asked for once a session is live, and again every couple of
//! seconds while a sign-in or a starting server waits, since the CLI
//! says nothing when a status changes.

use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};

use horadric_core::folio::{
    AskState, Block, ElicitAction, Elicitation, FieldKind, McpCard, McpRemoval, McpServer, McpTold,
    McpWant, ELICIT_SEP,
};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, SW_SHOWNORMAL};

use super::cards::{button, field, plate, BUTTON_H, PAD};
use super::rich::{Ctx, Scene, F};
use super::transcript::{Act, State};
use super::view::{Gfx, Hit, Palette};
use super::{answer_with, redraw, router, with, Chat, CHATS};
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

/// The router's timer that asks for status again.
pub(super) const MCP_TIMER: usize = 41;
const POLL_MS: u32 = 2000;

/// Before an elicitation's field ids: `elicit:<request id>:<field>`.
pub(super) const FIELD: &str = "elicit:";

/// What a click on an MCP card does.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Do {
    SignIn(String),
    /// The sign-in page of a server again, for a browser that closed it.
    Page(String),
    StopSignIn(String),
    Reconnect(String),
    Toggle(String, bool),
    /// Remove a server for good: asked to be sure first, then done.
    AskRemove(String),
    KeepIt(String),
    Remove(String),
    /// Signing in only the terminal's `/mcp` can do.
    Terminal,
    /// The list at a block: closed to a line, or opened again.
    Close(usize),
    /// A group of the list at a block: folded or unfolded.
    Group(usize, Group),
    /// A server's lesser actions in the list at a block: shown or not.
    More(usize, String),
    /// The list, from the line above the composer.
    Show,
    /// The line above the composer: waved away.
    Dismiss,
    /// An elicitation, by request id.
    Accept(String),
    Decline(String),
    Cancel(String),
    /// A url elicitation's page: opened, and accepted.
    Open(String),
    /// A field of an elicitation: a box ticked or not.
    Flip(String, String),
    /// A field of an elicitation: an option picked (or, for one taking
    /// several, picked or unpicked).
    Pick(String, String, String),
}

/// A session key, a server, and what removing it said.
type Removal = (String, String, Result<(), String>);

/// What removals said, for the UI thread.
static REMOVED: Mutex<Vec<Removal>> = Mutex::new(Vec::new());

/// Removes `server` where it is configured, on a thread since the CLI
/// takes a second or two. Its answer comes back through [`settle`].
fn remove(key: &str, server: &str) {
    let found = with(key, |c| {
        let m = c.folio.mcp.servers.iter().find(|m| m.name == server)?;
        Some((m.removal()?, c.dir.clone()))
    });
    let Some((removal, dir)) = found.flatten() else {
        return;
    };
    let args: Vec<String> = match removal {
        // Removing it on claude.ai needs a signed in browser, and a
        // connector that needs sign-in is the one the human wants gone.
        // Denying it in their own settings is local and undone the same way.
        McpRemoval::ClaudeAi => {
            with(key, |c| c.folio.mcp_removing(server));
            let result = deny_connector(server);
            if let Ok(mut r) = REMOVED.lock() {
                r.push((key.to_string(), server.to_string(), result));
            }
            super::sign_in::post(router());
            return;
        }
        McpRemoval::Plugin(p) => vec!["plugin".into(), "disable".into(), p, "--json".into()],
        McpRemoval::Scope(scope) => {
            vec![
                "mcp".into(),
                "remove".into(),
                server.into(),
                "-s".into(),
                scope,
            ]
        }
    };
    with(key, |c| c.folio.mcp_removing(server));
    let (key, server, router) = (key.to_string(), server.to_string(), router());
    std::thread::spawn(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = run_removal(&args, &dir);
        if let Ok(mut r) = REMOVED.lock() {
            r.push((key, server, result));
        }
        super::sign_in::post(router);
    });
}

/// Adds `server` to `deniedMcpServers` in the user's settings, so no
/// session in any project loads it.
fn deny_connector(server: &str) -> Result<(), String> {
    let path = crate::inventory::claude_dir().join("settings.json");
    let mut settings: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not JSON ({e}); fix it first", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("{} cannot be read ({e})", path.display())),
    };
    let switch = horadric_core::context::Switch::Server(server.to_string());
    horadric_core::context::set(&mut settings, &switch, false);
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

fn run_removal(args: &[&str], dir: &std::path::Path) -> Result<(), String> {
    let out = super::sign_in::command(args)?
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("claude did not start: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    // `plugin disable --json`: a plugin already off is as good as done.
    let json = (said.lines()).find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok());
    if out.status.success()
        || json
            .as_ref()
            .is_some_and(|v| v["alreadyInGoalState"] == true)
    {
        return Ok(());
    }
    let why = json
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = (err.lines().chain(said.lines()))
                .map(str::trim)
                .rfind(|l| !l.is_empty());
            last.unwrap_or("It could not be removed.").to_string()
        });
    Err(why)
}

/// Takes in what removals said: each removed server is turned off in its
/// session too.
pub(super) fn settle() {
    let done = REMOVED
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    let mut later = Vec::new();
    for (key, server, result) in done {
        let lines = with(&key, |c| c.folio.mcp_removed(&server, result.clone()));
        match lines {
            Some(lines) => {
                with(&key, |c| lines.iter().for_each(|l| send(c, l)));
                redraw(&key);
            }
            // The chats are held: try again on the next wake.
            None => later.push((key, server, result)),
        }
    }
    if let Ok(mut r) = REMOVED.lock() {
        r.extend(later);
    }
}

/// What every session here knows together of servers in trouble, read
/// once from the state folder.
pub(super) fn told() -> Arc<Mutex<McpTold>> {
    static TOLD: OnceLock<Arc<Mutex<McpTold>>> = OnceLock::new();
    TOLD.get_or_init(|| {
        let read = told_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Arc::new(Mutex::new(read.unwrap_or_default()))
    })
    .clone()
}

fn told_path() -> Option<std::path::PathBuf> {
    Some(crate::store::dir()?.join("mcp-told.json"))
}

/// Writes [`told`] when a session changed it.
fn keep_told() {
    let told = told();
    let Ok(mut t) = told.lock() else {
        return;
    };
    if !t.dirty {
        return;
    }
    t.dirty = false;
    let (Some(path), Ok(text)) = (told_path(), serde_json::to_string_pretty(&*t)) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn field_id(request_id: &str, name: &str) -> String {
    format!("{FIELD}{request_id}:{name}")
}

/// Where a failed accept says why, kept with the card's fields.
fn error_id(request_id: &str) -> String {
    format!("{FIELD}{request_id}")
}

/// Every field id an elicitation's card reads, for its stamp.
pub(super) fn field_ids(e: &Elicitation) -> Vec<String> {
    let mut ids: Vec<String> = (e.fields.iter())
        .map(|f| field_id(&e.request_id, &f.name))
        .collect();
    ids.push(error_id(&e.request_id));
    ids
}

/// Opens an address in the default browser: a sign-in is the human's
/// own, in the browser that holds their logins, not the browser pane.
fn open(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn send(chat: &mut Chat, line: &str) {
    if let Some(h) = &chat.hosted {
        if let Err(e) = h.send_line(line) {
            chat.error = Some(format!("That did not go through: {e}"));
        }
    }
}

/// `/mcp`: the servers as a card, with their status asked for again.
pub(super) fn show(chat: &mut Chat) {
    if chat.ended || chat.hosted.is_none() {
        return;
    }
    let line = chat.folio.show_mcp();
    send(chat, &line);
    chat.follow = true;
}

/// After each drain: opens the sign-in pages the CLI gave, and asks for
/// status when it is due.
pub(super) fn tend(chat: &mut Chat) {
    keep_told();
    let pages = chat.folio.take_mcp_pages();
    // A replay's pages were opened when they came.
    if !chat.live || chat.ended || chat.hosted.is_none() || chat.watch.is_some() {
        return;
    }
    for url in pages {
        open(&url);
    }
    for line in chat.folio.mcp_turn_offs() {
        send(chat, &line);
    }
    match chat.folio.mcp_wanted() {
        McpWant::Now => {
            let line = chat.folio.mcp_status();
            send(chat, &line);
        }
        McpWant::Soon => {
            let router = router();
            if router != 0 {
                unsafe {
                    SetTimer(
                        Some(HWND(router as *mut std::ffi::c_void)),
                        MCP_TIMER,
                        POLL_MS,
                        None,
                    );
                }
            }
        }
        McpWant::No => {}
    }
}

/// The timer: asks every session waiting on a status. Their answers set
/// it again while they still wait.
pub(super) fn tick(router: HWND) {
    unsafe {
        let _ = KillTimer(Some(router), MCP_TIMER);
    }
    CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return;
        };
        for chat in chats.values_mut() {
            let ready = chat.live && !chat.ended && chat.watch.is_none();
            if ready && chat.folio.mcp_wanted() != McpWant::No {
                for line in chat.folio.mcp_rechecks() {
                    send(chat, &line);
                }
                let line = chat.folio.mcp_status();
                send(chat, &line);
            }
        }
    });
}

pub(super) fn act(key: &str, d: Do) {
    match d {
        Do::SignIn(s) => answer_with(key, |f| Some(f.mcp_sign_in(&s))),
        Do::Page(s) => {
            let url = with(key, |c| {
                let found = c.folio.mcp.servers.iter().find(|m| m.name == s);
                found.and_then(|m| m.sign_in.clone())
            });
            if let Some(url) = url.flatten() {
                open(&url);
            }
        }
        Do::StopSignIn(s) => {
            with(key, |c| c.folio.mcp_sign_in_cancel(&s));
        }
        Do::Reconnect(s) => answer_with(key, |f| Some(f.mcp_reconnect(&s))),
        Do::Toggle(s, on) => answer_with(key, |f| Some(f.mcp_toggle(&s, on))),
        Do::AskRemove(s) => {
            with(key, |c| c.folio.mcp_ask_remove(&s, true));
        }
        Do::KeepIt(s) => {
            with(key, |c| c.folio.mcp_ask_remove(&s, false));
        }
        Do::Remove(s) => remove(key, &s),
        Do::Terminal => {
            with(key, |c| super::composer::to_terminal(c, "/mcp"));
        }
        Do::Close(i) => {
            with(key, |c| {
                if let Some(card) = c.folio.mcp_card(i) {
                    card.closed = !card.closed;
                }
            });
        }
        Do::Group(i, group) => {
            with(key, |c| {
                if let Some(card) = c.folio.mcp_card(i) {
                    match group {
                        Group::Connected => card.connected_open = !card.connected_open,
                        Group::Off => card.off_open = !card.off_open,
                    }
                }
            });
        }
        Do::More(i, name) => {
            with(key, |c| {
                if let Some(card) = c.folio.mcp_card(i) {
                    let shut = card.more.as_deref() == Some(name.as_str());
                    card.more = (!shut).then_some(name);
                }
            });
        }
        Do::Show => {
            with(key, show);
        }
        Do::Dismiss => {
            with(key, |c| c.folio.mcp_dismiss());
        }
        Do::Accept(id) => {
            let content = with(key, |c| {
                let e = elicitation_of(c, &id)?;
                let st = &c.tr.state;
                let made = e.content(|name| {
                    let t = st.text(&field_id(&id, name));
                    (!t.is_empty()).then(|| t.to_string())
                });
                match made {
                    Ok(content) => Some(content),
                    Err(why) => {
                        let f = c.tr.state.field(&error_id(&id));
                        f.text = why;
                        f.caret = f.text.len();
                        None
                    }
                }
            })
            .flatten();
            if let Some(content) = content {
                answer_with(key, |f| {
                    f.answer_elicitation(&id, ElicitAction::Accept, Some(content))
                });
                with(key, |c| c.tr.state.focus = None);
            }
        }
        Do::Decline(id) => answer_with(key, |f| {
            f.answer_elicitation(&id, ElicitAction::Decline, None)
        }),
        Do::Cancel(id) => answer_with(key, |f| {
            f.answer_elicitation(&id, ElicitAction::Cancel, None)
        }),
        Do::Open(id) => {
            let url = with(key, |c| elicitation_of(c, &id).and_then(|e| e.url.clone()));
            if let Some(url) = url.flatten() {
                open(&url);
                answer_with(key, |f| {
                    f.answer_elicitation(&id, ElicitAction::Accept, None)
                });
            }
        }
        Do::Flip(id, name) => {
            with(key, |c| {
                let on = current(c, &id, &name) == "true";
                set(
                    &mut c.tr.state,
                    &id,
                    &name,
                    if on { "false" } else { "true" },
                );
            });
        }
        Do::Pick(id, name, value) => {
            with(key, |c| {
                let many = elicitation_of(c, &id).is_some_and(|e| {
                    (e.fields.iter())
                        .any(|f| f.name == name && matches!(f.kind, FieldKind::Choices { .. }))
                });
                let now = if many {
                    let had = current(c, &id, &name);
                    let mut picked: Vec<&str> =
                        had.split(ELICIT_SEP).filter(|s| !s.is_empty()).collect();
                    match picked.iter().position(|p| *p == value) {
                        Some(i) => {
                            picked.remove(i);
                        }
                        None => picked.push(&value),
                    }
                    picked.join(&ELICIT_SEP.to_string())
                } else {
                    value.clone()
                };
                set(&mut c.tr.state, &id, &name, &now);
            });
        }
    }
    redraw(key);
}

fn elicitation_of<'a>(c: &'a Chat, id: &str) -> Option<&'a Elicitation> {
    c.folio.blocks.iter().rev().find_map(|b| match b {
        Block::Elicitation(e) if e.request_id == id => Some(e),
        _ => None,
    })
}

/// A field's text, its default until the human changed it.
fn current(c: &Chat, id: &str, name: &str) -> String {
    let t = c.tr.state.text(&field_id(id, name));
    if !t.is_empty() {
        return t.to_string();
    }
    elicitation_of(c, id)
        .and_then(|e| e.fields.iter().find(|f| f.name == name))
        .map(|f| f.default.clone())
        .unwrap_or_default()
}

fn set(st: &mut State, id: &str, name: &str, text: &str) {
    let f = st.field(&field_id(id, name));
    f.text = text.to_string();
    f.caret = f.text.len();
    st.fields.remove(&error_id(id));
}

/// A status as a word, and its ink.
fn status_of(cx: &Ctx, s: &McpServer) -> (String, Color) {
    match s.status.as_str() {
        "connected" if s.tools > 0 => (format!("connected, {} tools", s.tools), theme::done()),
        "connected" => ("connected".into(), theme::done()),
        "failed" => ("failed".into(), theme::waiting()),
        "needs-auth" => ("needs sign-in".into(), theme::waiting()),
        "pending" => ("starting\u{2026}".into(), cx.pal.dim),
        "disabled" => ("disabled".into(), cx.pal.faint),
        other => (other.to_string(), cx.pal.dim),
    }
}

/// A row of the list, and a group's heading.
const ROW: f32 = 28.0;
/// Where a row's name starts, after its dot.
const INDENT: f32 = 18.0;

/// A group of the list that folds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Group {
    Connected,
    Off,
}

/// A small button ending at `right`: a row of the list is one line, which
/// the cards' buttons are too tall for. Returns its left edge.
fn small(cx: &Ctx, s: &mut Scene, right: f32, y: f32, label: &str, primary: bool, d: Do) -> f32 {
    let w = label.chars().count() as f32 * 6.8 + 18.0;
    let r = Rect::new(right - w, y + 3.0, w, ROW - 6.0);
    if primary {
        s.fill(r, 6.0, cx.pal.fg.with_alpha(0.12));
    }
    let ink = if primary { cx.pal.fg } else { cx.pal.dim };
    s.line(label, F::Centre, r, ink);
    s.hit(r, Hit::Tr(Act::Mcp(d)));
    r.x
}

fn icon(cx: &Ctx, s: &mut Scene, right: f32, y: f32, glyph: &str, d: Do) -> f32 {
    let r = Rect::new(right - 22.0, y + 3.0, 22.0, ROW - 6.0);
    s.line(glyph, F::Icon, Rect::new(r.x + 3.0, r.y, 16.0, r.h), cx.pal.faint);
    s.hit(r, Hit::Tr(Act::Mcp(d)));
    r.x
}

/// The servers as a list: those that need the human first, each on one
/// line with the one thing to do for it, then the connected and the off
/// ones as a count that unfolds. It closes to a line.
pub(super) unsafe fn card(cx: &Ctx, i: usize, card: &McpCard, w: f32) -> Scene {
    let mut s = Scene::default();
    let inner = w - 2.0 * PAD;
    let right = w - PAD;
    let trouble = |m: &&McpServer| horadric_core::folio::troubled(&m.status);
    let on = |m: &&McpServer| matches!(m.status.as_str(), "connected" | "pending");
    let needs: Vec<&McpServer> = card.servers.iter().filter(trouble).collect();
    let connected: Vec<&McpServer> = card.servers.iter().filter(on).collect();
    let off: Vec<&McpServer> = (card.servers.iter())
        .filter(|m| !trouble(m) && !on(m))
        .collect();
    if card.closed {
        let mut parts = Vec::new();
        if !needs.is_empty() {
            parts.push(format!("{} need you", needs.len()));
        }
        parts.push(format!("{} connected", connected.len()));
        if !off.is_empty() {
            parts.push(format!("{} off", off.len()));
        }
        let h = ROW + 6.0;
        let label = Rect::new(PAD, 3.0, 96.0, ROW);
        s.line("MCP servers", F::Line, label, cx.pal.dim);
        let rest = Rect::new(PAD + 100.0, 5.0, (inner - 124.0).max(0.0), ROW - 4.0);
        s.line(parts.join(", "), F::Small, rest, cx.pal.faint);
        let mark = Rect::new(right - 16.0, 3.0, 16.0, ROW);
        s.line("\u{E76C}", F::Icon, mark, cx.pal.faint);
        s.hit(Rect::new(0.0, 0.0, w, h), Hit::Tr(Act::Mcp(Do::Close(i))));
        plate(cx, &mut s, w, h, false);
        s.h = h;
        return s;
    }
    let title = Rect::new(PAD, PAD - 2.0, inner - 30.0, 20.0);
    s.line("MCP servers", F::Title, title, cx.pal.fg);
    icon(cx, &mut s, right + 4.0, PAD - 6.0, "\u{E711}", Do::Close(i));
    let mut y = PAD + 24.0;
    if card.servers.is_empty() {
        let say = "No MCP servers in this session, or none heard of yet.";
        s.line(say, F::Line, Rect::new(PAD, y, inner, 22.0), cx.pal.dim);
        y += ROW;
    }
    let heading = |s: &mut Scene, y: &mut f32, label: String, fold: Option<(Group, bool)>| {
        let r = Rect::new(PAD, *y + 5.0, inner - 24.0, 20.0);
        s.line(label, F::Small, r, cx.pal.faint);
        if let Some((group, open)) = fold {
            let glyph = if open { "\u{E70D}" } else { "\u{E76C}" };
            let mark = Rect::new(right - 16.0, *y + 4.0, 16.0, 20.0);
            s.line(glyph, F::Icon, mark, cx.pal.faint);
            let all = Rect::new(0.0, *y, w, ROW);
            s.hit(all, Hit::Tr(Act::Mcp(Do::Group(i, group))));
        }
        *y += ROW;
    };
    if !needs.is_empty() {
        heading(&mut s, &mut y, "NEEDS YOU".into(), None);
        for m in &needs {
            y = row(cx, &mut s, i, card, m, y, w);
        }
    }
    let folds = [
        ("CONNECTED", Group::Connected, card.connected_open, &connected),
        ("OFF", Group::Off, card.off_open, &off),
    ];
    for (label, group, open, servers) in folds {
        if servers.is_empty() {
            continue;
        }
        let label = format!("{label}  {}", servers.len());
        heading(&mut s, &mut y, label, Some((group, open)));
        if open {
            for m in servers {
                y = row(cx, &mut s, i, card, m, y, w);
            }
        }
    }
    let h = y + PAD - 6.0;
    plate(cx, &mut s, w, h, false);
    s.h = h;
    s
}

/// One server: a dot for how it is, its name, and from the right the one
/// thing to do and a mark that unfolds the rest. Returns its bottom.
unsafe fn row(
    cx: &Ctx,
    s: &mut Scene,
    i: usize,
    card: &McpCard,
    m: &McpServer,
    top: f32,
    w: f32,
) -> f32 {
    let right = w - PAD;
    let name = m.name.clone();
    let removal = m.removal();
    let page = m.sign_in.as_ref().is_some_and(|u| !u.is_empty());
    let settled = m.busy || m.removed;
    let has_more = removal.is_some() || m.status != "disabled" || page;
    let mut y = top;
    let mut x = right;
    if settled {
    } else if m.asking_remove {
        x = small(cx, s, x, y, "Remove", true, Do::Remove(name.clone())) - 6.0;
        x = small(cx, s, x, y, "Keep it", false, Do::KeepIt(name.clone())) - 6.0;
    } else {
        if has_more {
            x = icon(cx, s, x + 4.0, y, "\u{E712}", Do::More(i, name.clone())) - 2.0;
        }
        let needs_auth = m.status == "needs-auth";
        let main = if m.sign_in.is_some() {
            Some(("Stop", false, Do::StopSignIn(name.clone())))
        } else if needs_auth && m.can_sign_in() && !m.sign_in_failed {
            Some(("Sign in", true, Do::SignIn(name.clone())))
        } else if needs_auth {
            Some(("In the terminal", false, Do::Terminal))
        } else if m.status == "failed" {
            Some(("Reconnect", true, Do::Reconnect(name.clone())))
        } else if m.status == "disabled" {
            Some(("Enable", true, Do::Toggle(name.clone(), true)))
        } else {
            None
        };
        if let Some((label, primary, d)) = main {
            x = small(cx, s, x, y, label, primary, d) - 6.0;
        }
    }
    let (word, ink) = match (m.removed, m.busy) {
        (true, _) => ("removed".to_string(), cx.pal.faint),
        (false, true) => ("\u{2026}".to_string(), cx.pal.dim),
        _ => status_of(cx, m),
    };
    let dot = match m.status.as_str() {
        "connected" => theme::done(),
        "failed" | "needs-auth" => theme::waiting(),
        "pending" => cx.pal.dim,
        _ => cx.pal.faint.with_alpha(0.5),
    };
    s.fill(Rect::new(PAD + 2.0, y + ROW / 2.0 - 3.5, 7.0, 7.0), 3.5, dot);
    let from = PAD + INDENT;
    let left = (x - from - 8.0).max(80.0);
    let name_ink = if m.status == "disabled" || m.removed {
        cx.pal.dim
    } else {
        cx.pal.fg
    };
    let name_r = Rect::new(from, y + 3.0, left * 0.6, 22.0);
    s.line(m.name.clone(), F::Line, name_r, name_ink);
    let word_r = Rect::new(from + left * 0.6 + 8.0, y + 5.0, left * 0.4 - 8.0, 20.0);
    s.line(word, F::Small, word_r, ink);
    y += ROW;

    let inner = right - from;
    let mut say = |s: &mut Scene, text: &str, ink: Color| {
        if let Some(l) = cx.layout(&cx.fonts.small_wrap, text, inner) {
            s.unit(&l, from, y, ink, text);
            y += render::text_size(&l).1 + 6.0;
        }
    };
    if !m.error.is_empty() {
        say(s, &m.error, cx.pal.dim);
    }
    if m.asking_remove {
        let what = match &removal {
            Some(McpRemoval::Plugin(p)) => format!(
                "This turns off the {p} plugin in every session, with its skills and commands. \
                 claude plugin enable {p} brings it back."
            ),
            Some(McpRemoval::ClaudeAi) => "This turns it off in every session on this \
                 machine, in your own settings. It stays on claude.ai."
                .to_string(),
            Some(McpRemoval::Scope(scope)) if scope == "project" => {
                "This removes it from the project's .mcp.json, which everyone working in it shares."
                    .to_string()
            }
            Some(McpRemoval::Scope(scope)) => format!(
                "This removes it from your {scope} MCP settings, for every session that reads them."
            ),
            None => String::new(),
        };
        say(s, &what, cx.pal.fg.with_alpha(0.85));
    }
    if m.removed {
        say(s, "Removed. New sessions start without it.", cx.pal.dim);
    }
    match &m.sign_in {
        Some(url) if url.is_empty() => say(s, "Starting the sign-in\u{2026}", cx.pal.dim),
        Some(_) => say(
            s,
            "The sign-in page opened in your browser. Finish there and this follows by itself.",
            cx.pal.fg.with_alpha(0.85),
        ),
        None if m.status == "needs-auth"
            && !m.removed
            && !m.asking_remove
            && (m.sign_in_failed || !m.can_sign_in()) =>
        {
            say(
                s,
                "Its sign-in runs only in the terminal: the session goes on there, then type /mcp.",
                cx.pal.dim,
            )
        }
        None => {}
    }
    let open = card.more.as_deref() == Some(m.name.as_str());
    if open && has_more && !settled && !m.asking_remove {
        let mut x = right;
        if removal.is_some() {
            x = small(cx, s, x, y, "Remove", false, Do::AskRemove(name.clone())) - 6.0;
        }
        if m.status != "disabled" {
            let off = Do::Toggle(name.clone(), false);
            x = small(cx, s, x, y, "Turn off everywhere", false, off) - 6.0;
        }
        if page {
            small(cx, s, x, y, "Open the page again", false, Do::Page(name));
        }
        y += ROW;
    }
    y
}

/// The line above the composer, in the way of nothing: which server
/// needs the human, the one thing to do for it, and a way to wave it
/// off. A click on its words opens the list. Returns its top, `bottom`
/// when there is nothing to say.
pub(super) unsafe fn notice(
    gfx: &Gfx,
    pal: &Palette,
    chat: &Chat,
    cx: f32,
    cw: f32,
    bottom: f32,
    hits: &mut Vec<(Rect, Hit)>,
) -> f32 {
    const H: f32 = 19.0;
    let needs = chat.folio.mcp_notice();
    let Some(first) = needs.first() else {
        return bottom;
    };
    let top = bottom - 4.0 - H;
    let (x, right) = (cx + PAD, cx + cw - PAD);
    let name = first.name.clone();
    let (say, main) = match needs.len() {
        1 if first.sign_in.is_some() => {
            (format!("{name}: finish the sign-in in your browser"), None)
        }
        1 if first.status == "failed" => (
            format!("{name} failed"),
            Some(("Reconnect", Do::Reconnect(name))),
        ),
        1 if first.can_sign_in() && !first.sign_in_failed => (
            format!("{name} needs sign-in"),
            Some(("Sign in", Do::SignIn(name))),
        ),
        1 => (format!("{name} needs sign-in"), Some(("Show", Do::Show))),
        n => (format!("{n} MCP servers need you"), Some(("Show", Do::Show))),
    };
    gfx.fill_round(
        &Rect::new(x + 1.0, top + H / 2.0 - 3.0, 6.0, 6.0),
        3.0,
        theme::waiting(),
    );
    let mut edge = right;
    let mut button = |label: &str, ink: Color, d: Do| {
        let w = label.chars().count() as f32 * 6.6 + 14.0;
        let r = Rect::new(edge - w, top, w, H);
        gfx.text_in(label, &gfx.small, &Rect::new(r.x + 7.0, top, w, H), ink);
        hits.push((r, Hit::Tr(Act::Mcp(d))));
        edge = r.x;
    };
    button("Dismiss", pal.faint, Do::Dismiss);
    if let Some((label, d)) = main {
        button(label, pal.fg, d);
    }
    let words = Rect::new(x + 14.0, top, (edge - x - 20.0).max(0.0), H);
    gfx.text_in(&say, &gfx.small, &words, pal.dim);
    hits.push((words, Hit::Tr(Act::Mcp(Do::Show))));
    top
}

/// An MCP server's question: its message, then its fields (text and
/// numbers typed, boxes to tick, options to pick) or the page it wants
/// opened, then Submit, Decline and Cancel.
pub(super) unsafe fn elicitation(cx: &Ctx, e: &Elicitation, st: &State, w: f32) -> Scene {
    let mut s = Scene::default();
    let inner = w - 2.0 * PAD;
    let right = w - PAD;
    let waiting = e.state == AskState::Waiting;
    let title = if e.title.is_empty() {
        format!("{} asks", e.server)
    } else {
        e.title.clone()
    };
    s.line(title, F::Title, Rect::new(PAD, PAD, inner, 20.0), cx.pal.fg);
    let mut y = PAD + 28.0;
    let say = |s: &mut Scene, y: &mut f32, text: &str, ink: Color, small: bool| {
        let font = if small {
            &cx.fonts.small_wrap
        } else {
            &cx.fonts.body
        };
        if let Some(l) = cx.layout(font, text, inner) {
            s.unit(&l, PAD, *y, ink, text);
            *y += render::text_size(&l).1 + 6.0;
        }
    };
    if !e.message.is_empty() {
        say(&mut s, &mut y, &e.message, cx.pal.fg, false);
        y += 4.0;
    }
    let rid = &e.request_id;
    if let Some(url) = &e.url {
        say(
            &mut s,
            &mut y,
            url,
            cx.pal.accent.mix(cx.pal.fg, 0.45),
            true,
        );
        y += 6.0;
    } else if waiting {
        for f in &e.fields {
            let label = if f.required {
                format!("{} *", f.label)
            } else {
                f.label.clone()
            };
            let id = field_id(rid, &f.name);
            let typed = st.text(&id);
            let value = if typed.is_empty() {
                f.default.as_str()
            } else {
                typed
            };
            if !matches!(f.kind, FieldKind::Boolean) {
                s.line(
                    label.clone(),
                    F::Small,
                    Rect::new(PAD, y, inner, 18.0),
                    cx.pal.dim,
                );
                y += 20.0;
                if !f.description.is_empty() {
                    say(&mut s, &mut y, &f.description, cx.pal.faint, true);
                }
            }
            match &f.kind {
                FieldKind::Text { .. } | FieldKind::Number { .. } => {
                    let hint = match (&f.kind, f.default.is_empty()) {
                        (_, false) => f.default.clone(),
                        (FieldKind::Number { min, max, .. }, true) => match (min, max) {
                            (Some(a), Some(b)) => format!("A number from {a} to {b}"),
                            _ => "A number".to_string(),
                        },
                        _ => String::new(),
                    };
                    y = field(cx, &mut s, st, &id, &hint, PAD, y, inner) + 12.0;
                }
                FieldKind::Boolean => {
                    let on = value == "true";
                    let r = Rect::new(PAD, y, inner, 26.0);
                    let icon = if on { "\u{E73A}" } else { "\u{E739}" };
                    s.line(
                        icon,
                        F::Icon,
                        Rect::new(PAD, y + 3.0, 16.0, 20.0),
                        cx.pal.dim,
                    );
                    s.line(
                        label,
                        F::Line,
                        Rect::new(PAD + 24.0, y + 2.0, inner - 24.0, 22.0),
                        cx.pal.fg,
                    );
                    s.hit(r, Hit::Tr(Act::Mcp(Do::Flip(rid.clone(), f.name.clone()))));
                    y += 28.0;
                    if !f.description.is_empty() {
                        say(&mut s, &mut y, &f.description, cx.pal.faint, true);
                    }
                    y += 6.0;
                }
                FieldKind::Choice(options) | FieldKind::Choices { options, .. } => {
                    let many = matches!(f.kind, FieldKind::Choices { .. });
                    for (v, shown) in options {
                        let on = if many {
                            value.split(ELICIT_SEP).any(|p| p == v)
                        } else {
                            value == v
                        };
                        let r = Rect::new(PAD, y, inner, 30.0);
                        if on {
                            s.fill(r, 8.0, cx.pal.accent.with_alpha(0.18));
                            s.stroke(r, 8.0, cx.pal.accent.with_alpha(0.7), 1.0);
                        } else {
                            s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.16), 1.0);
                        }
                        let icon = match (many, on) {
                            (true, true) => "\u{E73A}",
                            (true, false) => "\u{E739}",
                            (false, true) => "\u{ECCB}",
                            (false, false) => "\u{ECCA}",
                        };
                        s.line(
                            icon,
                            F::Icon,
                            Rect::new(r.x + 10.0, y + 6.0, 16.0, 18.0),
                            cx.pal.dim,
                        );
                        s.line(
                            shown.clone(),
                            F::Line,
                            Rect::new(r.x + 34.0, y + 4.0, inner - 44.0, 22.0),
                            cx.pal.fg,
                        );
                        let pick = Do::Pick(rid.clone(), f.name.clone(), v.clone());
                        s.hit(r, Hit::Tr(Act::Mcp(pick)));
                        y = r.bottom() + 6.0;
                    }
                    y += 6.0;
                }
            }
        }
    }
    let why = st.text(&error_id(rid));
    if waiting && !why.is_empty() {
        say(&mut s, &mut y, why, theme::waiting(), false);
    }
    if waiting {
        let (go, label) = match &e.url {
            Some(_) => (Do::Open(rid.clone()), "Open page"),
            None => (Do::Accept(rid.clone()), "Submit"),
        };
        let mut x = button(cx, &mut s, right, y, label, true, Hit::Tr(Act::Mcp(go))) - 8.0;
        x = button(
            cx,
            &mut s,
            x,
            y,
            "Decline",
            false,
            Hit::Tr(Act::Mcp(Do::Decline(rid.clone()))),
        ) - 8.0;
        button(
            cx,
            &mut s,
            x,
            y,
            "Cancel",
            false,
            Hit::Tr(Act::Mcp(Do::Cancel(rid.clone()))),
        );
        y += BUTTON_H;
    } else {
        let said = match (e.said, e.state) {
            (Some(ElicitAction::Accept), _) if e.url.is_some() && e.complete => "Opened, and done",
            (Some(ElicitAction::Accept), _) if e.url.is_some() => "Opened in your browser",
            (Some(ElicitAction::Accept), _) => "Sent",
            (Some(ElicitAction::Decline), _) => "Declined",
            (Some(ElicitAction::Cancel), _) => "Cancelled",
            (None, AskState::Allowed) => "Sent",
            (None, AskState::Denied) => "Declined",
            _ => "Not needed",
        };
        s.line(said, F::Line, Rect::new(PAD, y, inner, 22.0), cx.pal.dim);
        y += 22.0;
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, waiting);
    s.h = h;
    s
}
