//! Adding and removing a session's own git worktree. What a project wants
//! and what a worktree is told live in `horadric_core::worktree`; this is
//! the part that runs `git`.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use horadric_core::diff::{self, Diff};
use horadric_core::experience;
use horadric_core::journal::{self, Commit};
use horadric_core::merge;
use horadric_core::worktree::{self, Place, Ports, Worktree};
use horadric_hooks::tasks as file;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often ending a session tries to remove its worktree. The agent was
/// just killed, and its files can stay locked for a moment after.
const REMOVE_TRIES: u32 = 3;

/// A worktree just added for a new session.
pub struct Fresh {
    /// Where the session starts: the same folder in the new worktree as it
    /// was asked to start in the main one.
    pub cwd: PathBuf,
    pub worktree: Worktree,
    /// The project's setup commands, for its first start only.
    pub setup: Vec<String>,
}

/// Where `dir` is in its repository's main working tree, None when it is
/// in a linked worktree or in no repository.
///
/// Menus ask this as they open, so an answer is remembered: a folder's
/// place in its repository hardly ever changes. A folder in no repository
/// may get one, so that answer is asked again after [`NO_PLACE_FOR`].
pub fn main_tree(dir: &Path) -> Option<Place> {
    type Known = HashMap<PathBuf, (Option<Place>, Instant)>;
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let now = Instant::now();
    let kept = known.lock().ok().and_then(|k| {
        k.get(dir)
            .filter(|(place, at)| place.is_some() || now.duration_since(*at) < NO_PLACE_FOR)
            .map(|(place, _)| place.clone())
    });
    if let Some(place) = kept {
        return place;
    }
    let place = ask_main_tree(dir);
    if let Ok(mut k) = known.lock() {
        k.insert(dir.to_path_buf(), (place.clone(), now));
    }
    place
}

/// How long a folder is taken to be in no repository before git is asked
/// again.
const NO_PLACE_FOR: Duration = Duration::from_secs(30);

fn ask_main_tree(dir: &Path) -> Option<Place> {
    let out = git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--show-prefix",
        ],
    )
    .ok()?;
    worktree::main_tree(&out)
}

/// Adds a worktree for a new session named `name` in `dir`, on a new
/// branch from the project's main branch (see [`main_branch`]), with a range of ports
/// that `taken` does not use. None when the project does not want one and
/// it was not `asked` for; an error when git refused, and the session then
/// shares the main tree.
pub fn add(dir: &Path, name: &str, taken: &[Ports], asked: bool) -> Result<Option<Fresh>, String> {
    let settings = file::worktrees(dir);
    if !settings.enabled && !asked {
        return Ok(None);
    }
    let Some(place) = main_tree(dir) else {
        return Ok(None);
    };
    let top = Path::new(&place.top);
    let branch = worktree::branch(name, |b| {
        Path::new(&worktree::folder(&place.top, b)).exists()
            || git(
                top,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_ok()
    });
    let path = worktree::folder(&place.top, &branch);
    let mut args = vec!["worktree", "add", "-b", &branch, &path];
    let start = main_branch(top);
    if let Some(start) = &start {
        args.push(start);
    }
    git(top, &args)?;
    if horadric_hooks::dev() {
        mark_dev_made(Path::new(&path));
    }
    let cwd = if place.prefix.is_empty() {
        PathBuf::from(&path)
    } else {
        Path::new(&path).join(&place.prefix)
    };
    Ok(Some(Fresh {
        cwd,
        worktree: Worktree {
            path,
            main: place.top.replace('\\', "/"),
            branch,
            ports: worktree::free_ports(settings.ports, taken),
        },
        setup: settings.setup,
    }))
}

/// The worktree's own git folder, where git keeps what is per worktree.
fn admin_dir(tree: &Path) -> Option<PathBuf> {
    git(tree, &["rev-parse", "--path-format=absolute", "--git-dir"])
        .ok()
        .map(|d| PathBuf::from(d.trim()))
}

fn mark_dev_made(tree: &Path) {
    let written = admin_dir(tree).map(|d| std::fs::write(d.join(worktree::DEV_MARK), ""));
    if !matches!(written, Some(Ok(()))) {
        eprintln!(
            "horadric: could not mark {} as a dev worktree",
            tree.display()
        );
    }
}

fn dev_made(tree: &Path) -> bool {
    admin_dir(tree).is_some_and(|d| d.join(worktree::DEV_MARK).is_file())
}

/// Removes a session's worktree once the session has ended, and its branch
/// once nothing is on it that the main tree lacks. Git refuses both while
/// there is work to lose, changed files in the tree or commits on the
/// branch, and then both stay for the human. In the background, since the
/// ended agent may hold its files for a moment.
pub fn remove(w: Worktree) {
    remove_then(w, |_| ());
}

/// Removes as `remove` does, and calls `kept` with the worktree when its
/// tree went but its branch stayed, since it has commits the main tree
/// lacks: a branch worth offering to merge.
pub fn remove_then(w: Worktree, kept: impl FnOnce(Worktree) + Send + 'static) {
    remove_with(w, false, kept);
}

/// Removes a worktree and its branch with whatever is in them. Only for a
/// tomb the human did not pick, whose work is thrown away on purpose.
pub fn discard(w: Worktree) {
    remove_with(w, true, |_| ());
}

fn remove_with(w: Worktree, force: bool, kept: impl FnOnce(Worktree) + Send + 'static) {
    std::thread::spawn(move || {
        let main = Path::new(&w.main);
        let mut result = Ok(String::new());
        let remove: &[&str] = if force {
            &["worktree", "remove", "--force", &w.path]
        } else {
            &["worktree", "remove", &w.path]
        };
        for attempt in 0..REMOVE_TRIES {
            std::thread::sleep(Duration::from_secs(1 + attempt as u64));
            result = git(main, remove);
            if result.is_ok() || !Path::new(&w.path).exists() {
                break;
            }
        }
        if let Err(e) = result {
            eprintln!("horadric: kept the worktree {}: {e}", w.path);
            return;
        }
        let delete = if force { "-D" } else { "-d" };
        if let Err(e) = git(main, &["branch", delete, &w.branch]) {
            eprintln!("horadric: kept the branch {}: {e}", w.branch);
            kept(w);
        }
    });
}

/// The branches with commits that what the main tree has checked out
/// lacks.
pub fn unmerged(main: &Path) -> Vec<String> {
    git(
        main,
        &["branch", "--no-merged", "HEAD", "--format=%(refname:short)"],
    )
    .map(|out| out.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

/// What `branch` changed since it left `into`, as `git diff` says it. Empty
/// when git can not say.
pub fn diff(main: &Path, into: &str, branch: &str) -> String {
    git(main, &["diff", &format!("{into}...{branch}")]).unwrap_or_default()
}

/// The files `branch` changed since it left `into`. Empty when git can not
/// say.
pub fn changed_since(main: &Path, into: &str, branch: &str) -> Vec<String> {
    git(
        main,
        &["diff", "--name-only", &format!("{into}...{branch}")],
    )
    .map(|out| out.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

/// Whether merging `branch` into `into` would conflict, tried in git's
/// object store without touching any tree. True when git can not say (an
/// old git has no `--write-tree`), so a session is told rather than left.
pub fn would_conflict(main: &Path, into: &str, branch: &str) -> bool {
    git(main, &["merge-tree", "--write-tree", into, branch]).is_err()
}

/// What the main tree has checked out, for saying where a merge goes.
pub fn checked_out(main: &Path) -> Option<String> {
    git(main, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD")
}

/// The branch a quest's worktree starts from: the one [`land`] merges it
/// back into, what the main tree `main` has checked out. While the main
/// tree is on no branch, as in the middle of a `pull --rebase`, its HEAD
/// is some other commit, so then the remote's default branch, or `main`
/// or `master`. None when there is none of those either.
pub fn main_branch(main: &Path) -> Option<String> {
    let remote = git(
        main,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok();
    let has = |b: &str| {
        git(
            main,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .is_ok()
    };
    pick_main_branch(checked_out(main).as_deref(), remote.as_deref(), has)
}

/// [`main_branch`] from what git said: the branch `checked_out`, the
/// remote's default as `origin/main`, and which local branches there are.
fn pick_main_branch(
    checked_out: Option<&str>,
    remote: Option<&str>,
    has: impl Fn(&str) -> bool,
) -> Option<String> {
    if let Some(b) = checked_out {
        return Some(b.to_string());
    }
    let remote = remote
        .map(str::trim)
        .and_then(|r| r.strip_prefix("origin/"));
    remote
        .into_iter()
        .chain(["main", "master"])
        .find(|b| has(b))
        .map(str::to_string)
}

/// The file `rel` as the last commit in `dir` has it, None where it has
/// none.
pub fn committed(dir: &Path, rel: &str) -> Option<String> {
    git(dir, &["show", &format!("HEAD:{rel}")]).ok()
}

/// Commits the file `rel` in the main tree `main` by itself, leaving
/// whatever else is staged there to the sessions that staged it. True when
/// there was something to commit. Nothing on a detached head, or where git
/// ignores the file.
pub fn commit_file(main: &Path, rel: &str, message: &str) -> Result<bool, String> {
    if git(main, &["status", "--porcelain", "--", rel])?
        .trim()
        .is_empty()
    {
        return Ok(false);
    }
    if checked_out(main).is_none() {
        return Err("the main tree is not on a branch".to_string());
    }
    git(main, &["add", "--", rel])?;
    git(main, &["commit", "--only", "-m", message, "--", rel])?;
    Ok(true)
}

/// Merges `branch` into what the main tree has checked out, always with a
/// merge commit so the item stays one piece in the history, and then
/// deletes the branch. A merge that stops on a conflict is undone, so the
/// main tree is never left half merged; the branch stays for the human.
pub fn merge(main: &Path, branch: &str) -> Result<(), String> {
    if let Err(e) = git(main, &["merge", "--no-ff", "--no-edit", branch]) {
        let _ = git(main, &["merge", "--abort"]);
        return Err(e);
    }
    if let Err(e) = git(main, &["branch", "-d", branch]) {
        eprintln!("horadric: merged but kept the branch {branch}: {e}");
    }
    Ok(())
}

/// Whether what `dir` has checked out is in what its main tree has
/// checked out, so the work done there landed. None when git cannot say:
/// no repository, a bare one, or the main tree on no branch. The main tree
/// is where the shared `.git` is, which a linked worktree also knows.
pub fn landed(dir: &Path) -> Option<bool> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let main = Path::new(common.trim()).parent()?;
    let branch = checked_out(main)?;
    let head = format!("refs/heads/{branch}");
    Some(git(dir, &["merge-base", "--is-ancestor", "HEAD", &head]).is_ok())
}

/// The commit checked out in `dir`.
pub fn head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|h| h.trim().to_string())
}

/// The commits made on the branch checked out in `dir` since `since`, in
/// Unix seconds, newest first. Only its own line: a merge of main into it
/// would otherwise bring every commit main had meanwhile.
pub fn commits_since(dir: &Path, since: u64) -> Vec<Commit> {
    let since = format!("--since=@{since}");
    let format = format!("--format={}", journal::LOG_FORMAT);
    git(
        dir,
        &[
            "log",
            "--first-parent",
            "--no-merges",
            &since,
            &format,
            "HEAD",
        ],
    )
    .map(|log| journal::commits(&log))
    .unwrap_or_default()
}

/// The hashes of the commits on what the main tree of `dir` has checked
/// out whose author is the one git would name for a commit made there now.
/// Merges are left out: they add nothing a commit of their own did not.
pub fn mine(dir: &Path) -> Vec<String> {
    let Ok(email) = git(dir, &["config", "user.email"]) else {
        return Vec::new();
    };
    let format = format!("--format={}", experience::LOG_FORMAT);
    git(dir, &["log", "--no-merges", &format, "HEAD"])
        .map(|log| {
            experience::mine(&log, email.trim())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Removes the linked worktrees of the repository `dir` is in that are
/// done with: the branch is merged into what the main tree has checked
/// out, nothing in the tree is changed or untracked, and no session in
/// `busy` has its folder there. Hand-made ones too, since an agent that
/// adds a worktree itself rarely removes it after the merge. The branch
/// must have been committed to, or a worktree just added and not yet
/// worked in would count as merged and go. Their branches go with them.
/// A dev instance sweeps only the worktrees it added, see
/// [`worktree::sweepable`]. Returns the folders removed.
pub fn sweep(dir: &Path, busy: &[String]) -> Vec<String> {
    let Some(place) = main_tree(dir) else {
        return Vec::new();
    };
    let main = Path::new(&place.top);
    let Ok(list) = git(main, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let dev = horadric_hooks::dev();
    let mut removed = Vec::new();
    for w in worktree::linked(&list) {
        if busy.iter().any(|d| worktree::inside(d, &w.path)) {
            continue;
        }
        if !worktree::sweepable(dev, dev && dev_made(Path::new(&w.path))) {
            continue;
        }
        let branch = format!("refs/heads/{}", w.branch);
        let merged = git(main, &["merge-base", "--is-ancestor", &branch, "HEAD"]).is_ok();
        let worked_in = git(main, &["reflog", "show", "--format=%H", &branch])
            .is_ok_and(|log| log.lines().count() > 1);
        let clean = git(
            Path::new(&w.path),
            &["--no-optional-locks", "status", "--porcelain"],
        )
        .is_ok_and(|s| s.trim().is_empty());
        if !(merged && worked_in && clean) {
            continue;
        }
        if let Err(e) = git(main, &["worktree", "remove", &w.path]) {
            eprintln!("horadric: kept the worktree {}: {e}", w.path);
            continue;
        }
        if let Err(e) = git(main, &["branch", "-d", &w.branch]) {
            eprintln!("horadric: kept the branch {}: {e}", w.branch);
        }
        removed.push(w.path);
    }
    removed
}

/// Counts what a session's worktree has changed: against its last commit,
/// untracked files included, and on its branch since the main tree's
/// commit. None once the worktree is gone. `--no-optional-locks` keeps
/// git from rewriting the index, which a watcher would take for a change.
pub fn count(w: &Worktree) -> Option<Diff> {
    let top = Path::new(&w.path);
    if !top.is_dir() {
        return None;
    }
    let numstat = |range: &str| {
        git(
            top,
            &[
                "--no-optional-locks",
                "diff",
                "--numstat",
                "-z",
                "--no-renames",
                range,
            ],
        )
        .map(|out| diff::parse_numstat(&out))
    };
    let mut uncommitted = numstat("HEAD").ok()?;
    uncommitted.extend(untracked(top));
    // The main tree may have moved on since; `...` counts from where the
    // branch left it.
    let committed = git(Path::new(&w.main), &["rev-parse", "HEAD"])
        .and_then(|main| numstat(&format!("{}...HEAD", main.trim())))
        .unwrap_or_default();
    Some(Diff {
        uncommitted,
        committed,
    })
}

/// The files git does not track yet in `top`, each counted as all added.
fn untracked(top: &Path) -> Vec<diff::FileDiff> {
    let others = git(
        top,
        &[
            "--no-optional-locks",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ],
    )
    .unwrap_or_default();
    others
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(|path| {
            let content = std::fs::read(top.join(path)).unwrap_or_default();
            diff::untracked(path, &content)
        })
        .collect()
}

/// Where a phone reads what a session changed: the top of its tree, and
/// the commit the changes count from. For a session in a worktree that is
/// where its branch left the main tree, so its commits count too; for any
/// other, its tree's last commit. None outside a repository, or in one
/// with no commit yet.
pub fn changes_base(cwd: &Path, w: Option<&Worktree>) -> Option<(PathBuf, String)> {
    let top = match w {
        Some(w) => PathBuf::from(&w.path),
        None => PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"]).ok()?.trim()),
    };
    let head = git(&top, &["rev-parse", "HEAD"]).ok()?.trim().to_string();
    let base = w
        .and_then(|w| git(Path::new(&w.main), &["rev-parse", "HEAD"]).ok())
        .and_then(|main| git(&top, &["merge-base", main.trim(), &head]).ok())
        .map_or(head, |b| b.trim().to_string());
    Some((top, base))
}

/// Every file changed in `top` since `base`, committed or not, untracked
/// ones included, with the lines each added and removed.
pub fn changed_files(top: &Path, base: &str) -> Option<Vec<diff::FileDiff>> {
    let out = git(
        top,
        &[
            "--no-optional-locks",
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            base,
        ],
    )
    .ok()?;
    let mut files = diff::parse_numstat(&out);
    files.extend(untracked(top));
    Some(files)
}

/// One file's diff since `base`, as `git diff` prints it, without colour.
/// A file git does not track is diffed against nothing, all added.
pub fn file_diff(top: &Path, base: &str, path: &str) -> Option<String> {
    let tracked = git(
        top,
        &[
            "--no-optional-locks",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            base,
            "--",
            path,
        ],
    )
    .ok()?;
    if !tracked.is_empty() {
        return Some(tracked);
    }
    // `git diff --no-index` exits 1 when the files differ, which they do.
    let out = Command::new("git")
        .args([
            "--no-optional-locks",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-index",
            "--",
            "/dev/null",
            path,
        ])
        .current_dir(top)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    matches!(out.status.code(), Some(0 | 1))
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs git in `dir`: what it printed, or what it said when it failed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // A merge that stops on a conflict says why on stdout.
        let said =
            [&out.stdout, &out.stderr].map(|s| String::from_utf8_lossy(s).trim().to_string());
        Err(said
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// How long one check may run before it counts as failed, so a test that
/// hangs does not hold every later merge of its project.
const CHECK_LONGEST: Duration = Duration::from_secs(30 * 60);

/// How a finished quest's merge into the main tree came out.
pub enum Landing {
    /// `main` holds the branch now, or held it already. With the commit
    /// the checks passed on, which `main` was left at, or empty when it
    /// held the branch already and none ran.
    Merged(String),
    /// A step failed with this output, and the other side was this. The
    /// branch stayed.
    Failed(merge::Failure, String, merge::Theirs),
}

/// Merges the finished quest in worktree `w` into `into`, what the main
/// tree has checked out: rebased on it in the worktree, `checks` run
/// there, and `into` fast forwarded to it. Starts over when `into` moved
/// meanwhile, without the checks when what it gained touches no file the
/// branch changed. `live` when the quest's session still runs, idle, in
/// the worktree; else it was just ended. Blocks for as long as the checks
/// take, so it runs on a thread of its own.
pub fn land(w: &Worktree, into: &str, checks: &[String], live: bool) -> Landing {
    let main = Path::new(&w.main);
    let tree = Path::new(&w.path);
    if !live {
        // The agent was just ended, and may hold its files for a moment.
        std::thread::sleep(Duration::from_secs(1));
    }
    if git(main, &["merge-base", "--is-ancestor", &w.branch, into]).is_ok() {
        return Landing::Merged(String::new());
    }
    let mut last = (
        merge::Failure::Moved,
        String::new(),
        merge::Theirs::default(),
    );
    // The commit of `into` the checks last passed on, so a later move of
    // `into` is weighed against it.
    let mut checked_on: Option<String> = None;
    let (mut full, mut quiet) = (0, 0);
    loop {
        let rerun = checked_on
            .as_deref()
            .is_none_or(|on| needs_checks(tree, on, into));
        let (tries, most) = if rerun {
            (&mut full, merge::TRIES)
        } else {
            (&mut quiet, merge::QUIET_TRIES)
        };
        if *tries == most {
            break;
        }
        *tries += 1;
        let mut moved = false;
        // Asked before the rebase moves the branch onto them.
        let gained = format!("HEAD..{into}");
        let mut theirs = merge::Theirs {
            files: Vec::new(),
            subjects: git(tree, &["log", "--format=%s", &gained])
                .map(|l| merge::lines(&l))
                .unwrap_or_default(),
        };
        for step in merge::plan(if rerun { checks } else { &[] }) {
            let done = match &step {
                merge::Step::Rebase => git(tree, &["rebase", into]),
                merge::Step::Check(c) => shell(tree, c),
                merge::Step::FastForward => git(main, &["merge", "--ff-only", &w.branch]),
            };
            let Err(out) = done else { continue };
            let why = merge::failed(&step, &out);
            if step == merge::Step::Rebase {
                if why == merge::Failure::Conflict {
                    theirs = conflict_side(tree, &w.branch, into, theirs.subjects);
                }
                let _ = git(tree, &["rebase", "--abort"]);
            }
            if why == merge::Failure::Moved {
                if rerun {
                    checked_on = git(tree, &["merge-base", "HEAD", into])
                        .ok()
                        .map(|b| b.trim().to_string());
                }
                last = (why, out, theirs);
                moved = true;
                break;
            }
            return Landing::Failed(why, out, theirs);
        }
        if !moved {
            return Landing::Merged(head(tree).unwrap_or_default());
        }
    }
    Landing::Failed(last.0, last.1, last.2)
}

/// Whether `into` moved since `on`, the commit the checks passed on, in
/// a way that needs them again: it changed a file the branch checked out
/// in `tree` changed. When git cannot tell, they run again.
fn needs_checks(tree: &Path, on: &str, into: &str) -> bool {
    let changed = |from: &str, to: &str| {
        // Without renames, so a moved file counts on both of its paths.
        git(tree, &["diff", "--name-only", "--no-renames", from, to]).map(|l| merge::lines(&l))
    };
    let base = git(tree, &["merge-base", "HEAD", into]).map(|b| b.trim().to_string());
    let ours = base.and_then(|b| changed(&b, "HEAD"));
    let theirs = changed(on, into);
    match (ours, theirs) {
        (Ok(ours), Ok(theirs)) => merge::recheck(&ours, &theirs),
        _ => true,
    }
}

/// The other side of the rebase of `branch` stopped in `tree`: the files
/// it stopped on, and of `all` that `into` gained, the commits that
/// touched them when any did. Asked before the rebase is undone, while
/// `branch` still points where it did before it.
fn conflict_side(tree: &Path, branch: &str, into: &str, all: Vec<String>) -> merge::Theirs {
    let files = git(tree, &["diff", "--name-only", "--diff-filter=U"])
        .map(|f| merge::lines(&f))
        .unwrap_or_default();
    let range = format!("{branch}..{into}");
    let mut args = vec!["log", "--format=%s", range.as_str(), "--"];
    args.extend(files.iter().map(String::as_str));
    let touched = git(tree, &args)
        .ok()
        .filter(|_| !files.is_empty())
        .map(|l| merge::lines(&l))
        .filter(|s| !s.is_empty());
    merge::Theirs {
        subjects: touched.unwrap_or(all),
        files,
    }
}

/// Runs `command` with `cmd /c` in `dir`, with no window: what it printed
/// when it exits 0, else what it printed, stdout and stderr as they came.
fn shell(dir: &Path, command: &str) -> Result<String, String> {
    let (mut read, write) = std::io::pipe().map_err(|e| format!("cannot run {command}: {e}"))?;
    let err = write
        .try_clone()
        .map_err(|e| format!("cannot run {command}: {e}"))?;
    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/d").arg("/c").raw_arg(command);
    let child = cmd
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(write)
        .stderr(err)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    // The pipe ends only once every writer is gone, ours included.
    drop(cmd);
    let mut child = child.map_err(|e| format!("cannot run {command}: {e}"))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut read, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < CHECK_LONGEST => {
                std::thread::sleep(Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        // What it started may still hold the pipe, so its output is not
        // waited for.
        let minutes = CHECK_LONGEST.as_secs() / 60;
        return Err(format!("{command} ran longer than {minutes} minutes"));
    };
    let out = reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }

    #[test]
    fn a_conflicted_landing_names_the_files_and_the_other_side_and_is_undone() {
        let base = std::env::temp_dir().join(format!("horadric-land-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let top = base.join("main");
        std::fs::create_dir_all(&top).unwrap();
        run(&top, &["init", "-q", "--template=", "-b", "main"]);
        run(&top, &["config", "user.email", "t@example.com"]);
        run(&top, &["config", "user.name", "t"]);
        std::fs::write(top.join("a.txt"), "one\n").unwrap();
        std::fs::write(top.join("b.txt"), "b\n").unwrap();
        run(&top, &["add", "."]);
        run(&top, &["commit", "-q", "-m", "first"]);
        let path = base.join("q").to_string_lossy().replace('\\', "/");
        run(&top, &["worktree", "add", "-q", "-b", "q", &path]);
        let tree = Path::new(&path);
        std::fs::write(tree.join("a.txt"), "mine\n").unwrap();
        run(tree, &["commit", "-q", "-am", "Mine"]);
        std::fs::write(top.join("a.txt"), "theirs\n").unwrap();
        run(&top, &["commit", "-q", "-am", "Theirs on a"]);
        std::fs::write(top.join("b.txt"), "b2\n").unwrap();
        run(&top, &["commit", "-q", "-am", "Theirs on b"]);

        let w = Worktree {
            path: path.clone(),
            main: top.to_string_lossy().replace('\\', "/"),
            branch: "q".into(),
            ports: None,
        };
        let Landing::Failed(why, _, theirs) = land(&w, "main", &[], true) else {
            panic!("it should conflict");
        };
        assert_eq!(why, merge::Failure::Conflict);
        assert_eq!(theirs.files, ["a.txt"]);
        assert_eq!(theirs.subjects, ["Theirs on a"]);
        // Undone, so the session that mends it starts from its own work.
        assert!(git(tree, &["status", "--porcelain"]).unwrap().is_empty());
        let mine = std::fs::read_to_string(tree.join("a.txt")).unwrap();
        assert_eq!(mine.trim_end(), "mine");
        let _ = git(&top, &["worktree", "remove", "--force", &path]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_move_of_main_needs_the_checks_only_when_it_touched_a_file_of_ours() {
        let base = std::env::temp_dir().join(format!("horadric-recheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let top = base.join("main");
        std::fs::create_dir_all(&top).unwrap();
        run(&top, &["init", "-q", "-b", "main"]);
        run(&top, &["config", "user.email", "t@example.com"]);
        run(&top, &["config", "user.name", "t"]);
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(top.join(f), "one\n").unwrap();
        }
        run(&top, &["add", "."]);
        run(&top, &["commit", "-q", "-m", "first"]);
        let on = head(&top).unwrap();
        let path = base.join("q").to_string_lossy().replace('\\', "/");
        run(&top, &["worktree", "add", "-q", "-b", "q", &path]);
        let tree = Path::new(&path);
        std::fs::write(tree.join("a.txt"), "mine\n").unwrap();
        run(tree, &["commit", "-q", "-am", "Mine"]);

        std::fs::write(top.join("b.txt"), "theirs\n").unwrap();
        run(&top, &["commit", "-q", "-am", "Theirs on b"]);
        assert!(!needs_checks(tree, &on, "main"));
        std::fs::write(top.join("a.txt"), "theirs\n").unwrap();
        run(&top, &["commit", "-q", "-am", "Theirs on a"]);
        assert!(needs_checks(tree, &on, "main"));
        // A commit git does not know cannot be weighed, so the checks run.
        assert!(needs_checks(tree, "0123456789abcdef", "main"));
        let _ = git(&top, &["worktree", "remove", "--force", &path]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_main_branch_is_what_is_checked_out_else_a_default() {
        let has = |b: &str| ["main", "trunk"].contains(&b);
        assert_eq!(
            pick_main_branch(Some("dev"), None, has).as_deref(),
            Some("dev")
        );
        assert_eq!(
            pick_main_branch(None, Some("origin/trunk\n"), has).as_deref(),
            Some("trunk")
        );
        assert_eq!(
            pick_main_branch(None, Some("origin/gone"), has).as_deref(),
            Some("main")
        );
        assert_eq!(pick_main_branch(None, None, |_| false), None);
    }

    #[test]
    fn a_quest_starts_from_the_main_branch_not_a_detached_head() {
        let top = std::env::temp_dir().join(format!("horadric-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(&top).unwrap();
        run(&top, &["init", "-q", "-b", "main"]);
        run(&top, &["config", "user.email", "t@example.com"]);
        run(&top, &["config", "user.name", "t"]);
        run(&top, &["commit", "-q", "--allow-empty", "-m", "first"]);
        // As a `pull --rebase` leaves it midway: HEAD on a commit of its own.
        run(&top, &["checkout", "-q", "--detach"]);
        run(&top, &["commit", "-q", "--allow-empty", "-m", "stray"]);
        assert_eq!(main_branch(&top).as_deref(), Some("main"));

        let place = main_tree(&top).expect("a main tree");
        let path = worktree::folder(&place.top, "quest");
        let _ = std::fs::remove_dir_all(&path);
        let start = main_branch(&top).unwrap();
        run(
            &top,
            &["worktree", "add", "-q", "-b", "quest", &path, &start],
        );
        let log = git(Path::new(&path), &["log", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), "first");
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn a_phone_sees_tracked_and_untracked_changes() {
        let top = std::env::temp_dir().join(format!("horadric-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.join("src")).unwrap();
        run(&top, &["init", "-q", "--template="]);
        run(&top, &["config", "user.email", "t@example.com"]);
        run(&top, &["config", "user.name", "t"]);
        std::fs::write(top.join("src/a.txt"), "one\ntwo\n").unwrap();
        run(&top, &["add", "."]);
        run(&top, &["commit", "-q", "-m", "first"]);
        std::fs::write(top.join("src/a.txt"), "one\nthree\nfour\n").unwrap();
        std::fs::write(top.join("new.txt"), "fresh\n").unwrap();

        // Asked from a subfolder, the answer is about the whole tree.
        let (found, base) = changes_base(&top.join("src"), None).expect("a repository");
        let files = changed_files(&found, &base).unwrap();
        let mut paths: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.lines)).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![("new.txt", Some((1, 0))), ("src/a.txt", Some((2, 1)))]
        );

        let tracked = file_diff(&found, &base, "src/a.txt").unwrap();
        assert!(tracked.contains("-two\n+three\n+four\n"), "{tracked}");
        let fresh = file_diff(&found, &base, "new.txt").unwrap();
        assert!(fresh.contains("+fresh"), "{fresh}");

        let outside = std::env::temp_dir().join(format!("horadric-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert!(changes_base(&outside, None).is_none(), "no repository");
        let _ = std::fs::remove_dir_all(&top);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
