//! MCP in the Folio: the card listing servers (those in trouble as they
//! turn up, all of them on `/mcp`) with sign in, reconnect, disable and
//! remove, and the card an MCP server's question (an elicitation) becomes. The
//! status is asked for once a session is live, and again every couple of
//! seconds while a sign-in or a starting server waits, since the CLI
//! says nothing when a status changes.

use std::process::Stdio;
use std::sync::Mutex;

use horadric_core::folio::{
    AskState, Block, ElicitAction, Elicitation, FieldKind, McpCard, McpRemoval, McpServer, McpWant,
    ELICIT_SEP,
};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, SW_SHOWNORMAL};

use super::cards::{button, field, plate, BUTTON_H, PAD};
use super::rich::{Ctx, Scene, F};
use super::transcript::{Act, State};
use super::view::Hit;
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

/// Where claude.ai connectors are removed.
const CONNECTORS: &str = "https://claude.ai/settings/connectors";

/// What removals said, by session key and server, for the UI thread.
static REMOVED: Mutex<Vec<(String, String, Result<(), String>)>> = Mutex::new(Vec::new());

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
        McpRemoval::ClaudeAi => {
            with(key, |c| c.folio.mcp_ask_remove(server, false));
            open(CONNECTORS);
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
                .filter(|l| !l.is_empty())
                .last();
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
    let pages = chat.folio.take_mcp_pages();
    // A replay's pages were opened when they came.
    if !chat.live || chat.ended || chat.hosted.is_none() || chat.watch.is_some() {
        return;
    }
    for url in pages {
        open(&url);
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

/// The servers, each with what can be done for it.
pub(super) unsafe fn card(cx: &Ctx, card: &McpCard, w: f32) -> Scene {
    let mut s = Scene::default();
    let inner = w - 2.0 * PAD;
    let right = w - PAD;
    let trouble = (card.servers.iter()).any(|m| horadric_core::folio::troubled(&m.status));
    let title = match (card.all, card.servers.len()) {
        (true, _) => "MCP servers",
        (false, 1) if trouble => "An MCP server needs you",
        (false, _) if trouble => "MCP servers need you",
        (false, _) => "MCP servers",
    };
    s.line(title, F::Title, Rect::new(PAD, PAD, inner, 20.0), cx.pal.fg);
    let mut y = PAD + 30.0;
    if card.servers.is_empty() {
        let say = if card.all {
            "No MCP servers in this session, or none heard of yet."
        } else {
            "Nothing to do."
        };
        s.line(say, F::Line, Rect::new(PAD, y, inner, 22.0), cx.pal.dim);
        y += 22.0;
    }
    let say = |s: &mut Scene, y: &mut f32, text: &str, ink: Color| {
        if let Some(l) = cx.layout(&cx.fonts.small_wrap, text, inner) {
            s.unit(&l, PAD, *y, ink, text);
            *y += render::text_size(&l).1 + 6.0;
        }
    };
    for m in &card.servers {
        // The buttons first, from the right, so the name takes what is left.
        let name = m.name.clone();
        let mut x = right;
        let mut row = |label: &str, primary: bool, d: Do, s: &mut Scene| {
            x = button(cx, s, x, y, label, primary, Hit::Tr(Act::Mcp(d))) - 8.0;
        };
        let removal = m.removal();
        let trouble = horadric_core::folio::troubled(&m.status);
        if m.busy || m.removed {
        } else if m.asking_remove {
            row("Remove", true, Do::Remove(name.clone()), &mut s);
            row("Keep it", false, Do::KeepIt(name.clone()), &mut s);
        } else if let Some(url) = &m.sign_in {
            row("Stop", false, Do::StopSignIn(name.clone()), &mut s);
            if !url.is_empty() {
                row("Open the page again", false, Do::Page(name.clone()), &mut s);
            }
        } else if m.status == "needs-auth" && m.can_sign_in() && !m.sign_in_failed {
            row("Sign in", true, Do::SignIn(name.clone()), &mut s);
        } else if m.status == "needs-auth" {
            row("Sign in from the terminal", false, Do::Terminal, &mut s);
        } else if m.status == "failed" {
            row("Reconnect", true, Do::Reconnect(name.clone()), &mut s);
        } else if m.status == "disabled" {
            row("Enable", false, Do::Toggle(name.clone(), true), &mut s);
        } else if card.all && m.status == "connected" {
            row("Disable", false, Do::Toggle(name.clone(), false), &mut s);
        }
        // A server in trouble can also be let go of: off here, or gone.
        let settled = m.busy || m.removed || m.asking_remove || m.sign_in.is_some();
        if !settled && trouble {
            row("Disable", false, Do::Toggle(name.clone(), false), &mut s);
        }
        if !settled && removal.is_some() && (card.all || trouble) {
            row("Remove", false, Do::AskRemove(name.clone()), &mut s);
        }
        let (word, ink) = match (m.removed, m.busy) {
            (true, _) => ("removed".to_string(), cx.pal.faint),
            (false, true) => ("\u{2026}".to_string(), cx.pal.dim),
            _ => status_of(cx, m),
        };
        let left = (x - PAD).max(80.0);
        s.line(
            m.name.clone(),
            F::Line,
            Rect::new(PAD, y + 4.0, left * 0.62, 22.0),
            cx.pal.fg,
        );
        s.line(
            word,
            F::Small,
            Rect::new(PAD + left * 0.62 + 8.0, y + 6.0, left * 0.38 - 8.0, 20.0),
            ink,
        );
        y += BUTTON_H + 6.0;
        if !m.error.is_empty() {
            say(&mut s, &mut y, &m.error, cx.pal.dim);
        }
        if m.asking_remove {
            let what = match &removal {
                Some(McpRemoval::Plugin(p)) => format!(
                    "This turns off the {p} plugin in every session, with its skills and commands. \
                     claude plugin enable {p} brings it back."
                ),
                Some(McpRemoval::ClaudeAi) => "Connectors are removed on claude.ai: \
                     this opens its settings in your browser."
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
            say(&mut s, &mut y, &what, cx.pal.fg.with_alpha(0.85));
        }
        if m.removed {
            say(
                &mut s,
                &mut y,
                "Removed. New sessions start without it.",
                cx.pal.dim,
            );
        }
        match &m.sign_in {
            Some(url) if url.is_empty() => {
                say(&mut s, &mut y, "Starting the sign-in\u{2026}", cx.pal.dim)
            }
            Some(_) => say(
                &mut s,
                &mut y,
                "The sign-in page opened in your browser. Finish there and this follows by itself.",
                cx.pal.fg.with_alpha(0.85),
            ),
            None if m.status == "needs-auth"
                && !m.removed
                && !m.asking_remove
                && (m.sign_in_failed || !m.can_sign_in()) =>
            {
                say(
                &mut s,
                &mut y,
                    "Its sign-in runs only in the terminal: the session goes on there, then type /mcp.",
                    cx.pal.dim,
                )
            }
            None => {}
        }
        y += 4.0;
    }
    let h = y + PAD - 6.0;
    plate(cx, &mut s, w, h, trouble);
    s.h = h;
    s
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
