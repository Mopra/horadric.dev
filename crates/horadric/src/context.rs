//! `horadric context`: what every session of a project carries before it
//! does any work (connectors, plugins, skills, instructions), what each
//! costs a turn, whether the project's sessions ever used it, and the
//! switch that turns it off. The reading is `horadric_ui::inventory`, the
//! sums and switches `horadric_core::context`; this is only the words.

use std::path::{Path, PathBuf};

use horadric_core::context::{Source, Switch};
use horadric_ui::inventory::{self, Inventory, Line};

const USAGE: &str = "\
usage: horadric context            what this project's sessions load, what they use
       horadric context off NAME   turn one off for the project's new sessions
       horadric context on NAME    turn it back on
       horadric context --all      every known project in one line each";

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None => {
            let main = main_tree();
            let inv = inventory::gather(&main);
            print!("{}", table(&inv, unix_now()));
            Ok(())
        }
        Some("--all") => all(),
        Some(verb @ ("off" | "on")) => {
            let query = args[1..].join(" ");
            if query.trim().is_empty() {
                return Err(format!("say which: horadric context {verb} <name>"));
            }
            switch(&main_tree(), &query, verb == "on")
        }
        _ => Err(USAGE.into()),
    }
}

/// The project's main folder: where `horadric quest` finds it for a
/// session Horadric started, else the main working tree of the repository
/// here, else here. A worktree's sessions are the main tree's project.
fn main_tree() -> PathBuf {
    if let Some(m) = std::env::var_os(horadric_hooks::TASKS_ENV).filter(|s| !s.is_empty()) {
        return PathBuf::from(m);
    }
    let here = std::env::current_dir().unwrap_or_default();
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--show-toplevel",
        ])
        .current_dir(&here)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| main_of(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(here)
}

/// The main working tree from what `git rev-parse --git-common-dir
/// --show-toplevel` printed: the folder holding the shared `.git`, or the
/// top level when the repository keeps it elsewhere.
fn main_of(out: &str) -> Option<PathBuf> {
    let mut lines = out.lines().map(str::trim);
    let common = lines.next()?;
    let top = lines.next().filter(|t| !t.is_empty());
    match common
        .strip_suffix("/.git")
        .or_else(|| common.strip_suffix("\\.git"))
    {
        Some(root) => Some(PathBuf::from(root)),
        None => top.map(PathBuf::from),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What a switch writes, by the key of the project's settings it lives in.
fn how(switch: &Switch) -> &'static str {
    match switch {
        Switch::Plugin(_) => "enabledPlugins",
        Switch::Server(_) => "deniedMcpServers",
        Switch::Skill(_) => "skillOverrides",
        Switch::None => "not switchable here",
    }
}

/// How long ago a transcript's timestamp was, as `3d ago`. The transcript
/// writes milliseconds, which the time reader does not take.
fn ago(stamp: &str, now: u64) -> String {
    let whole = stamp.get(..19).map(|s| format!("{s}Z"));
    let Some(at) = whole.as_deref().and_then(horadric_core::tasks::parse_utc) else {
        return "at an unknown time".into();
    };
    let secs = now.saturating_sub(at);
    match secs {
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// What a line's use says, in the table's use column.
fn use_of(line: &Line, now: u64) -> String {
    if line.item.source == Source::Hook {
        return match line.item.times {
            1 => "speaks once a session".into(),
            n => format!("speaks {n} times a session"),
        };
    }
    match &line.used {
        Some(u) => format!(
            "{} call{}, last {}",
            u.calls,
            if u.calls == 1 { "" } else { "s" },
            ago(&u.last, now)
        ),
        None if line.idle() => "never: likely waste".into(),
        None if line.switch == Switch::None => "-".into(),
        None => "never".into(),
    }
}

/// The longest a name column grows: a CLAUDE.md's whole path would push
/// the rest off the screen, and its end is the part that tells them apart.
const NAME_WIDTH: usize = 40;

fn clip(name: &str) -> String {
    let n = name.chars().count();
    if n <= NAME_WIDTH {
        return name.to_string();
    }
    let tail: String = name.chars().skip(n - (NAME_WIDTH - 3)).collect();
    format!("...{tail}")
}

/// The inventory as a table, largest first, and the per turn sum under it.
fn table(inv: &Inventory, now: u64) -> String {
    let Some(measured) = &inv.measured else {
        return "No session of this project has started fresh in its last 20 transcripts, \
                so there is nothing to measure yet.\n"
            .into();
    };
    let rows: Vec<[String; 6]> = inv
        .lines
        .iter()
        .map(|l| {
            [
                l.item.tokens().to_string(),
                if l.on { "on" } else { "off" }.into(),
                l.item.source.label().into(),
                clip(&l.item.name),
                use_of(l, now),
                how(&l.switch).into(),
            ]
        })
        .collect();
    let head = ["tokens", "", "source", "name", "use", "switched by"];
    let mut width = head.map(|h| h.chars().count());
    for r in &rows {
        for (w, c) in width.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in std::iter::once(head.map(String::from)).chain(rows) {
        let mut line = format!("{:>w$}", r[0], w = width[0]);
        for (c, w) in r.iter().zip(width).skip(1) {
            line.push_str(&format!("  {c:<w$}"));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let (before, after) = inv.per_turn();
    let idle = inv.lines.iter().filter(|l| l.idle()).count();
    out.push_str(&format!(
        "\nPer turn: {before} tokens as measured, {after} with the switches as they are now. \
         {idle} on and unused in {} days.\n",
        inventory::DAYS
    ));
    out.push_str(&format!(
        "Measured from {}, use from {} transcript{} of the last {} days.\n",
        measured.display(),
        inv.sessions,
        if inv.sessions == 1 { "" } else { "s" },
        inventory::DAYS
    ));
    if idle > 0 {
        out.push_str("Turn one off with `horadric context off <name>`.\n");
    }
    out
}

/// The line `query` means: its key or name, in any case, or the one line
/// whose key or name holds it.
fn find(lines: &[Line], query: &str) -> Result<usize, String> {
    let q = query.trim().to_lowercase();
    let exact = |l: &Line| l.item.key.to_lowercase() == q || l.item.name.to_lowercase() == q;
    if let Some(i) = lines.iter().position(exact) {
        return Ok(i);
    }
    let near: Vec<usize> = (0..lines.len())
        .filter(|&i| {
            let l = &lines[i];
            l.item.key.to_lowercase().contains(&q) || l.item.name.to_lowercase().contains(&q)
        })
        .collect();
    match near.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!(
            "nothing this project's sessions load is called `{query}`; `horadric context` lists them"
        )),
        many => Err(format!(
            "`{query}` could be {}; say which",
            many.iter()
                .map(|&i| lines[i].item.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn switch(main: &Path, query: &str, on: bool) -> Result<(), String> {
    let inv = inventory::gather(main);
    // A key works even for what no measured session loaded yet.
    let (key, name) = match find(&inv.lines, query) {
        Ok(i) => (
            inv.lines[i].item.key.clone(),
            inv.lines[i].item.name.clone(),
        ),
        Err(_) if query.contains(':') && !query.contains(' ') => {
            (query.to_string(), query.to_string())
        }
        Err(e) => return Err(e),
    };
    let switch = inventory::toggle(main, &key, on)?;
    let what = match &switch {
        Switch::Plugin(id) if on => format!("took enabledPlugins[\"{id}\"] out"),
        Switch::Plugin(id) => format!("wrote enabledPlugins[\"{id}\"]: false"),
        Switch::Server(s) if on => format!("took \"{s}\" out of deniedMcpServers"),
        Switch::Server(s) => format!("added \"{s}\" to deniedMcpServers"),
        Switch::Skill(s) if on => format!("took skillOverrides[\"{s}\"] out"),
        Switch::Skill(s) => format!("wrote skillOverrides[\"{s}\"]: \"off\""),
        Switch::None => String::new(),
    };
    println!(
        "{name} is {} for this project: {what} in {}.",
        if on { "on" } else { "off" },
        inventory::settings_path(main).display()
    );
    println!("New sessions pick it up; running ones keep what they started with.");
    Ok(())
}

/// `--all`: one line for each project Horadric knows, read side by side,
/// since each reads its own transcripts.
fn all() -> Result<(), String> {
    let dirs = horadric_ui::town::known_dirs();
    let found: Vec<(String, Inventory)> = std::thread::scope(|s| {
        let reads: Vec<_> = dirs
            .iter()
            .map(|d| s.spawn(move || (d.clone(), inventory::gather(Path::new(d)))))
            .collect();
        reads.into_iter().filter_map(|r| r.join().ok()).collect()
    });
    let rows: Vec<Overview> = found
        .iter()
        .map(|(dir, inv)| {
            let (before, after) = inv.per_turn();
            Overview {
                name: horadric_ui::project_name(&horadric_ui::folder_key(dir)),
                measured: inv.measured.is_some(),
                before,
                after,
                idle: inv.lines.iter().filter(|l| l.idle()).count(),
            }
        })
        .collect();
    print!("{}", overview(&rows));
    Ok(())
}

struct Overview {
    name: String,
    measured: bool,
    before: u64,
    after: u64,
    idle: usize,
}

/// The `--all` table, the heaviest project first.
fn overview(rows: &[Overview]) -> String {
    let mut rows: Vec<&Overview> = rows.iter().collect();
    rows.sort_by(|a, b| b.after.cmp(&a.after).then(a.name.cmp(&b.name)));
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .chain([7])
        .max()
        .unwrap_or(7);
    let mut out = format!(
        "{:<width$}  {:>9}  {:>9}  idle\n",
        "project", "per turn", "after"
    );
    for r in rows {
        if r.measured {
            out.push_str(&format!(
                "{:<width$}  {:>9}  {:>9}  {}\n",
                r.name, r.before, r.after, r.idle
            ));
        } else {
            out.push_str(&format!("{:<width$}  no session measured yet\n", r.name));
        }
    }
    out.push_str(
        "\nTokens every turn carries before any work, as measured and with the switches \
         as they are now; idle is what is on and went unused in 30 days.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::context::{Item, Use};

    fn line(key: &str, name: &str, source: Source, chars: u64, switch: Switch) -> Line {
        Line {
            item: Item {
                key: key.into(),
                name: name.into(),
                source,
                chars,
                times: 1,
            },
            switch,
            on: true,
            used: None,
        }
    }

    fn lines() -> Vec<Line> {
        let mut slack = line(
            "connector:Slack",
            "claude.ai Slack",
            Source::Account,
            8000,
            Switch::Server("claude.ai Slack".into()),
        );
        slack.used = Some(Use {
            calls: 12,
            last: "2026-10-26T10:00:00.512Z".into(),
        });
        let mut figma = line(
            "plugin:figma",
            "figma",
            Source::Plugin,
            4000,
            Switch::Plugin("figma@claude-plugins-official".into()),
        );
        figma.on = false;
        vec![
            slack,
            line(
                "connector:monday.com",
                "claude.ai monday.com",
                Source::Account,
                6000,
                Switch::Server("claude.ai monday.com".into()),
            ),
            figma,
            line(
                "claude-code",
                "Claude Code",
                Source::ClaudeCode,
                2000,
                Switch::None,
            ),
            line(
                "skill:wrangler",
                "wrangler",
                Source::Skill,
                400,
                Switch::Skill("wrangler".into()),
            ),
        ]
    }

    #[test]
    fn the_table_says_cost_use_and_switch_line_by_line() {
        let now = horadric_core::tasks::parse_utc("2026-10-29T10:00Z").unwrap();
        let inv = Inventory {
            lines: lines(),
            measured: Some(PathBuf::from("C:/t/abc.jsonl")),
            sessions: 7,
        };
        let t = table(&inv, now);
        let rows: Vec<&str> = t.lines().collect();
        assert!(rows[0].starts_with("tokens"));
        assert!(rows[1].contains("2000  on   claude.ai connector  claude.ai Slack"));
        assert!(rows[1].ends_with("12 calls, last 3d ago  deniedMcpServers"));
        assert!(rows[2].contains("never: likely waste"));
        assert!(rows[3].contains("off") && rows[3].ends_with("enabledPlugins"));
        assert!(rows[3].contains("never ") && !rows[3].contains("waste"));
        assert!(rows[4].contains(" - ") && rows[4].ends_with("not switchable here"));
        assert!(rows[5].ends_with("skillOverrides"));
        assert!(t.contains("Per turn: 5100 tokens as measured, 4100 with the switches"));
        assert!(t.contains("2 on and unused in 30 days"));
        assert!(t.contains("Measured from C:/t/abc.jsonl, use from 7 transcripts"));
        assert!(table(&Inventory::default(), now).starts_with("No session"));
    }

    #[test]
    fn a_name_finds_its_line_whole_or_by_a_unique_part() {
        let l = lines();
        assert_eq!(find(&l, "claude.ai Slack"), Ok(0));
        assert_eq!(find(&l, "CONNECTOR:SLACK"), Ok(0));
        assert_eq!(find(&l, "monday"), Ok(1));
        assert_eq!(find(&l, "wrang"), Ok(4));
        assert!(find(&l, "claude.ai").unwrap_err().contains("say which"));
        assert!(find(&l, "zoho").is_err());
    }

    #[test]
    fn the_main_tree_holds_the_shared_git_folder() {
        assert_eq!(
            main_of("C:/G/horadric.dev/.git\nC:/G/horadric.dev.quest\n"),
            Some(PathBuf::from("C:/G/horadric.dev"))
        );
        assert_eq!(
            main_of("C:/G/bare.git\nC:/G/wt\n"),
            Some(PathBuf::from("C:/G/wt"))
        );
        assert_eq!(main_of(""), None);
    }

    #[test]
    fn long_names_keep_their_end_and_times_read_as_ago() {
        let long = format!("C:/{}/CLAUDE.md", "x".repeat(60));
        let c = clip(&long);
        assert_eq!(c.chars().count(), NAME_WIDTH);
        assert!(c.starts_with("...") && c.ends_with("/CLAUDE.md"));
        let now = 10 * 86_400;
        assert_eq!(ago("1970-01-10T23:00:00.000Z", now), "1h ago");
        assert_eq!(ago("garbage", now), "at an unknown time");
    }

    #[test]
    fn every_project_gets_a_line_heaviest_first() {
        let rows = [
            Overview {
                name: "light".into(),
                measured: true,
                before: 9000,
                after: 3000,
                idle: 1,
            },
            Overview {
                name: "heavy".into(),
                measured: true,
                before: 52000,
                after: 50000,
                idle: 14,
            },
            Overview {
                name: "new".into(),
                measured: false,
                before: 0,
                after: 0,
                idle: 0,
            },
        ];
        let t = overview(&rows);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[1].starts_with("heavy") && lines[1].ends_with("50000  14"));
        assert!(lines[2].starts_with("light"));
        assert!(lines[3].ends_with("no session measured yet"));
    }
}
