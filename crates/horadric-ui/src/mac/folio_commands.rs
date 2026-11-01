//! What the Mac's Folio does beyond sending prompts: the `/` commands it
//! runs itself and the `!` commands it runs in the project's shell, the
//! menu at the bar's start, a login with its card, images pasted in, and
//! ending claude for the tile to go on rewound, forked, logged in or in a
//! terminal. The lists, the runs and the rewinds are the Windows Folio's,
//! shared through `folio_slash`, `folio_cli` and `folio_rewind`; this is
//! how the Mac shows them, in AppKit menus and the bar.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use horadric_core::folio::{
    prompt_point, shell_command, Block, Heard, ImageData, Rewind, SignInState,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
    NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};

use crate::folio_cli::{self, Login};
use crate::folio_rewind::{find_rewind, flags, fork_line, read_transcript};
use crate::folio_slash::{self as shared, Row};

use super::app::{self, Input};
use super::folio::{self, refresh, with, Again, Ask, FolioView, Leave};
use super::menu::{self, Item};

/// Commands the Mac's Folio runs itself where the CLI would draw a
/// screen, with what the `/` menu says of each. The rest go to claude.
const NATIVE: &[(&str, &str)] = &[
    ("btw", "Ask aside, without it joining the conversation"),
    ("login", "Log in to Claude in your browser"),
    ("logout", "Log out of Claude"),
    ("mcp", "MCP servers: status, sign in, reconnect"),
    ("memory", "Open a CLAUDE.md"),
    ("agents", "Open a subagent's file"),
    ("hooks", "The hooks the settings run"),
    ("permissions", "The allow, ask and deny rules"),
    ("plugin", "Plugins and marketplaces"),
    ("export", "Save the conversation as markdown"),
    ("add-dir", "Let Claude work in another folder too"),
    ("model", "Switch the model"),
    ("exit", "End this chat"),
];

/// The most a `/` menu or a menu of prompts lists.
const LISTED: usize = 30;

/// A fork from an earlier prompt, waiting for its files to go back.
pub(super) struct Fork {
    rewind: Rewind,
    prompt: String,
    /// How putting back the files went, once it has.
    pub(super) files: Option<Result<(), String>>,
}

/// Opens `url` in the default browser: a login is the human's own, with
/// the browser that holds it.
pub(super) fn open_url(url: &str) {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return;
    }
    if let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

fn notice(id: &str, text: &str) {
    with(id, |c| c.notice = (!text.is_empty()).then(|| text.to_string()));
    refresh(id);
}

/// Carries out what the human typed: a command the Folio runs itself, a
/// `!` command, or a prompt. True once it went, so the composer empties.
pub(super) fn submit(id: &str, text: &str, view: &FolioView) -> bool {
    let text = text.trim();
    if let Some((name, words)) = native(text) {
        run(id, name, words, view);
        return true;
    }
    if let Some(command) = shell_command(text) {
        run_shell(id, command);
        return true;
    }
    with(id, |c| c.ready() && c.deliver(text)).unwrap_or(false)
}

/// The native command `text` runs, and the words after its name.
fn native(text: &str) -> Option<(&'static str, &str)> {
    let rest = text.strip_prefix('/')?;
    let name = rest.split_whitespace().next()?;
    let found = NATIVE.iter().map(|(n, _)| *n).find(|n| *n == name)?;
    Some((found, rest[name.len()..].trim()))
}

/// Tab after a `/`: the commands whose names start with what is typed,
/// the Folio's own then claude's, in a menu. The name picked.
pub(super) fn pick_command(id: &str, typed: &str, view: &FolioView) -> Option<String> {
    let word = typed.trim_start_matches('/');
    let mut names: Vec<(String, String)> = (NATIVE.iter())
        .filter(|(n, _)| n.starts_with(word))
        .map(|(n, d)| (n.to_string(), d.to_string()))
        .collect();
    let theirs = with(id, |c| c.folio.slash_commands(word)).unwrap_or_default();
    for c in theirs {
        if !names.iter().any(|(n, _)| *n == c.name) {
            names.push((c.name, c.description));
        }
    }
    names.truncate(LISTED);
    if names.is_empty() {
        return None;
    }
    let items: Vec<Item> = (names.iter().enumerate())
        .map(|(i, (n, d))| Item::pick(labelled(&format!("/{n}"), d), i + 1))
        .collect();
    let picked = menu::popup(MainThreadMarker::from(view), &items, view.more_point())?;
    names.get(picked.checked_sub(1)?).map(|(n, _)| n.clone())
}

/// A menu row's title: its label, and what it is when that is short.
fn labelled(label: &str, detail: &str) -> String {
    let detail: String = detail.chars().take(70).collect();
    match detail.trim() {
        "" => label.to_string(),
        d => format!("{label}    {d}"),
    }
}

/// Runs native command `name` with the `words` typed after it.
fn run(id: &str, name: &str, words: &str, view: &FolioView) {
    let Some(dir) = with(id, |c| c.dir.clone()) else {
        return;
    };
    let claude = crate::inventory::claude_dir();
    with(id, |c| c.notice = None);
    match name {
        "btw" if words.is_empty() => notice(id, "Say /btw and the question."),
        "btw" => {
            with(id, |c| {
                if c.ready() {
                    let line = c.folio.side_question(words);
                    if !c.send(Some(line)) {
                        c.folio.blocks.pop();
                    }
                }
            });
        }
        "login" => log_in(id),
        "logout" => log_out(id),
        "mcp" => {
            with(id, |c| {
                if c.ready() {
                    let line = c.folio.show_mcp();
                    c.send(Some(line));
                }
            });
            mcp_menu(id, view);
        }
        "memory" => choose(
            id,
            view,
            shared::existing(shared::memory_files(&dir, &claude)),
            "No CLAUDE.md yet: /init writes one for the project.",
        ),
        "agents" => choose(
            id,
            view,
            shared::existing(shared::agent_files(&dir, &claude)),
            "No subagents: ask Claude to write one in .claude/agents.",
        ),
        "hooks" => choose(
            id,
            view,
            shared::hook_rows(&dir, &claude),
            "No hooks in the settings.",
        ),
        "permissions" if words.is_empty() => {
            choose(id, view, shared::permission_rows(&dir, &claude), "")
        }
        "permissions" => notice(id, &shared::add_rule_said(&dir, words)),
        "plugin" if words.is_empty() => hidden(id, &["plugin", "list", "--json"], Then::Plugins),
        "plugin" => {
            let mut args = vec!["plugin"];
            args.extend(words.split_whitespace());
            hidden(id, &args, Then::Show(format!("/plugin {words}")));
        }
        "export" => {
            let Some(blocks) = with(id, |c| c.folio.blocks.clone()) else {
                return;
            };
            let name = shared::export_name(&blocks, words);
            let path = dir.join(&name);
            match std::fs::write(&path, shared::markdown(&blocks)) {
                Ok(()) => {
                    notice(id, &format!("Saved to {name}"));
                    app::input(Input::Folio(id.to_string(), Ask::View(path)));
                }
                Err(e) => notice(id, &format!("Not saved: {e}")),
            }
        }
        "add-dir" => add_dir(id, &dir, words, view),
        "model" if words.is_empty() => notice(id, "Say /model and the model, as /model opus."),
        "model" => {
            with(id, |c| {
                if c.ready() {
                    let line = c.folio.set_model(words);
                    c.send(Some(line));
                }
            });
            notice(id, &format!("Switching to {words}."));
        }
        "exit" => app::input(Input::Folio(id.to_string(), Ask::Exit)),
        _ => {}
    }
}

/// Shows a command's rows in a menu at the bar and does what the one
/// picked says, or says `empty` when there are none.
fn choose(id: &str, view: &FolioView, rows: Vec<Row>, empty: &str) {
    if rows.is_empty() {
        notice(id, empty);
        return;
    }
    let items: Vec<Item> = (rows.iter().enumerate().take(LISTED))
        .map(|(i, r)| match r.current {
            true => Item::checked(labelled(&r.label, &r.detail), i + 1, true),
            false => Item::pick(labelled(&r.label, &r.detail), i + 1),
        })
        .collect();
    let Some(picked) = menu::popup(MainThreadMarker::from(view), &items, view.more_point()) else {
        return;
    };
    if let Some(r) = picked.checked_sub(1).and_then(|i| rows.get(i)) {
        act(id, &r.value, view);
    }
}

/// Does what a command row's value says.
fn act(id: &str, value: &str, view: &FolioView) {
    let w: Vec<&str> = value.split('\t').collect();
    match w.as_slice() {
        ["view", path] => {
            app::input(Input::Folio(id.to_string(), Ask::View(PathBuf::from(path))));
        }
        ["put", text] => view.put_composer(text),
        ["unrule", path, list, rule] => {
            notice(id, &shared::remove_rule_said(Path::new(path), list, rule));
        }
        ["plugin", name, on] => choose(id, view, shared::plugin_actions(name, *on == "true"), ""),
        ["market", name] => choose(id, view, shared::market_actions(name), ""),
        ["markets"] => hidden(id, &["plugin", "marketplace", "list", "--json"], Then::Markets),
        ["run", words] => {
            let args: Vec<&str> = words.split_whitespace().collect();
            hidden(id, &args, Then::Show(format!("/{words}")));
        }
        _ => {}
    }
}

/// `/add-dir`: the Folio starts again on its conversation with the folder
/// added, since the CLI takes folders only as it starts.
fn add_dir(id: &str, dir: &Path, words: &str, view: &FolioView) {
    let typed = words.trim_matches('"');
    if typed.is_empty() {
        view.put_composer("/add-dir ");
        return;
    }
    let path = dir.join(typed);
    if !path.is_dir() {
        notice(id, &format!("{typed} is not a folder."));
        return;
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let again = Again {
        own: vec!["--add-dir".into(), path.display().to_string()],
        said: Some(format!("Added {}", path.display())),
        ..carry_on(id)
    };
    if !with(id, |c| c.let_go(id, Leave::Again(again))).unwrap_or(false) {
        notice(id, "/add-dir starts Claude again: once it rests.");
    }
}

/// What a hidden `claude` run is for, once it has said its piece.
enum Then {
    /// Shown in the conversation under this line.
    Show(String),
    Plugins,
    Markets,
}

/// Runs `claude` with `args`, no window, in the session's folder, and
/// does `then` with what it said once it ends.
fn hidden(id: &str, args: &[&str], then: Then) {
    let Some(dir) = with(id, |c| c.dir.clone()) else {
        return;
    };
    let mut c = match folio_cli::claude_command(args) {
        Ok(c) => c,
        Err(why) => {
            notice(id, &why);
            return;
        }
    };
    c.current_dir(&dir);
    notice(id, &format!("claude {}\u{2026}", args.join(" ")));
    let id = id.to_string();
    folio_cli::run_hidden(c, move |said| {
        super::queue::post(move || hidden_done(&id, then, said));
    });
}

fn hidden_done(id: &str, then: Then, said: Result<String, String>) {
    notice(id, "");
    let view = with(id, |c| c.view.clone()).flatten();
    match (then, said) {
        (Then::Show(line), said) => {
            let text = match said {
                Ok(t) if t.is_empty() => "Done.".to_string(),
                Ok(t) => t,
                Err(e) => e,
            };
            with(id, |c| {
                c.folio.note(&line);
                c.folio.blocks.push(Block::Reply(folio_cli::fenced(&text)));
            });
        }
        (Then::Plugins, Ok(json)) => {
            if let Some(v) = view {
                choose(id, &v, shared::plugin_menu(&json), "");
            }
        }
        (Then::Markets, Ok(json)) => {
            if let Some(v) = view {
                choose(id, &v, shared::market_menu(&json), "");
            }
        }
        (_, Err(e)) => notice(id, e.lines().next().unwrap_or("claude failed")),
    }
    refresh(id);
}

/// Runs a `!` command in the project's folder with the shell a terminal
/// gets, off the main thread, its output shown as it ends and sent to
/// Claude with the next prompt.
fn run_shell(id: &str, command: &str) {
    let Some(program) = crate::console::shell_program() else {
        notice(id, "No shell found to run that in.");
        return;
    };
    let Some((dir, at)) = with(id, |c| (c.dir.clone(), c.folio.shell_started(command))) else {
        return;
    };
    let (id, said) = (id.to_string(), command.to_string());
    folio_cli::run_shell(program, command, &dir, move |output, code| {
        super::queue::post(move || {
            with(&id, |c| c.folio.shell_done(at, &said, &output, code));
            refresh(&id);
        });
    });
    refresh_soon();
}

fn refresh_soon() {
    super::queue::post(folio::drain);
}

// The menu at the bar's start, by tag.
const REVIEW: usize = 1;
const UNDO: usize = 2;
const ATTACH: usize = 3;
const MCP: usize = 4;
const LOG_IN: usize = 5;
const TERMINAL: usize = 6;
const STOP: usize = 7;
/// A prompt to rewind to, by how far back it is.
const PROMPT: usize = 100;
/// A permission mode, by its place in [`MODES`].
const MODE: usize = 300;

/// The permission modes the menu offers, with what it calls each.
const MODES: &[(&str, &str)] = &[
    ("default", "Ask Before Edits"),
    ("acceptEdits", "Edit Without Asking"),
    ("plan", "Plan First"),
];

/// The menu at the bar's start: review, undo, rewind, mode, images, MCP,
/// login and the terminal.
pub(super) fn more(id: &str, view: &FolioView) {
    let Some((review, rests, prompts, mode, busy)) = with(id, |c| {
        let prompts: Vec<String> = (c.folio.blocks.iter())
            .filter_map(|b| match b {
                Block::Prompt(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        (
            c.review,
            c.between_turns(),
            prompts,
            c.folio.permission_mode().to_string(),
            c.folio.busy,
        )
    }) else {
        return;
    };
    let mut items = vec![Item::checked("Review Edits", REVIEW, review)];
    let modes: Vec<Item> = (MODES.iter().enumerate())
        .map(|(i, (m, label))| {
            let on = *m == mode || (mode.is_empty() && *m == "default");
            Item::checked(*label, MODE + i, on)
        })
        .collect();
    items.push(Item::Sub("Mode".into(), modes));
    items.push(Item::Separator);
    if busy {
        items.push(Item::pick("Stop Claude", STOP));
    }
    if rests && !prompts.is_empty() {
        items.push(Item::pick("Undo Last Turn", UNDO));
        // The newest first, as far back as a menu reads well.
        let back: Vec<Item> = (prompts.iter().rev().enumerate().take(LISTED))
            .map(|(i, p)| Item::pick(short(p), PROMPT + i + 1))
            .collect();
        items.push(Item::Sub("Rewind to Before".into(), back));
    } else {
        items.push(Item::disabled("Undo Last Turn"));
    }
    items.push(Item::Separator);
    items.push(Item::pick("Attach Image\u{2026}", ATTACH));
    items.push(Item::pick("MCP Servers", MCP));
    items.push(Item::pick("Log In to Claude\u{2026}", LOG_IN));
    items.push(Item::Separator);
    match rests {
        true => items.push(Item::pick("Go On in a Terminal", TERMINAL)),
        false => items.push(Item::disabled("Go On in a Terminal")),
    }
    let mtm = MainThreadMarker::from(view);
    let Some(picked) = menu::popup(mtm, &items, view.more_point()) else {
        return;
    };
    match picked {
        REVIEW => toggle_review(id),
        UNDO => undo(id),
        STOP => folio::stop(id),
        ATTACH => attach(id, mtm),
        MCP => {
            with(id, |c| {
                if c.ready() {
                    let line = c.folio.show_mcp();
                    c.send(Some(line));
                }
            });
            mcp_menu(id, view);
        }
        LOG_IN => log_in(id),
        TERMINAL => {
            if !with(id, |c| c.let_go(id, Leave::Terminal)).unwrap_or(false) {
                notice(id, "It goes on in a terminal once Claude rests.");
            }
        }
        n if n > PROMPT && n <= PROMPT + LISTED => {
            let back = n - PROMPT;
            if let Some(text) = prompts.iter().rev().nth(back - 1).cloned() {
                fork(id, back, &text, view);
            }
        }
        n if (MODE..MODE + MODES.len()).contains(&n) => {
            let mode = MODES[n - MODE].0;
            with(id, |c| {
                if c.ready() {
                    let line = c.folio.set_permission_mode(mode);
                    c.send(Some(line));
                }
            });
        }
        _ => {}
    }
    refresh(id);
}

/// A prompt as a menu row: its first words.
fn short(text: &str) -> String {
    let said: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match said.chars().count() > 60 {
        true => said.chars().take(59).collect::<String>() + "\u{2026}",
        false => said,
    }
}

/// Review on or off. Turned on, a claude started without its hook starts
/// again to take it, once it rests (see `folio::tend`).
fn toggle_review(id: &str) {
    with(id, |c| {
        c.review = !c.review;
        c.folio.review = c.review;
        c.notice = Some(match c.review {
            true => "Each edit waits for you, as a diff to comment on.".into(),
            false => "Edits land as the mode says.".into(),
        });
    });
    refresh_soon();
}

/// How a Folio starts again on the conversation it holds.
fn carry_on(id: &str) -> Again {
    let resume = with(id, |c| c.folio.session_id.clone().or(c.tag.resume.clone())).flatten();
    Again {
        resume,
        ..Again::default()
    }
}

/// Starts session `id` again on its conversation, its page saying `said`,
/// once it rests.
pub(super) fn start_again(id: &str, said: &str) {
    let again = Again {
        said: Some(said.to_string()),
        ..carry_on(id)
    };
    with(id, |c| c.let_go(id, Leave::Again(again)));
}

/// Undo last turn: claude ends and starts again resumed at the line
/// before the turn's prompt, the turn kept in the transcript but out of
/// the conversation.
fn undo(id: &str) {
    let found = with(id, |c| {
        if !c.between_turns() {
            return Err("It works or asks something now. Undo once it rests.");
        }
        let conversation = c.folio.session_id.clone().ok_or("No turn to undo yet.")?;
        let prompted = (c.folio.blocks.get(c.folio.history..).into_iter().flatten())
            .any(|b| matches!(b, Block::Prompt(_)));
        let head = c.rewound.clone().filter(|_| !prompted);
        let path = horadric_hooks::transcript::path(&c.dir.to_string_lossy(), &conversation)
            .ok_or("No turn to undo was found in the conversation.")?;
        let rewind = find_rewind(&path, head.as_deref())
            .ok_or("No turn to undo was found in the conversation.")?;
        Ok((conversation, rewind))
    });
    let (conversation, rewind) = match found {
        Some(Ok(f)) => f,
        Some(Err(why)) => return notice(id, why),
        None => return,
    };
    let fresh = rewind.keep.is_none();
    let again = Again {
        resume: (!fresh).then_some(conversation),
        flags: flags(&rewind, false),
        at: rewind.keep.clone(),
        said: Some("The last turn was undone.".into()),
        ..Again::default()
    };
    with(id, |c| c.let_go(id, Leave::Again(again)));
}

/// Rewinds to before the `back`th prompt from the end, which reads
/// `text`: its files go back first, by the CLI while it still runs, then
/// it starts again as a new conversation from there, the old one kept
/// whole in History. The prompt comes back into the composer.
fn fork(id: &str, back: usize, text: &str, view: &FolioView) {
    let found = with(id, |c| {
        if !c.between_turns() {
            return Err("It works or asks something now. Rewind once it rests.");
        }
        let conversation = c.folio.session_id.clone().ok_or("Nothing to rewind yet.")?;
        let prompted = (c.folio.blocks.get(c.folio.history..).into_iter().flatten())
            .any(|b| matches!(b, Block::Prompt(_)));
        let head = c.rewound.clone().filter(|_| !prompted);
        let path = horadric_hooks::transcript::path(&c.dir.to_string_lossy(), &conversation);
        let rewind = path
            .and_then(|p| read_transcript(&p, true))
            .and_then(|(jsonl, _)| prompt_point(&jsonl, head.as_deref(), back, text))
            .ok_or("That prompt was not found in the conversation Claude keeps.")?;
        let line = c.folio.rewind_files(&rewind.drop);
        if !c.send(Some(line)) {
            return Err("Claude did not take the rewind.");
        }
        c.fork = Some(Fork {
            rewind,
            prompt: text.to_string(),
            files: None,
        });
        Ok(())
    });
    match found {
        Some(Ok(())) => view.put_composer(text),
        Some(Err(why)) => notice(id, why),
        None => {}
    }
}

/// A fork's files went back, or could not: the conversation forks now.
pub(super) fn fork_now(id: &str) {
    let Some(Some(f)) = with(id, |c| c.fork.take()) else {
        return;
    };
    let fresh = f.rewind.keep.is_none();
    let again = Again {
        flags: flags(&f.rewind, true),
        at: f.rewind.keep.clone(),
        said: Some(fork_line(&f.prompt, f.files.as_ref())),
        ..carry_on(id)
    };
    let again = Again {
        resume: if fresh { None } else { again.resume.clone() },
        ..again
    };
    if !with(id, |c| c.let_go(id, Leave::Again(again))).unwrap_or(false) {
        notice(id, "It started working before the conversation could go back.");
    }
}

/// A line comment on the edit ask `request_id`: the rows by their
/// numbers in the transcript, and what to say of them.
pub(super) fn comment(id: &str, request_id: &str) {
    let Some((from, to, text)) = super::folio_forms::comment() else {
        return;
    };
    with(id, |c| {
        let review = c.reviews.entry(request_id.to_string()).or_default();
        review.pick(from, false);
        review.pick(to, true);
        review.add(&text);
    });
    refresh(id);
}

/// The MCP server's form or page that waits, as a dialog.
pub(super) fn elicit(id: &str, request_id: &str) {
    let asked = with(id, |c| {
        c.folio.blocks.iter().rev().find_map(|b| match b {
            Block::Elicitation(e) if e.request_id == request_id => Some(e.clone()),
            _ => None,
        })
    })
    .flatten();
    let Some(e) = asked else {
        return;
    };
    let Some((action, content)) = super::folio_forms::elicit(&e) else {
        return;
    };
    folio::answer(id, |c| c.folio.answer_elicitation(request_id, action, content));
}

/// The MCP servers in a menu, each with what can be done for it.
fn mcp_menu(id: &str, view: &FolioView) {
    let servers = with(id, |c| c.folio.mcp.servers.clone()).unwrap_or_default();
    if servers.is_empty() {
        notice(id, "No MCP servers yet. Their status comes in a moment.");
        return;
    }
    // A server's actions by tag: its place times ten, plus the action.
    let mut items = Vec::new();
    for (i, s) in servers.iter().enumerate() {
        let base = (i + 1) * 10;
        let mut acts = vec![Item::pick("Reconnect", base + 1)];
        acts.push(match s.status.as_str() {
            "disabled" => Item::pick("Turn On", base + 2),
            _ => Item::pick("Turn Off", base + 3),
        });
        if s.can_sign_in() {
            acts.push(Item::pick("Sign In\u{2026}", base + 4));
        }
        let mut title = format!("{}: {}", s.name, s.status);
        if !s.error.is_empty() {
            title.push_str(&format!(" ({})", s.error));
        }
        items.push(Item::Sub(title, acts));
    }
    let Some(picked) = menu::popup(MainThreadMarker::from(view), &items, view.more_point()) else {
        return;
    };
    let Some(s) = (picked / 10).checked_sub(1).and_then(|i| servers.get(i)) else {
        return;
    };
    let name = s.name.clone();
    folio::answer(id, |c| match picked % 10 {
        1 => Some(c.folio.mcp_reconnect(&name)),
        2 => Some(c.folio.mcp_toggle(&name, true)),
        3 => Some(c.folio.mcp_toggle(&name, false)),
        4 => Some(c.folio.mcp_sign_in(&name)),
        _ => None,
    });
}

thread_local! {
    /// The login running now: its run, the session whose card it serves,
    /// and the CLI. One at a time, as on Windows.
    static LOGIN: RefCell<Option<(u64, String, Login)>> = const { RefCell::new(None) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

/// Starts a login for session `id`'s card, or a new card (`/login`). Any
/// login still running ends.
pub(super) fn log_in(id: &str) {
    let run = NEXT.with(|n| n.replace(n.get() + 1));
    LOGIN.with(|l| l.borrow_mut().take());
    with(id, |c| c.folio.sign_in_started(run));
    let wake: folio_cli::Wake = Arc::new(|| super::queue::post(tend_login));
    let started =
        folio_cli::claude_command(&["auth", "login"]).and_then(|cmd| Login::start(cmd, wake));
    match started {
        Ok(l) => LOGIN.with(|slot| *slot.borrow_mut() = Some((run, id.to_string(), l))),
        Err(why) => {
            with(id, |c| {
                if let Some(card) = c.folio.sign_in_card() {
                    card.state = SignInState::Failed(why);
                }
            });
        }
    }
    // Other cards a login ran for are told it ended.
    tend_login();
}

/// Opens the card's sign-in page.
pub(super) fn open_page(id: &str) {
    let url = with(id, |c| c.folio.sign_in_card().and_then(|k| k.url.clone())).flatten();
    if let Some(url) = url {
        open_url(&url);
    }
}

/// Sends the code pasted in the composer to the login.
pub(super) fn send_code(id: &str, code: &str) {
    let code = code.trim();
    if code.is_empty() {
        return notice(id, "Paste the code the browser shows below, then Send Code.");
    }
    let Some(Some(run)) = with(id, |c| c.folio.sign_in_card().filter(|k| k.active()).map(|k| k.run))
    else {
        return;
    };
    let sent = LOGIN.with(|l| {
        let mut l = l.borrow_mut();
        l.as_mut()
            .filter(|(r, _, _)| *r == run)
            .is_some_and(|(_, _, login)| login.send_code(code))
    });
    if sent {
        with(id, |c| {
            if let Some(card) = c.folio.sign_in_card() {
                card.code_sent();
            }
        });
    }
    refresh(id);
}

/// The card's Cancel: its login ends.
pub(super) fn cancel_login(id: &str) {
    let run = with(id, |c| {
        let card = c.folio.sign_in_card().filter(|k| k.active())?;
        card.state = SignInState::Cancelled;
        Some(card.run)
    })
    .flatten();
    if let Some(run) = run {
        LOGIN.with(|l| {
            let mut l = l.borrow_mut();
            if l.as_ref().is_some_and(|(r, _, _)| *r == run) {
                l.take();
            }
        });
    }
    refresh(id);
}

/// `/logout`: the CLI logs out, hidden, and the page says how it went.
fn log_out(id: &str) {
    let mut c = match folio_cli::claude_command(&["auth", "logout"]) {
        Ok(c) => c,
        Err(why) => return notice(id, &format!("Not logged out: {why}")),
    };
    with(id, |c| c.folio.note("Logging out of Claude\u{2026}"));
    let id = id.to_string();
    std::thread::spawn(move || {
        let said = folio_cli::logout_said(c.stdin(std::process::Stdio::null()).output());
        super::queue::post(move || {
            with(&id, |c| c.folio.note(&said));
            refresh(&id);
        });
    });
}

/// Brings every card up to what the login said. Once it succeeded, the
/// session it was for, and any other whose turn failed for want of it,
/// start again on their conversations with the new login.
fn tend_login() {
    let run = LOGIN.with(|l| {
        let l = l.borrow();
        let (run, owner, login) = l.as_ref()?;
        Some((*run, owner.clone(), login.said()))
    });
    let (current, owner, said) = run.unwrap_or_default();
    let mut heard = Heard::read(&said.out, &said.err);
    // Done waits for the end, which brings who it logged in as.
    heard.succeeded &= said.exit.is_some();
    let mut changed = Vec::new();
    let mut waiting = Vec::new();
    let mut done = false;
    let tended = folio::each(|id, chat| {
        let mut moved = false;
        for b in chat.folio.blocks.iter_mut() {
            let Block::SignIn(card) = b else {
                continue;
            };
            if !card.active() {
                continue;
            }
            if card.run != current || id != owner {
                card.state = SignInState::Cancelled;
                moved = true;
                continue;
            }
            if card.hear(&heard, said.exit) {
                moved = true;
                if card.state == SignInState::Done(None) {
                    let email = said.status.as_ref().and_then(|s| s.email.clone());
                    card.state = SignInState::Done(email);
                    done = true;
                }
            }
        }
        if (chat.folio.sign_in_card()).is_some_and(|k| k.state == SignInState::Needed) {
            waiting.push(id.to_string());
        }
        if moved {
            changed.push(id.to_string());
        }
    });
    if !tended {
        // A view holds the Folios: come back once it is done.
        super::queue::post(tend_login);
        return;
    }
    if said.exit.is_some() {
        LOGIN.with(|l| {
            let mut l = l.borrow_mut();
            if l.as_ref().is_some_and(|(r, _, _)| *r == current) {
                l.take();
            }
        });
    }
    for id in &changed {
        refresh(id);
    }
    if !done {
        return;
    }
    let who = said.status.as_ref().and_then(|s| {
        let email = s.email.as_deref()?;
        Some(match &s.subscription {
            Some(plan) => format!("Logged in to Claude as {email} ({plan})."),
            None => format!("Logged in to Claude as {email}."),
        })
    });
    let who = who.unwrap_or_else(|| "Logged in to Claude.".to_string());
    start_again(&owner, &who);
    for id in waiting.iter().filter(|w| **w != owner) {
        start_again(id, &who);
    }
}

/// An image on the pasteboard, or an image file copied in Finder, goes
/// with session `id`'s next prompt. False when the pasteboard holds none.
pub(super) fn paste_image(id: &str) -> bool {
    let Some(image) = pasted_image() else {
        return false;
    };
    with(id, |c| c.images.push(image));
    refresh(id);
    true
}

fn pasted_image() -> Option<ImageData> {
    let pb = NSPasteboard::generalPasteboard();
    if let Some(data) = unsafe { pb.dataForType(NSPasteboardTypePNG) } {
        return ImageData::from_bytes(&data.to_vec());
    }
    if let Some(data) = unsafe { pb.dataForType(NSPasteboardTypeTIFF) } {
        // Screenshots and most apps put TIFF: Claude takes PNG.
        let rep = NSBitmapImageRep::imageRepWithData(&data)?;
        let png = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        return ImageData::from_bytes(&png.to_vec());
    }
    let url = unsafe { pb.stringForType(NSPasteboardTypeFileURL) }?;
    let path = NSURL::URLWithString(&url)?.path()?.to_string();
    let bytes = std::fs::read(path).ok()?;
    ImageData::from_bytes(&bytes)
}

/// Attach Image: a file picked goes with the next prompt.
fn attach(id: &str, mtm: MainThreadMarker) {
    let start = with(id, |c| c.dir.clone());
    let Some(path) = super::dialog::pick_file(mtm, start.as_deref()) else {
        return;
    };
    match std::fs::read(&path).ok().and_then(|b| ImageData::from_bytes(&b)) {
        Some(image) => {
            with(id, |c| c.images.push(image));
        }
        None => notice(id, "That is not an image Claude reads: PNG, JPEG, GIF or WebP."),
    }
    refresh(id);
}
