//! The pure half of the `/` commands the Folio runs itself, shared by
//! both faces: which files `/memory`, `/agents` and `/hooks` list, the
//! permission rules `/permissions` adds and takes out, `/export`'s
//! markdown, and `/plugin`'s lists read from `claude plugin`'s JSON. Each
//! face shows the rows its own way: Windows in a list under the header,
//! the Mac in a menu.
//!
//! A row carries what picking it does in its value, as words split by
//! tabs, which the face reads back.

use std::path::{Path, PathBuf};

use horadric_core::folio::Block;
use serde_json::Value;

/// One row of a command's list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub value: String,
    /// Marked as the one in use, as a plugin that is on.
    pub current: bool,
}

/// The permission lists a settings file holds, in the order shown.
pub const LISTS: [&str; 3] = ["allow", "ask", "deny"];

pub fn row(label: &str, detail: &str, value: &str) -> Row {
    Row {
        label: label.to_string(),
        detail: detail.to_string(),
        value: value.to_string(),
        current: false,
    }
}

/// The rows of the files that are there, each opening it.
pub fn existing(files: Vec<(String, PathBuf)>) -> Vec<Row> {
    (files.into_iter())
        .filter(|(_, p)| p.is_file())
        .map(|(label, p)| Row {
            label,
            detail: p.display().to_string(),
            value: format!("view\t{}", p.display()),
            current: false,
        })
        .collect()
}

/// Where Claude Code reads memory from, project first as `/memory` lists.
pub fn memory_files(dir: &Path, claude: &Path) -> Vec<(String, PathBuf)> {
    vec![
        ("Project".into(), dir.join("CLAUDE.md")),
        ("Project".into(), dir.join(".claude").join("CLAUDE.md")),
        ("Project, local".into(), dir.join("CLAUDE.local.md")),
        ("User".into(), claude.join("CLAUDE.md")),
    ]
}

/// The subagents' files, the project's then the user's, by name.
pub fn agent_files(dir: &Path, claude: &Path) -> Vec<(String, PathBuf)> {
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
pub fn settings_files(dir: &Path, claude: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Project, local", local_settings(dir)),
        ("Project", dir.join(".claude").join("settings.json")),
        ("User", claude.join("settings.json")),
    ]
}

/// Where a rule added from the Folio goes: the project's own file that
/// is not committed, as Claude Code's "always allow" puts them.
pub fn local_settings(dir: &Path) -> PathBuf {
    dir.join(".claude").join("settings.local.json")
}

pub fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// Reads, changes and writes back a settings file, made when missing. A
/// file that does not parse is left alone rather than overwritten.
pub fn edit_json(path: &Path, change: impl FnOnce(&mut Value)) -> Result<(), String> {
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
pub fn hooks_in(settings: &Value) -> Vec<(String, String, String)> {
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
pub fn rules_in<'a>(settings: &'a Value, list: &str) -> Vec<&'a str> {
    (settings.pointer(&format!("/permissions/{list}")))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// `allow Bash(npm test)` read as the list and the rule.
pub fn add_rule_words(words: &str) -> Option<(&'static str, &str)> {
    let (list, rule) = words.split_once(char::is_whitespace)?;
    let list = LISTS.into_iter().find(|l| l.eq_ignore_ascii_case(list))?;
    let rule = rule.trim();
    (!rule.is_empty()).then_some((list, rule))
}

/// Adds `rule` to `list`, once, making what is missing on the way.
pub fn add_rule(settings: &mut Value, list: &str, rule: &str) {
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

pub fn remove_rule(settings: &mut Value, list: &str, rule: &str) {
    if let Some(rules) =
        (settings.pointer_mut(&format!("/permissions/{list}"))).and_then(Value::as_array_mut)
    {
        rules.retain(|r| r.as_str() != Some(rule));
    }
}

/// The conversation as a reader would want it: what was asked, what was
/// said, and a line per tool step. Thinking and the cards' insides stay
/// out, as Claude Code's own export leaves them.
pub fn markdown(blocks: &[Block]) -> String {
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
pub fn default_name(blocks: &[Block], now: std::time::SystemTime) -> String {
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
pub fn civil(days: i64) -> (i64, u32, u32) {
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

pub fn plugin_rows(json: &str) -> Vec<Row> {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    (v.as_array().into_iter().flatten())
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?;
            let on = p.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let scope = p.get("scope").and_then(Value::as_str).unwrap_or("");
            Some(Row {
                label: id.to_string(),
                detail: format!("{}, {scope}", if on { "On" } else { "Off" }),
                value: format!("plugin\t{id}\t{on}"),
                current: on,
            })
        })
        .collect()
}

pub fn market_rows(json: &str) -> Vec<Row> {
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

/// `/hooks`: every hook in the settings Claude Code reads, each opening
/// its file.
pub fn hook_rows(dir: &Path, claude: &Path) -> Vec<Row> {
    let mut rows = Vec::new();
    for (place, path) in settings_files(dir, claude) {
        for (event, matcher, command) in hooks_in(&read_json(&path)) {
            let label = match matcher.as_str() {
                "" | "*" => event,
                m => format!("{event} {m}"),
            };
            rows.push(Row {
                label,
                detail: format!("{place}: {command}"),
                value: format!("view\t{}", path.display()),
                current: false,
            });
        }
    }
    rows
}

/// `/permissions` alone: every rule, each removed when picked, and a row
/// to add one.
pub fn permission_rows(dir: &Path, claude: &Path) -> Vec<Row> {
    let mut rows = vec![row(
        "Add a rule\u{2026}",
        "/permissions allow, ask or deny, then the rule",
        "put\t/permissions allow ",
    )];
    for (place, path) in settings_files(dir, claude) {
        let v = read_json(&path);
        for list in LISTS {
            for rule in rules_in(&v, list) {
                rows.push(Row {
                    label: rule.to_string(),
                    detail: format!("{list}, {place}. Pick to take it out"),
                    value: format!("unrule\t{}\t{list}\t{rule}", path.display()),
                    current: false,
                });
            }
        }
    }
    rows
}

/// `/permissions allow <rule>` and the like: the rule added to the
/// project's local settings, and what to say of it.
pub fn add_rule_said(dir: &Path, words: &str) -> String {
    match add_rule_words(words) {
        Some((list, rule)) => match edit_json(&local_settings(dir), |v| add_rule(v, list, rule)) {
            Ok(()) => format!("{rule} added to {list} in .claude/settings.local.json"),
            Err(e) => format!("Not saved: {e}"),
        },
        None => "Say /permissions allow, ask or deny, then the rule.".to_string(),
    }
}

/// A rule picked from `/permissions` taken out of `path`, and what to say
/// of it.
pub fn remove_rule_said(path: &Path, list: &str, rule: &str) -> String {
    let said = edit_json(path, |v| remove_rule(v, list, rule));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    match said {
        Ok(()) => format!("{rule} taken out of {list} in {name}"),
        Err(e) => format!("Not saved: {e}"),
    }
}

/// What `/export [file]` writes to: the name typed, made markdown, or one
/// from the day and the first prompt.
pub fn export_name(blocks: &[Block], words: &str) -> String {
    match words.trim_matches('"') {
        "" => default_name(blocks, std::time::SystemTime::now()),
        n if n.ends_with(".md") || n.ends_with(".txt") => n.to_string(),
        n => format!("{n}.md"),
    }
}

/// The rows `/plugin` opens with: install, the marketplaces, then each
/// plugin from `claude plugin list --json`.
pub fn plugin_menu(json: &str) -> Vec<Row> {
    let mut rows = vec![
        row(
            "Install a plugin\u{2026}",
            "/plugin install name@marketplace",
            "put\t/plugin install ",
        ),
        row("Marketplaces", "Add, update or remove", "markets"),
    ];
    rows.extend(plugin_rows(json));
    rows
}

/// The rows of the marketplaces, from `claude plugin marketplace list
/// --json`, after a row to add one.
pub fn market_menu(json: &str) -> Vec<Row> {
    let mut rows = vec![row(
        "Add a marketplace\u{2026}",
        "A URL, a path or owner/repo",
        "put\t/plugin marketplace add ",
    )];
    rows.extend(market_rows(json));
    rows
}

/// What a plugin's row opens: turn it on or off, update, uninstall.
pub fn plugin_actions(id: &str, on: bool) -> Vec<Row> {
    let toggle = if on { "Turn off" } else { "Turn on" };
    let verb = if on { "disable" } else { "enable" };
    vec![
        row(toggle, "", &format!("run\tplugin {verb} {id}")),
        row(
            "Update",
            "Restart to apply",
            &format!("run\tplugin update {id}"),
        ),
        row("Uninstall", "", &format!("run\tplugin uninstall {id}")),
    ]
}

/// What a marketplace's row opens.
pub fn market_actions(name: &str) -> Vec<Row> {
    vec![
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
    ]
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
            result: None,
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
