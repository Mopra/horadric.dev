//! The `/` commands the Folio runs itself. Over stream-json the CLI says
//! "isn't available in this environment" to each of these, since in a
//! terminal they draw a screen of their own; here they open a file in the
//! viewer, a list under the header, or run `claude` hidden and show what
//! it said. `/login` and `/logout` are in [`super::sign_in`].
//!
//! A list's rows carry what picking them does in their value, as words
//! split by tabs, read back by [`act`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horadric_core::folio::Block;
use horadric_hooks::transcript::Past;
use serde_json::Value;

use super::composer::{choose, notice, put, session_id};
use super::picker::Item;
use super::{redraw, with, Chat};
use crate::app::{self, Input};

/// The permission lists a settings file holds, in the order shown.
const LISTS: [&str; 3] = ["allow", "ask", "deny"];

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
    let mut c = std::process::Command::new(&program);
    c.args(crate::shell::run_args(&program, command))
        .current_dir(&chat.dir)
        .stdin(std::process::Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    let (router, command) = (super::router(), command.to_string());
    std::thread::spawn(move || {
        let (output, code) = match c.output() {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&err);
                }
                (text, o.status.code())
            }
            Err(e) => (format!("Could not start {}: {e}", program.display()), None),
        };
        if let Ok(mut d) = SHELLS.lock() {
            d.push((id, at, command, output, code));
        }
        super::sign_in::post(router);
    });
}

thread_local! {
    /// The conversations a `/resume` list offered, by session.
    static OFFERED: RefCell<HashMap<String, Vec<Past>>> = RefCell::new(HashMap::new());
}

/// Runs native command `name` with the `words` typed after it.
pub(super) fn run(chat: &mut Chat, id: &str, name: &str, words: &str) {
    match name {
        "memory" => {
            let rows = existing(memory_files(&chat.dir, &crate::inventory::claude_dir()));
            choose(
                chat,
                rows,
                "No CLAUDE.md yet: /init writes one for the project.",
            );
        }
        "agents" => {
            let rows = existing(agent_files(&chat.dir, &crate::inventory::claude_dir()));
            choose(
                chat,
                rows,
                "No subagents: ask Claude to write one in .claude/agents.",
            );
        }
        "hooks" => {
            let mut rows = Vec::new();
            for (place, path) in settings_files(&chat.dir, &crate::inventory::claude_dir()) {
                for (event, matcher, command) in hooks_in(&read_json(&path)) {
                    let label = match matcher.as_str() {
                        "" | "*" => event,
                        m => format!("{event} {m}"),
                    };
                    rows.push(Item {
                        label,
                        detail: format!("{place}: {command}"),
                        value: format!("view\t{}", path.display()),
                        ..Default::default()
                    });
                }
            }
            choose(chat, rows, "No hooks in the settings.");
        }
        "permissions" if words.is_empty() => permissions(chat),
        "permissions" => match add_rule_words(words) {
            Some((list, rule)) => {
                let path = local_settings(&chat.dir);
                let said = edit_json(&path, |v| add_rule(v, list, rule));
                notice(
                    chat,
                    &match said {
                        Ok(()) => format!("{rule} added to {list} in .claude/settings.local.json"),
                        Err(e) => format!("Not saved: {e}"),
                    },
                );
            }
            None => notice(chat, "Say /permissions allow, ask or deny, then the rule."),
        },
        "plugin" if words.is_empty() => {
            hidden(chat, id, &["plugin", "list", "--json"], Then::Plugins)
        }
        "plugin" => {
            let mut args = vec!["plugin"];
            args.extend(words.split_whitespace());
            hidden(chat, id, &args, Then::Show(format!("/plugin {words}")));
        }
        "theme" => app::push(Input::PaneAsk(id.to_string(), app::PaneAsk::Theme)),
        "resume" => {
            let dir = chat.dir.to_string_lossy().into_owned();
            // Not the one this chat holds: it is already open.
            let held: Vec<String> = chat.folio.session_id.iter().cloned().collect();
            let past = horadric_hooks::transcript::history(&dir, &held, 10);
            let now = std::time::SystemTime::now();
            let rows = (past.iter().enumerate())
                .map(|(i, p)| {
                    let ago = now.duration_since(p.modified).unwrap_or_default();
                    let (title, when) = crate::history::label(&p.title.text, ago)
                        .split_once('\t')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                        .unwrap_or_default();
                    Item {
                        label: title,
                        detail: when,
                        value: format!("resume\t{i}"),
                        ..Default::default()
                    }
                })
                .collect();
            OFFERED.with(|o| o.borrow_mut().insert(id.to_string(), past));
            choose(chat, rows, "No earlier conversations in this folder.");
        }
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
            let said = edit_json(Path::new(path), |v| remove_rule(v, list, rule));
            let name = Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            notice(
                chat,
                &match said {
                    Ok(()) => format!("{rule} taken out of {list} in {name}"),
                    Err(e) => format!("Not saved: {e}"),
                },
            );
        }
        ["resume", i] => {
            let past = OFFERED.with(|o| {
                let o = o.borrow();
                o.get(&id)?.get(i.parse::<usize>().ok()?).cloned()
            });
            if let Some(p) = past {
                app::push(Input::Resume(chat.dir.clone(), p));
            }
        }
        ["plugin", id_, on] => {
            let toggle = if *on == "true" { "Turn off" } else { "Turn on" };
            let verb = if *on == "true" { "disable" } else { "enable" };
            let rows = vec![
                row(toggle, "", &format!("run\tplugin {verb} {id_}")),
                row(
                    "Update",
                    "Restart to apply",
                    &format!("run\tplugin update {id_}"),
                ),
                row("Uninstall", "", &format!("run\tplugin uninstall {id_}")),
            ];
            choose(chat, rows, "");
        }
        ["market", name] => {
            let rows = vec![
                row(
                    "Update",
                    "",
                    &format!("run\tplugin marketplace update {name}"),
                ),
                row(
                    "Remove",
                    "",
                    &format!("run\tplugin marketplace remove {name}"),
                ),
            ];
            choose(chat, rows, "");
        }
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

fn row(label: &str, detail: &str, value: &str) -> Item {
    Item {
        label: label.to_string(),
        detail: detail.to_string(),
        value: value.to_string(),
        ..Default::default()
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

/// The rows of the files that are there, each opening it.
fn existing(files: Vec<(String, PathBuf)>) -> Vec<Item> {
    (files.into_iter())
        .filter(|(_, p)| p.is_file())
        .map(|(label, p)| Item {
            label,
            detail: p.display().to_string(),
            value: format!("view\t{}", p.display()),
            ..Default::default()
        })
        .collect()
}

/// Where Claude Code reads memory from, project first as `/memory` lists.
fn memory_files(dir: &Path, claude: &Path) -> Vec<(String, PathBuf)> {
    vec![
        ("Project".into(), dir.join("CLAUDE.md")),
        ("Project".into(), dir.join(".claude").join("CLAUDE.md")),
        ("Project, local".into(), dir.join("CLAUDE.local.md")),
        ("User".into(), claude.join("CLAUDE.md")),
    ]
}

/// The subagents' files, the project's then the user's, by name.
fn agent_files(dir: &Path, claude: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for (place, folder) in [
        ("Project", dir.join(".claude")),
        ("User", claude.to_path_buf()),
    ] {
        let Ok(entries) = std::fs::read_dir(folder.join("agents")) else {
            continue;
        };
        let mut found: Vec<PathBuf> = (entries.flatten())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        found.sort();
        for p in found {
            let name = p.file_stem().unwrap_or_default().to_string_lossy();
            out.push((format!("{name} ({place})"), p));
        }
    }
    out
}

/// The settings files Claude Code reads, most particular first.
fn settings_files(dir: &Path, claude: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Project, local", local_settings(dir)),
        ("Project", dir.join(".claude").join("settings.json")),
        ("User", claude.join("settings.json")),
    ]
}

/// Where a rule added from the Folio goes: the project's own file that
/// is not committed, as Claude Code's "always allow" puts them.
fn local_settings(dir: &Path) -> PathBuf {
    dir.join(".claude").join("settings.local.json")
}

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// Reads, changes and writes back a settings file, made when missing. A
/// file that does not parse is left alone rather than overwritten.
fn edit_json(path: &Path, change: impl FnOnce(&mut Value)) -> Result<(), String> {
    let mut v = match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s)
            .map_err(|e| format!("{} does not parse: {e}", path.display()))?,
        _ => Value::Object(Default::default()),
    };
    change(&mut v);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| e.to_string())
}

/// Every hook in a settings file: its event, matcher and command.
fn hooks_in(settings: &Value) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let Some(events) = settings.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in events {
        for g in groups.as_array().into_iter().flatten() {
            let matcher = g.get("matcher").and_then(Value::as_str).unwrap_or("");
            for h in g
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let command = (h.get("command").or_else(|| h.get("url")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                out.push((event.clone(), matcher.to_string(), command.to_string()));
            }
        }
    }
    out
}

/// The rules of one list in a settings file.
fn rules_in<'a>(settings: &'a Value, list: &str) -> Vec<&'a str> {
    (settings.pointer(&format!("/permissions/{list}")))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// `allow Bash(npm test)` read as the list and the rule.
fn add_rule_words(words: &str) -> Option<(&'static str, &str)> {
    let (list, rule) = words.split_once(char::is_whitespace)?;
    let list = LISTS.into_iter().find(|l| l.eq_ignore_ascii_case(list))?;
    let rule = rule.trim();
    (!rule.is_empty()).then_some((list, rule))
}

/// Adds `rule` to `list`, once, making what is missing on the way.
fn add_rule(settings: &mut Value, list: &str, rule: &str) {
    if !settings.is_object() {
        *settings = Value::Object(Default::default());
    }
    let perms = (settings.as_object_mut().expect("made an object"))
        .entry("permissions")
        .or_insert_with(|| Value::Object(Default::default()));
    if !perms.is_object() {
        *perms = Value::Object(Default::default());
    }
    let rules = (perms.as_object_mut().expect("made an object"))
        .entry(list)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !rules.is_array() {
        *rules = Value::Array(Vec::new());
    }
    let rules = rules.as_array_mut().expect("made an array");
    if !rules.iter().any(|r| r.as_str() == Some(rule)) {
        rules.push(Value::String(rule.to_string()));
    }
}

fn remove_rule(settings: &mut Value, list: &str, rule: &str) {
    if let Some(rules) =
        (settings.pointer_mut(&format!("/permissions/{list}"))).and_then(Value::as_array_mut)
    {
        rules.retain(|r| r.as_str() != Some(rule));
    }
}

/// `/permissions` alone: every rule, each removed when picked, and a row
/// to add one.
fn permissions(chat: &mut Chat) {
    let mut rows = vec![row(
        "Add a rule\u{2026}",
        "/permissions allow, ask or deny, then the rule",
        "put\t/permissions allow ",
    )];
    for (place, path) in settings_files(&chat.dir, &crate::inventory::claude_dir()) {
        let v = read_json(&path);
        for list in LISTS {
            for rule in rules_in(&v, list) {
                rows.push(Item {
                    label: rule.to_string(),
                    detail: format!("{list}, {place}. Pick to take it out"),
                    value: format!("unrule\t{}\t{list}\t{rule}", path.display()),
                    ..Default::default()
                });
            }
        }
    }
    choose(chat, rows, "");
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
    let name = match words.trim_matches('"') {
        "" => default_name(&chat.folio.blocks, std::time::SystemTime::now()),
        n if n.ends_with(".md") || n.ends_with(".txt") => n.to_string(),
        n => format!("{n}.md"),
    };
    let path = chat.dir.join(&name);
    match std::fs::write(&path, markdown(&chat.folio.blocks)) {
        Ok(()) => {
            notice(chat, &format!("Saved to {name}"));
            view(chat, &path);
        }
        Err(e) => notice(chat, &format!("Not saved: {e}")),
    }
}

/// The conversation as a reader would want it: what was asked, what was
/// said, and a line per tool step. Thinking and the cards' insides stay
/// out, as Claude Code's own export leaves them.
fn markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Prompt(t) => {
                out.push_str("## You\n\n");
                out.push_str(t.trim());
                out.push_str("\n\n");
            }
            Block::Reply(t) if !t.trim().is_empty() => {
                out.push_str("## Claude\n\n");
                out.push_str(t.trim());
                out.push_str("\n\n");
            }
            Block::Tools(steps) => {
                for s in steps {
                    match s.summary.as_str() {
                        "" => out.push_str(&format!("- {}\n", s.tool)),
                        sum => {
                            out.push_str(&format!("- {}: `{}`\n", s.tool, sum.replace('`', "'")))
                        }
                    }
                }
                out.push('\n');
            }
            Block::Plan { plan, .. } => {
                out.push_str("## Plan\n\n");
                out.push_str(plan.trim());
                out.push_str("\n\n");
            }
            _ => {}
        }
    }
    out
}

/// `2026-10-31-fix-the-tiles.md`: the day and the first prompt's words.
fn default_name(blocks: &[Block], now: std::time::SystemTime) -> String {
    let days = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    let (y, m, d) = civil(days);
    let first = blocks.iter().find_map(|b| match b {
        Block::Prompt(t) => Some(t.as_str()),
        _ => None,
    });
    let slug: String = first
        .unwrap_or("conversation")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    let slug = if slug.is_empty() {
        "conversation".into()
    } else {
        slug
    };
    format!("{y:04}-{m:02}-{d:02}-{slug}.md")
}

/// The calendar date `days` after 1970-01-01, by Howard Hinnant's
/// algorithm, which needs no table of month lengths.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
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
    std::thread::spawn(move || {
        let said = match c.stdin(std::process::Stdio::null()).output() {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                match (o.status.success(), out.is_empty()) {
                    (true, _) => Ok(out),
                    (false, true) => Err(err),
                    (false, false) => Err(format!("{out}\n{err}").trim().to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        };
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
                    chat.folio.blocks.push(Block::Reply(fenced(&text)));
                    chat.follow = true;
                }
                (Then::Plugins, Ok(json)) => {
                    let mut rows = vec![
                        row(
                            "Install a plugin\u{2026}",
                            "/plugin install name@marketplace",
                            "put\t/plugin install ",
                        ),
                        row("Marketplaces", "Add, update or remove", "markets"),
                    ];
                    rows.extend(plugin_rows(&json));
                    choose(chat, rows, "");
                }
                (Then::Markets, Ok(json)) => {
                    let mut rows = vec![row(
                        "Add a marketplace\u{2026}",
                        "A URL, a path or owner/repo",
                        "put\t/plugin marketplace add ",
                    )];
                    rows.extend(market_rows(&json));
                    choose(chat, rows, "");
                }
                (_, Err(e)) => notice(chat, e.lines().next().unwrap_or("claude failed")),
            }
        });
        redraw(&id);
    }
}

/// A CLI's output as a block of code, so its lines and columns stay.
fn fenced(text: &str) -> String {
    format!("```text\n{}\n```", text.replace("```", "'''"))
}

fn plugin_rows(json: &str) -> Vec<Item> {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    (v.as_array().into_iter().flatten())
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?;
            let on = p.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let scope = p.get("scope").and_then(Value::as_str).unwrap_or("");
            Some(Item {
                label: id.to_string(),
                detail: format!("{}, {scope}", if on { "On" } else { "Off" }),
                value: format!("plugin\t{id}\t{on}"),
                current: on,
                ..Default::default()
            })
        })
        .collect()
}

fn market_rows(json: &str) -> Vec<Item> {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    (v.as_array().into_iter().flatten())
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?;
            let from = (m
                .get("repo")
                .or_else(|| m.get("url"))
                .or_else(|| m.get("path")))
            .and_then(Value::as_str)
            .unwrap_or("");
            Some(row(name, from, &format!("market\t{name}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_rule_is_added_once_and_taken_out() {
        let mut v = json!({"model": "opus"});
        add_rule(&mut v, "allow", "Bash(npm test)");
        add_rule(&mut v, "allow", "Bash(npm test)");
        assert_eq!(rules_in(&v, "allow"), ["Bash(npm test)"]);
        assert_eq!(v["model"], "opus");
        remove_rule(&mut v, "allow", "Bash(npm test)");
        assert!(rules_in(&v, "allow").is_empty());
        let mut odd = json!({"permissions": {"deny": "x"}});
        add_rule(&mut odd, "deny", "Read(.env)");
        assert_eq!(rules_in(&odd, "deny"), ["Read(.env)"]);
    }

    #[test]
    fn the_words_of_permissions_name_a_list_and_a_rule() {
        assert_eq!(
            add_rule_words("allow Bash(git log:*)"),
            Some(("allow", "Bash(git log:*)"))
        );
        assert_eq!(
            add_rule_words("Deny  Read(.env) "),
            Some(("deny", "Read(.env)"))
        );
        assert_eq!(add_rule_words("allow"), None);
        assert_eq!(add_rule_words("permit Bash"), None);
    }

    #[test]
    fn hooks_are_listed_with_their_matcher() {
        let v = json!({"hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check.sh"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "done.sh"}, {"type": "http", "url": "http://x"}]}]
        }});
        let all = hooks_in(&v);
        assert!(all.contains(&("PreToolUse".into(), "Bash".into(), "check.sh".into())));
        assert!(all.contains(&("Stop".into(), "".into(), "done.sh".into())));
        assert!(all.contains(&("Stop".into(), "".into(), "http://x".into())));
        assert!(hooks_in(&Value::Null).is_empty());
    }

    #[test]
    fn an_export_has_the_prompts_replies_and_steps() {
        use horadric_core::folio::{Step, StepState};
        let step = Step {
            id: "t".into(),
            tool: "Bash".into(),
            summary: "cargo check".into(),
            state: StepState::Done,
            input: json!({}),
            edit: None,
            lane: None,
            output: String::new(),
            images: Vec::new(),
            began: None,
            ended: None,
        };
        let blocks = vec![
            Block::Prompt("Fix the tiles".into()),
            Block::Thinking("hidden".into()),
            Block::Tools(vec![step]),
            Block::Reply("Done.".into()),
        ];
        let md = markdown(&blocks);
        assert!(md.starts_with("## You\n\nFix the tiles\n\n"));
        assert!(md.contains("- Bash: `cargo check`\n"));
        assert!(md.contains("## Claude\n\nDone."));
        assert!(!md.contains("hidden"));
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_404_800);
        assert_eq!(default_name(&blocks, at), "2026-10-31-fix-the-tiles.md");
        assert_eq!(default_name(&[], at), "2026-10-31-conversation.md");
    }

    #[test]
    fn days_become_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_782), (2024, 2, 29));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn plugins_and_markets_become_rows() {
        let p = plugin_rows(
            r#"[{"id":"a@m","enabled":true,"scope":"user"},{"id":"b@m","enabled":false,"scope":"project"}]"#,
        );
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].value, "plugin\ta@m\ttrue");
        assert_eq!(p[1].detail, "Off, project");
        let m = market_rows(r#"[{"name":"official","repo":"anthropics/x"}]"#);
        assert_eq!(m[0].detail, "anthropics/x");
        assert!(plugin_rows("not json").is_empty());
    }
}
