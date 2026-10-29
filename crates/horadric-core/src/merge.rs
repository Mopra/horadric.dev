//! A finished quest merging itself into `main` in auto mode: rebased on
//! `main` in its own worktree, the project's checks run there, and `main`
//! fast forwarded to it, all while the quest's session is still alive. A
//! conflict or a red check goes back to that session, which knows what it
//! meant; only when it is gone does it become a fix-up quest for a fresh
//! one. Never a question for the human, since a worker can resolve a
//! conflict or make a check pass. This is the part that decides; the UI
//! runs git.

use serde_json::Value;

use crate::tasks::{
    after_line, after_note, append_with_notes, end_of, find, insert_note, insert_with_notes,
    one_line, parse, Mark, Task,
};

/// The project's checks, from `"checks": ["cargo test", ...]` in its
/// `config.json`. A single string is one check. None means a finished
/// quest merges without any.
pub fn checks(config: &str) -> Vec<String> {
    let v = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
    let list = match v.get("checks") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    list.into_iter()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from)
        .collect()
}

/// One step of a merge, in the order `plan` gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `git rebase <main>` in the quest's worktree.
    Rebase,
    /// A check, run with `cmd /c` in the quest's worktree.
    Check(String),
    /// `git merge --ff-only <branch>` in the main tree.
    FastForward,
}

/// The steps that merge a finished quest: rebase first, so the checks
/// run on what `main` will hold, and the fast forward last, so `main`
/// only ever moves to something that passed.
pub fn plan(checks: &[String]) -> Vec<Step> {
    let mut steps = vec![Step::Rebase];
    steps.extend(checks.iter().cloned().map(Step::Check));
    steps.push(Step::FastForward);
    steps
}

/// What a failed step means.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// The rebase stopped on a conflict, and was undone.
    Conflict,
    /// A check exited non zero.
    Red(String),
    /// `main` moved after the rebase, so the merge starts over.
    Moved,
    /// Git refused for a reason a worker cannot fix from its own branch,
    /// such as uncommitted changes. The human's click stays for it.
    Refused,
}

impl Failure {
    /// Whether a fix-up quest is the answer to it.
    pub fn fixable(&self) -> bool {
        matches!(self, Failure::Conflict | Failure::Red(_))
    }
}

/// What a step that failed with `output` means.
pub fn failed(step: &Step, output: &str) -> Failure {
    match step {
        Step::Rebase if output.contains("CONFLICT") || output.contains("could not apply") => {
            Failure::Conflict
        }
        Step::Rebase => Failure::Refused,
        Step::Check(c) => Failure::Red(c.clone()),
        Step::FastForward if output.contains("Not possible to fast-forward") => Failure::Moved,
        Step::FastForward => Failure::Refused,
    }
}

/// The files a stopped rebase's `output` says conflicted, in order, each
/// once, so the look back can tell which files keep conflicting.
pub fn conflicted(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("CONFLICT (") else {
            continue;
        };
        let file = match rest.split_once("Merge conflict in ") {
            Some((_, file)) => file,
            // These name the file first: "x.rs deleted in ...". Other
            // kinds start with a word, not a path, and are left out.
            None => match rest.split_once("): ") {
                Some((
                    "modify/delete" | "rename/delete" | "rename/rename" | "file location",
                    after,
                )) => after.split(' ').next().unwrap_or(""),
                _ => "",
            },
        };
        let file = file.trim();
        if !file.is_empty() && !out.iter().any(|f| f == file) {
            out.push(file.to_string());
        }
    }
    out
}

/// How many times a merge starts over when `main` moves under it.
pub const TRIES: usize = 3;

/// The title every fix-up quest starts with.
const FIX_UP: &str = "Fix the merge of ";

/// Whether the quest `title` is a fix-up, whose session holds its
/// project's landings while it works, so `main` does not move under the
/// merge it is mending.
pub fn is_fix_up(title: &str) -> bool {
    title.starts_with(FIX_UP)
}

/// What the other side of a failed landing did, so the session that
/// mends it knows what `main` meant and does not undo it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theirs {
    /// The files the rebase stopped on.
    pub files: Vec<String>,
    /// The subjects of the commits `main` has that the branch lacked,
    /// newest first: those that touched `files` when any did.
    pub subjects: Vec<String>,
}

/// How many commit subjects of the other side are named.
const SUBJECTS_MOST: usize = 10;

/// The lines `git log --format=%s` or `git diff --name-only` printed.
pub fn lines(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// `theirs` said on one line, for a session to read: what conflicted and
/// what `into` gained. Empty when there is nothing to say.
fn other_side(into: &str, theirs: &Theirs) -> String {
    let mut out = Vec::new();
    if !theirs.files.is_empty() {
        out.push(format!("It conflicts in {}.", quoted(&theirs.files)));
    }
    if !theirs.subjects.is_empty() {
        let shown = &theirs.subjects[..theirs.subjects.len().min(SUBJECTS_MOST)];
        let more = theirs.subjects.len() - shown.len();
        let mut said = shown
            .iter()
            .map(|s| format!("\"{}\"", one_line(s)))
            .collect::<Vec<_>>()
            .join(", ");
        if more > 0 {
            said.push_str(&format!(" and {more} more"));
        }
        out.push(format!(
            "`{into}` gained these commits meanwhile, so keep what they meant: {said}."
        ));
    }
    out.join(" ")
}

fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the live session of a finished quest is told when its landing
/// failed for `why`, on one line since it is typed into its terminal: what
/// went wrong, what the other side meant, and how to land again. None for
/// a failure it cannot fix from its branch.
pub fn hand_back(
    branch: &str,
    into: &str,
    why: &Failure,
    theirs: &Theirs,
    horadric: &str,
) -> Option<String> {
    let what = match why {
        Failure::Conflict => format!(
            "Your quest did not land: rebasing `{branch}` on `{into}` conflicts, and the \
             rebase was undone. Run `git rebase {into}` here, resolve each conflict so both \
             sides' work stays, and `git rebase --continue`."
        ),
        Failure::Red(c) => format!(
            "Your quest did not land: `{branch}` is rebased on `{into}` now, and the check \
             `{c}` fails on it. Run it here to see why and fix it."
        ),
        Failure::Moved | Failure::Refused => return None,
    };
    let side = other_side(into, theirs);
    let side = if side.is_empty() {
        String::new()
    } else {
        format!(" {side}")
    };
    Some(format!(
        "{what}{side} Then make every check in `.horadric/config.json` pass, commit, and run \
         `{horadric} quest done \"<one short line on what you achieved>\"` again. The \
         project's other landings wait for yours."
    ))
}

/// How many lines of a failure's output go into the fix-up quest's notes.
const OUTPUT_LINES: usize = 40;
/// How long a line of that output may be.
const LINE_CHARS: usize = 200;

/// A fix-up quest: its title and its notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixUp {
    pub title: String,
    pub notes: String,
}

/// The quest that makes `branch`, the finished quest `title`'s, merge
/// into `into` after `why` stopped it, with what the other side did and
/// the end of what failed in its notes.
pub fn fix_up(
    title: &str,
    branch: &str,
    into: &str,
    why: &Failure,
    output: &str,
    theirs: &Theirs,
) -> FixUp {
    let what = match why {
        Failure::Conflict => format!("rebasing it on `{into}` conflicts"),
        Failure::Red(c) => format!("the check `{c}` fails after rebasing it on `{into}`"),
        Failure::Moved => format!("`{into}` kept moving under it"),
        Failure::Refused => "git refused".to_string(),
    };
    let mut notes = vec![
        format!(
            "\"{}\" is done on the branch `{branch}` but did not merge into `{into}`: {what}.",
            one_line(title)
        ),
        // Not a merge: the auto merge rebases, which drops a merge commit
        // and meets the same conflict again.
        format!(
            "Take its commits onto your branch with `git cherry-pick {into}..{branch}`, \
             resolve each conflict and `git cherry-pick --continue`, make every check in \
             `.horadric/config.json` pass, and commit. Then delete it with `git branch -D \
             {branch}`. Your branch merges like any finished quest and carries its work."
        ),
        "The quests after the finished one wait for this one too.".to_string(),
    ];
    let side = other_side(into, theirs);
    if !side.is_empty() {
        notes.push(side);
    }
    let tail = tail(output);
    if !tail.is_empty() {
        notes.push("What failed said:".to_string());
        notes.push("```".to_string());
        notes.extend(tail);
        notes.push("```".to_string());
    }
    FixUp {
        title: format!("{FIX_UP}{branch}"),
        notes: notes.join("\n"),
    }
}

/// The last lines of `output` that say anything, each cut to a length a
/// notes line can carry.
fn tail(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output
        .lines()
        // A progress line rewritten with carriage returns ends as what it
        // said last.
        .map(|l| l.rsplit('\r').next().unwrap_or(l).trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(OUTPUT_LINES);
    lines[from..]
        .iter()
        .map(|l| {
            if after_line(l.trim_start()).is_some() {
                // Read as the fix-up's own `After:` line, it would make it
                // wait on whatever the output named.
                format!("> {l}")
            } else if l.chars().count() > LINE_CHARS {
                let cut: String = l.chars().take(LINE_CHARS).collect();
                format!("{cut}...")
            } else {
                l.to_string()
            }
        })
        .collect()
}

/// The list as the runner sees it while the done quests titled in
/// `landing` merge: still in hand, so a quest after one of them waits
/// until its work is on `main` or its fix-up is filed. The runner takes
/// their holders as live, since the merge is what works on them now.
pub fn while_landing(tasks: &[Task], landing: &[String]) -> Vec<Task> {
    tasks
        .iter()
        .cloned()
        .map(|mut t| {
            if t.mark == Mark::Done && landing.contains(&t.title) {
                t.mark = Mark::Working;
            }
            t
        })
        .collect()
}

/// The list with `fix` added right below the quest `finished` and its
/// notes, and an `After:` line for it on every quest that waits for
/// `finished`, so they wait for the fix-up too and the work they build on
/// is on `main` before they start. At the end when `finished` is not in
/// the list any more. None when the fix-up is there already and not done.
pub fn add_fix_up(text: &str, finished: &str, fix: &FixUp) -> Option<String> {
    let list = parse(text);
    if list
        .iter()
        .any(|t| t.title == fix.title && t.mark != Mark::Done)
    {
        return None;
    }
    let Ok(at) = find(&list, finished) else {
        return Some(append_with_notes(text, &fix.title, &fix.notes));
    };
    let waiting = list
        .iter()
        .filter(|t| t.after().iter().any(|n| find(&list, n) == Ok(at)))
        .map(|t| t.line);
    // From the bottom up, so each line number still points where it did.
    let mut edits: Vec<(usize, bool)> = waiting.map(|l| (l + 1, false)).collect();
    edits.push((end_of(text, list[at].line), true));
    edits.sort_by(|a, b| b.cmp(a));
    let mut out = text.to_string();
    for (line, is_fix) in edits {
        out = if is_fix {
            insert_with_notes(&out, line, &fix.title, &fix.notes)
        } else {
            insert_note(&out, line, &after_note(&fix.title))
        };
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_a_conflict_names() {
        let output = "Auto-merging apps/desktop/src/preload/index.ts
CONFLICT (content): Merge conflict in apps/desktop/src/preload/index.ts
Auto-merging docs/SPEC.md
CONFLICT (content): Merge conflict in docs/SPEC.md
CONFLICT (add/add): Merge conflict in docs/SPEC.md
CONFLICT (modify/delete): src/old.rs deleted in HEAD and modified in abc123.
CONFLICT (file/directory): directory in the way of src/x from HEAD; moving it to src/x~HEAD.
error: could not apply e36bb84... Notify natively
";
        assert_eq!(
            conflicted(output),
            [
                "apps/desktop/src/preload/index.ts",
                "docs/SPEC.md",
                "src/old.rs"
            ]
        );
        assert!(conflicted("fatal: not a git repository").is_empty());
    }

    #[test]
    fn checks_come_from_the_config_as_a_list_or_one_string() {
        assert_eq!(
            checks(r#"{"checks": ["cargo fmt --all -- --check", " ", "cargo test"]}"#),
            ["cargo fmt --all -- --check", "cargo test"]
        );
        assert_eq!(checks(r#"{"checks": "npm test"}"#), ["npm test"]);
        assert!(checks(r#"{"tasks": {"mode": "auto"}}"#).is_empty());
        assert!(checks("not json").is_empty());
    }

    #[test]
    fn the_plan_rebases_then_checks_then_fast_forwards() {
        let c = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            plan(&c),
            [
                Step::Rebase,
                Step::Check("a".into()),
                Step::Check("b".into()),
                Step::FastForward
            ]
        );
        assert_eq!(plan(&[]), [Step::Rebase, Step::FastForward]);
    }

    #[test]
    fn a_failure_is_read_from_its_step_and_what_git_said() {
        let conflict = "CONFLICT (content): Merge conflict in a.txt\nerror: could not apply 1a2b";
        assert_eq!(failed(&Step::Rebase, conflict), Failure::Conflict);
        assert_eq!(
            failed(
                &Step::Rebase,
                "error: cannot rebase: You have unstaged changes."
            ),
            Failure::Refused
        );
        assert_eq!(
            failed(&Step::Check("cargo test".into()), "test failed"),
            Failure::Red("cargo test".into())
        );
        assert_eq!(
            failed(
                &Step::FastForward,
                "fatal: Not possible to fast-forward, aborting."
            ),
            Failure::Moved
        );
        assert_eq!(
            failed(
                &Step::FastForward,
                "error: Your local changes would be overwritten"
            ),
            Failure::Refused
        );
        assert!(Failure::Conflict.fixable() && Failure::Red("x".into()).fixable());
        assert!(!Failure::Moved.fixable() && !Failure::Refused.fixable());
    }

    #[test]
    fn a_fix_up_names_the_branch_and_carries_the_failing_output() {
        let f = fix_up(
            "Add the\nlogin",
            "add-the-login",
            "main",
            &Failure::Red("cargo test".into()),
            "running 2 tests\r\n\ntest a ... FAILED\r\n",
            &Theirs::default(),
        );
        assert_eq!(f.title, "Fix the merge of add-the-login");
        let lines: Vec<&str> = f.notes.lines().collect();
        assert_eq!(
            lines[0],
            "\"Add the login\" is done on the branch `add-the-login` but did not merge into \
             `main`: the check `cargo test` fails after rebasing it on `main`."
        );
        assert!(lines[1].starts_with(
            "Take its commits onto your branch with `git cherry-pick main..add-the-login`"
        ));
        assert_eq!(
            lines[2],
            "The quests after the finished one wait for this one too."
        );
        assert_eq!(
            &lines[3..],
            [
                "What failed said:",
                "```",
                "running 2 tests",
                "test a ... FAILED",
                "```"
            ]
        );
    }

    #[test]
    fn a_conflict_fix_up_says_so_and_needs_no_output() {
        let f = fix_up(
            "T",
            "t",
            "trunk",
            &Failure::Conflict,
            "",
            &Theirs::default(),
        );
        assert!(f
            .notes
            .lines()
            .next()
            .unwrap()
            .ends_with("rebasing it on `trunk` conflicts."));
        assert!(!f.notes.contains("```"));
    }

    #[test]
    fn a_fix_up_says_what_the_other_side_did() {
        let theirs = Theirs {
            files: vec!["a.rs".into()],
            subjects: vec!["Rename the thing".into()],
        };
        let f = fix_up("T", "t", "main", &Failure::Conflict, "", &theirs);
        assert_eq!(
            f.notes.lines().nth(3).unwrap(),
            "It conflicts in `a.rs`. `main` gained these commits meanwhile, so keep what they \
             meant: \"Rename the thing\"."
        );
        assert!(is_fix_up(&f.title) && !is_fix_up("Fix the login"));
    }

    #[test]
    fn a_live_session_is_handed_its_conflict_on_one_line() {
        let theirs = Theirs {
            files: vec!["a.rs".into(), "b.rs".into()],
            subjects: (0..12).map(|i| format!("c{i}")).collect(),
        };
        let said = hand_back("q", "main", &Failure::Conflict, &theirs, "h").unwrap();
        assert!(!said.contains('\n'), "{said}");
        assert!(said.starts_with("Your quest did not land: rebasing `q` on `main` conflicts"));
        assert!(said.contains("Run `git rebase main` here"));
        assert!(said.contains("It conflicts in `a.rs`, `b.rs`."));
        assert!(said.contains("\"c9\" and 2 more."));
        assert!(said.contains("run `h quest done"));
        let red = hand_back(
            "q",
            "main",
            &Failure::Red("cargo test".into()),
            &Theirs::default(),
            "h",
        )
        .unwrap();
        assert!(red.contains("the check `cargo test` fails on it."));
        assert!(!red.contains("gained"));
        assert_eq!(hand_back("q", "main", &Failure::Moved, &theirs, "h"), None);
        assert_eq!(
            hand_back("q", "main", &Failure::Refused, &theirs, "h"),
            None
        );
    }

    #[test]
    fn git_lines_skip_blanks() {
        assert_eq!(lines("a.rs\n\n b.rs \n"), ["a.rs", "b.rs"]);
    }

    #[test]
    fn only_the_end_of_long_output_goes_into_the_notes() {
        let out: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let t = tail(&out);
        assert_eq!(t.len(), OUTPUT_LINES);
        assert_eq!(t[0], "line 60");
        assert_eq!(t.last().unwrap(), "line 99");
        let long = "x".repeat(500);
        assert_eq!(tail(&long)[0].chars().count(), LINE_CHARS + 3);
        assert_eq!(tail("50%\r100%\n"), ["100%"]);
    }

    #[test]
    fn output_that_reads_as_an_after_line_is_quoted() {
        assert_eq!(tail("After: something\n"), ["> After: something"]);
    }

    fn fix() -> FixUp {
        FixUp {
            title: "Fix the merge of a".into(),
            notes: "Why.".into(),
        }
    }

    #[test]
    fn a_fix_up_goes_below_the_finished_quest_and_its_waiters_wait_for_it() {
        let text = "\
# Quests
- [x] A @a
  A's note.

- [ ] B
  After: A
- [ ] C
- [ ] D
  First note.
  after: a
";
        let out = add_fix_up(text, "A", &fix()).unwrap();
        assert_eq!(
            out,
            "\
# Quests
- [x] A @a
  A's note.
- [ ] Fix the merge of a
  Why.

- [ ] B
  After: Fix the merge of a
  After: A
- [ ] C
- [ ] D
  After: Fix the merge of a
  First note.
  after: a
"
        );
        let list = parse(&out);
        let ready = crate::tasks::readiness(&list);
        assert_eq!(list[1].title, "Fix the merge of a");
        assert_eq!(ready[1], crate::tasks::Ready::Yes);
        assert_eq!(
            ready[2],
            crate::tasks::Ready::After("Fix the merge of a".into())
        );
        assert_eq!(ready[3], crate::tasks::Ready::Yes);
    }

    #[test]
    fn a_fix_up_is_added_once_and_at_the_end_when_its_quest_is_gone() {
        let text = "- [x] A @a\n- [ ] B\n";
        let once = add_fix_up(text, "A", &fix()).unwrap();
        assert_eq!(add_fix_up(&once, "A", &fix()), None);
        assert_eq!(
            add_fix_up(text, "Gone", &fix()).unwrap(),
            "- [x] A @a\n- [ ] B\n- [ ] Fix the merge of a\n  Why.\n"
        );
        // The last quest, with no ending on its last line.
        assert_eq!(
            add_fix_up("- [x] A @a\n  n", "A", &fix()).unwrap(),
            "- [x] A @a\n  n\n- [ ] Fix the merge of a\n  Why.\n"
        );
    }

    #[test]
    fn a_quest_still_merging_holds_back_the_quests_after_it() {
        use crate::tasks::{next, Holder, Mode, Next};
        let list = parse(
            "- [x] A @a
- [ ] B
  After: A
- [ ] C
",
        );
        let pick = |l: &[Task]| next(l, Mode::Auto, 2, false, |_| Holder::Live, |_| true);
        assert_eq!(pick(&list), Next::Start(1));
        let landing = while_landing(&list, &["A".to_string()]);
        assert_eq!(landing[0].mark, Mark::Working);
        assert_eq!(pick(&landing), Next::Start(2));
        assert_eq!(while_landing(&list, &[]), list);
    }
}
