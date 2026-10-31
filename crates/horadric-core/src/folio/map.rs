//! The work of a turn as a map: its steps on a timeline, subagents' steps
//! on rows of their own as they ran beside the rest, and the files it
//! read and changed. Built from the blocks on demand, when the human
//! opens a turn's map, so nothing is kept or repainted while it is shut.

use super::{Block, Step, StepState};

/// What sort of work a step was, for its colour on the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Read,
    Edit,
    Shell,
    Search,
    Agent,
    Web,
    Other,
}

impl StepKind {
    pub fn of(tool: &str) -> StepKind {
        match tool {
            "Read" | "NotebookRead" => StepKind::Read,
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => StepKind::Edit,
            "Bash" | "PowerShell" | "BashOutput" | "KillShell" => StepKind::Shell,
            "Grep" | "Glob" | "LS" | "ToolSearch" => StepKind::Search,
            "Agent" | "Task" => StepKind::Agent,
            "WebFetch" | "WebSearch" => StepKind::Web,
            t if t.starts_with("mcp__") && t.contains("browser") => StepKind::Web,
            _ => StepKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapStep {
    pub id: String,
    pub tool: String,
    pub summary: String,
    pub kind: StepKind,
    pub state: StepState,
    pub began: Option<u64>,
    pub ended: Option<u64>,
    /// 0 for Claude's own, 1 for a subagent's, and so on.
    pub depth: usize,
    /// The block of the run it is in, and the path of step ids down to
    /// it, for a click to open it there.
    pub block: usize,
    pub path: Vec<String>,
}

/// A file the turn touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTouch {
    /// As the tool was given it.
    pub path: String,
    pub reads: u32,
    pub edits: u32,
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnMap {
    pub steps: Vec<MapStep>,
    /// In the order first touched.
    pub files: Vec<FileTouch>,
    /// The turn's start and end in Unix milliseconds, where known.
    pub began: Option<u64>,
    pub ended: Option<u64>,
}

/// One bar on the timeline: where it starts and ends, as shares of the
/// turn's width, and its row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub from: f32,
    pub to: f32,
    pub row: usize,
}

/// The map of the turn the footer at `footer` ends, or of the turn that
/// runs when `footer` is past the end.
pub fn turn_map(blocks: &[Block], footer: usize) -> TurnMap {
    let footer = footer.min(blocks.len());
    let start = (blocks[..footer].iter())
        .rposition(|b| matches!(b, Block::Prompt(_)))
        .map_or(0, |i| i + 1);
    let mut map = TurnMap::default();
    for (i, b) in blocks.iter().enumerate().take(footer).skip(start) {
        if let Block::Tools(steps) = b {
            walk(steps, i, 0, &[], &mut map);
        }
    }
    let first = map.steps.iter().filter_map(|s| s.began).min();
    let last = (map.steps.iter())
        .filter_map(|s| s.ended.or(s.began))
        .max();
    match blocks.get(footer) {
        Some(Block::Footer {
            at: Some(at),
            duration_ms,
            ..
        }) => {
            map.ended = Some(*at);
            let start = at.saturating_sub(*duration_ms);
            map.began = Some(first.map_or(start, |f| f.min(start)));
        }
        _ => {
            map.began = first;
            map.ended = last;
        }
    }
    map
}

fn walk(steps: &[Step], block: usize, depth: usize, path: &[String], map: &mut TurnMap) {
    for s in steps {
        let mut here = path.to_vec();
        here.push(s.id.clone());
        map.steps.push(MapStep {
            id: s.id.clone(),
            tool: s.tool.clone(),
            summary: s.summary.clone(),
            kind: StepKind::of(&s.tool),
            state: s.state,
            began: s.began,
            ended: s.ended,
            depth,
            block,
            path: here.clone(),
        });
        touch(s, map);
        if let Some(lane) = &s.lane {
            walk(&lane.steps, block, depth + 1, &here, map);
        }
    }
}

fn touch(s: &Step, map: &mut TurnMap) {
    let path = match &s.edit {
        Some(e) => e.path.as_str(),
        None if s.tool == "Read" => s.input["file_path"].as_str().unwrap_or_default(),
        None => "",
    };
    if path.is_empty() || s.state == StepState::Failed {
        return;
    }
    let at = match map.files.iter().position(|f| same_file(&f.path, path)) {
        Some(at) => at,
        None => {
            map.files.push(FileTouch {
                path: path.to_string(),
                ..FileTouch::default()
            });
            map.files.len() - 1
        }
    };
    let f = &mut map.files[at];
    match &s.edit {
        Some(e) => {
            f.edits += 1;
            f.added += e.added;
            f.removed += e.removed;
        }
        None => f.reads += 1,
    }
}

/// Paths as tools are given them differ in slashes and case on Windows.
fn same_file(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// Where each step's bar goes. With the times of every step known, bars
/// sit where they ran, and steps that overlapped (subagents at work at
/// once) take rows of their own. Without, the steps line up one after
/// another in their order, a subagent's a row under its own.
pub fn timeline(map: &TurnMap) -> Vec<Bar> {
    let n = map.steps.len();
    if n == 0 {
        return Vec::new();
    }
    let timed = map.steps.iter().all(|s| s.began.is_some());
    let (Some(t0), Some(t1), true) = (map.began, map.ended, timed) else {
        let w = 1.0 / n as f32;
        return (map.steps.iter().enumerate())
            .map(|(i, s)| Bar {
                from: i as f32 * w,
                to: (i + 1) as f32 * w,
                row: s.depth,
            })
            .collect();
    };
    let span = (t1.saturating_sub(t0)).max(1) as f32;
    let at = |t: u64| (t.saturating_sub(t0) as f32 / span).clamp(0.0, 1.0);
    // Free from where on each row is, as a share.
    let mut rows: Vec<f32> = Vec::new();
    // A sliver so a step of no length still shows and two at the same
    // moment do not share a row.
    const MIN: f32 = 0.004;
    map.steps
        .iter()
        .map(|s| {
            let from = at(s.began.unwrap_or(t0));
            let to = at(s.ended.unwrap_or(t1)).max(from + MIN);
            let row = match rows.iter().position(|free| *free <= from) {
                Some(r) => r,
                None => {
                    rows.push(0.0);
                    rows.len() - 1
                }
            };
            rows[row] = to;
            Bar { from, to, row }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folio::{Ending, Folio, TurnUsage};
    use serde_json::json;

    fn step(f: &mut Folio, id: &str, tool: &str, input: serde_json::Value, at: u64) {
        f.clock = Some(at);
        f.add_step(id, tool, &input);
    }

    fn result(f: &mut Folio, id: &str, at: u64) {
        f.clock = Some(at);
        let v = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": "ok"}
        ]}});
        f.feed(&v.to_string());
    }

    fn footer(f: &mut Folio, at: u64, duration_ms: u64) {
        f.blocks.push(Block::Footer {
            duration_ms,
            ending: Ending::Done,
            usage: TurnUsage::default(),
            at: Some(at),
        });
    }

    #[test]
    fn a_turn_s_steps_and_files_make_its_map() {
        let mut f = Folio::new();
        f.push_prompt("go");
        step(&mut f, "a", "Read", json!({"file_path": "C:\\p\\src\\a.rs"}), 1_000);
        result(&mut f, "a", 1_500);
        let edit = json!({"file_path": "C:/p/src/a.rs", "old_string": "x", "new_string": "y\nz"});
        step(&mut f, "b", "Edit", edit, 1_500);
        result(&mut f, "b", 2_000);
        footer(&mut f, 3_000, 2_500);
        let map = turn_map(&f.blocks, f.blocks.len() - 1);
        assert_eq!(map.steps.len(), 2);
        assert_eq!(map.steps[1].kind, StepKind::Edit);
        assert_eq!((map.began, map.ended), (Some(500), Some(3_000)));
        assert_eq!(map.files.len(), 1, "one file, however its path is written");
        let file = &map.files[0];
        assert_eq!((file.reads, file.edits), (1, 1));
        assert!(file.added > 0);
        let bars = timeline(&map);
        assert!((bars[0].from - 0.2).abs() < 1e-4);
        assert!((bars[1].to - 0.6).abs() < 1e-4);
        assert_eq!((bars[0].row, bars[1].row), (0, 0), "one after the other");
    }

    #[test]
    fn steps_at_once_take_rows_and_without_times_line_up() {
        let mut f = Folio::new();
        f.push_prompt("go");
        step(&mut f, "a", "Bash", json!({"command": "sleep 1"}), 0);
        step(&mut f, "b", "Bash", json!({"command": "sleep 1"}), 10);
        result(&mut f, "a", 900);
        result(&mut f, "b", 950);
        footer(&mut f, 1_000, 1_000);
        let map = turn_map(&f.blocks, 3);
        let rows: Vec<usize> = timeline(&map).iter().map(|b| b.row).collect();
        assert_eq!(rows, vec![0, 1]);

        let mut g = Folio::new();
        g.push_prompt("go");
        g.add_step("a", "Read", &json!({"file_path": "x"}));
        g.add_step("b", "Read", &json!({"file_path": "y"}));
        let map = turn_map(&g.blocks, g.blocks.len());
        let bars = timeline(&map);
        assert_eq!((bars[0].from, bars[1].from), (0.0, 0.5));
    }

    #[test]
    fn only_the_footer_s_own_turn_is_mapped() {
        let mut f = Folio::new();
        f.push_prompt("one");
        f.add_step("a", "Read", &json!({"file_path": "x"}));
        footer(&mut f, 10, 5);
        f.push_prompt("two");
        f.add_step("b", "Glob", &json!({"pattern": "*"}));
        footer(&mut f, 20, 5);
        let map = turn_map(&f.blocks, f.blocks.len() - 1);
        let ids: Vec<&str> = map.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
        assert!(map.files.is_empty());
    }
}
