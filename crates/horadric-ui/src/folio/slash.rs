//! The `/` commands the Folio runs itself. Over stream-json the CLI says
//! "isn't available in this environment" to each of these, since in a
//! terminal they draw a screen of their own; here they open a file in the
//! viewer, a list under the header, or run `claude` hidden and show what
//! it said. `/login` and `/logout` are in [`super::sign_in`]. What the
//! lists hold and what picking a row changes is shared with the Mac, in
//! [`crate::folio_slash`]; this is how Windows shows them.
//!
//! A list's rows carry what picking them does in their value, as words
//! split by tabs, read back by [`act`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horadric_core::folio::Block;

use super::composer::{choose, notice, put, session_id};
use super::picker::Item;
use super::{redraw, with, Chat};
use crate::app::{self, Input};
use crate::folio_cli;
use crate::folio_slash::{self as shared, Row};

/// What a hidden `claude` run is for, once it has said its piece.
#[derive(Debug, Clone, PartialEq)]
enum Then {
    /// Shown in the conversation under this line.
    Show(String),
    /// `plugin list --json`, as a list to pick from.
    Plugins,
    /// `plugin marketplace list --json`, the same.
    Markets,
}

/// A hidden run that ended: the session that asked, what to do next, and
/// what it printed or why it failed.
type Done = (String, Then, Result<String, String>);

/// Hidden runs that ended, for the UI thread.
static DONE: Mutex<Vec<Done>> = Mutex::new(Vec::new());

/// A `!` command that ended: the session, its block and command, what it
/// printed and its exit code.
type ShellDone = (String, usize, String, String, Option<i32>);
static SHELLS: Mutex<Vec<ShellDone>> = Mutex::new(Vec::new());

/// Runs a `!` command in the project's folder with the shell a terminal
/// gets, off the UI thread, its output shown as it ends and sent to
/// Claude with the next prompt.
pub(super) fn run_shell(chat: &mut Chat, command: &str) {
    let Some(id) = session_id(chat) else {
        return;
    };
    let Some(program) = crate::console::shell_program() else {
        notice(chat, "No shell found to run that in.");
        return;
    };
    let at = chat.folio.shell_started(command);
    chat.follow = true;
    let (router, said) = (super::router(), command.to_string());
    folio_cli::run_shell(program, command, &chat.dir, move |output, code| {
        if let Ok(mut d) = SHELLS.lock() {
            d.push((id, at, said, output, code));
        }
        super::sign_in::post(router);
    });
}

/// A shared row as the list under the header shows it.
fn item(r: Row) -> Item {
    Item {
        label: r.label,
        detail: r.detail,
        value: r.value,
        current: r.current,
        ..Default::default()
    }
}

fn items(rows: Vec<Row>) -> Vec<Item> {
    rows.into_iter().map(item).collect()
}

/// Runs native command `name` with the `words` typed after it.
pub(super) fn run(chat: &mut Chat, id: &str, name: &str, words: &str) {
    let claude = crate::inventory::claude_dir();
    match name {
        "memory" => {
            let rows = items(shared::existing(shared::memory_files(&chat.dir, &claude)));
            choose(
                chat,
                rows,
                "No CLAUDE.md yet: /init writes one for the project.",
            );
        }
        "agents" => {
            let rows = items(shared::existing(shared::agent_files(&chat.dir, &claude)));
            choose(
                chat,
                rows,
                "No subagents: ask Claude to write one in .claude/agents.",
            );
        }
        "hooks" => {
            let rows = items(shared::hook_rows(&chat.dir, &claude));
            choose(chat, rows, "No hooks in the settings.");
        }
        "permissions" if words.is_empty() => {
            let rows = items(shared::permission_rows(&chat.dir, &claude));
            choose(chat, rows, "");
        }
        "permissions" => {
            let said = shared::add_rule_said(&chat.dir, words);
            notice(chat, &said);
        }
        "plugin" if words.is_empty() => {
            hidden(chat, id, &["plugin", "list", "--json"], Then::Plugins)
        }
        "plugin" => {
            let mut args = vec!["plugin"];
            args.extend(words.split_whitespace());
            hidden(chat, id, &args, Then::Show(format!("/plugin {words}")));
        }
        "theme" => app::push(Input::PaneAsk(id.to_string(), app::PaneAsk::Theme)),
        "export" => export(chat, words),
        "add-dir" => add_dir(chat, id, words),
        "exit" => app::push(Input::CloseFolio(id.to_string())),
        _ => {}
    }
}

/// Does what a native command's list row says.
pub(super) fn act(chat: &mut Chat, value: &str) {
    let Some(id) = session_id(chat) else {
        return;
    };
    let w: Vec<&str> = value.split('\t').collect();
    match w.as_slice() {
        ["view", path] => view(chat, Path::new(path)),
        ["put", text] => put(chat, text),
        ["unrule", path, list, rule] => {
            let said = shared::remove_rule_said(Path::new(path), list, rule);
            notice(chat, &said);
        }
        ["plugin", id_, on] => choose(chat, items(shared::plugin_actions(id_, *on == "true")), ""),
        ["market", name] => choose(chat, items(shared::market_actions(name)), ""),
        ["markets"] => hidden(
            chat,
            &id,
            &["plugin", "marketplace", "list", "--json"],
            Then::Markets,
        ),
        ["run", words] => {
            let args: Vec<&str> = words.split_whitespace().collect();
            hidden(chat, &id, &args, Then::Show(format!("/{words}")));
        }
        _ => {}
    }
}

/// Shows a file in the project's file viewer.
fn view(chat: &Chat, path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    app::push(Input::View(
        chat.key.clone(),
        dir.to_path_buf(),
        name.to_string_lossy().into_owned(),
        None,
    ));
}

/// `/add-dir`: the Folio starts again on its conversation, the folder
/// added, since the CLI takes folders only as it starts.
fn add_dir(chat: &mut Chat, id: &str, words: &str) {
    let typed = words.trim_matches('"');
    if typed.is_empty() {
        put(chat, "/add-dir ");
        return;
    }
    let path = chat.dir.join(typed);
    if !path.is_dir() {
        notice(chat, &format!("{typed} is not a folder."));
        return;
    }
    if chat.folio.busy || !chat.folio.pending_asks().is_empty() {
        notice(chat, "/add-dir starts Claude again: once it rests.");
        return;
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let path = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
    chat.folio.note(&format!("Adding {}", path.display()));
    app::push(Input::FolioAddDir(id.to_string(), path));
}

/// `/export [file]`: the conversation as markdown, in the project's
/// folder, opened in the viewer.
fn export(chat: &mut Chat, words: &str) {
    let name = shared::export_name(&chat.folio.blocks, words);
    let path = chat.dir.join(&name);
    match std::fs::write(&path, shared::markdown(&chat.folio.blocks)) {
        Ok(()) => {
            notice(chat, &format!("Saved to {name}"));
            view(chat, &path);
        }
        Err(e) => notice(chat, &format!("Not saved: {e}")),
    }
}

/// Runs `claude` with `args`, no window, and does `then` with what it
/// said once it ends.
fn hidden(chat: &mut Chat, id: &str, args: &[&str], then: Then) {
    let mut c = match super::sign_in::command(args) {
        Ok(c) => c,
        Err(why) => {
            notice(chat, &why);
            return;
        }
    };
    c.current_dir(&chat.dir);
    notice(chat, &format!("claude {}\u{2026}", args.join(" ")));
    let (router, id) = (super::router(), id.to_string());
    folio_cli::run_hidden(c, move |said| {
        if let Ok(mut d) = DONE.lock() {
            d.push((id, then, said));
        }
        super::sign_in::post(router);
    });
}

/// Brings in what hidden runs said, on the UI thread.
pub(super) fn tend() {
    let shells = SHELLS.lock().map(|mut d| std::mem::take(&mut *d));
    for (id, at, command, output, code) in shells.unwrap_or_default() {
        with(&id, |chat| {
            chat.folio.shell_done(at, &command, &output, code)
        });
        redraw(&id);
    }
    let done = DONE.lock().map(|mut d| std::mem::take(&mut *d));
    for (id, then, said) in done.unwrap_or_default() {
        with(&id, |chat| {
            notice(chat, "");
            match (then, said) {
                (Then::Show(line), said) => {
                    let text = match said {
                        Ok(t) if t.is_empty() => "Done.".to_string(),
                        Ok(t) => t,
                        Err(e) => e,
                    };
                    chat.folio.note(&line);
                    chat.folio
                        .blocks
                        .push(Block::Reply(folio_cli::fenced(&text)));
                    chat.follow = true;
                }
                (Then::Plugins, Ok(json)) => choose(chat, items(shared::plugin_menu(&json)), ""),
                (Then::Markets, Ok(json)) => choose(chat, items(shared::market_menu(&json)), ""),
                (_, Err(e)) => notice(chat, e.lines().next().unwrap_or("claude failed")),
            }
        });
        redraw(&id);
    }
}
