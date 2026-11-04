//! The Windows app's side of the quest log: the menus and dialogs that
//! drive a list from its tile, and what the app gives the runner and
//! Warriv, which are [`crate::quests`] and run alike on the Mac.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use horadric_core::ship;
use horadric_core::tasks::{self, Mark, Mode, Task};
use horadric_core::warriv::Drive;
use horadric_core::{Registry, Usage};

use super::{post, push, with_app, App, Input, WM_HORADRIC_KEPT, WM_HORADRIC_TASK_MENU};
use crate::ask;
use crate::board::{Board, Ink, RowState};
use crate::menu::{self, Item};
use crate::questlog::Ask;
use crate::quests::{self, Host, Menu, Runner, SessionSpec, Toast, Warriv};
use crate::window::project_name;

#[path = "runeword.rs"]
pub(super) mod runeword;

impl Host for App {
    fn tasks(&self) -> &quests::State {
        &self.tasks
    }
    fn tasks_mut(&mut self) -> &mut quests::State {
        &mut self.tasks
    }
    fn drives(&self) -> &BTreeMap<String, Drive> {
        &self.drives
    }
    fn drives_mut(&mut self) -> &mut BTreeMap<String, Drive> {
        &mut self.drives
    }
    fn stopped(&self) -> &HashSet<String> {
        &self.stopped
    }
    fn stopped_mut(&mut self) -> &mut HashSet<String> {
        &mut self.stopped
    }
    fn closed(&self) -> &HashSet<String> {
        &self.closed
    }
    fn recent(&self) -> &[String] {
        &self.recent
    }
    fn tiles(&self) -> Vec<String> {
        self.clusters.iter().map(|c| c.key.clone()).collect()
    }
    fn registry(&self) -> &Arc<Mutex<Registry>> {
        &self.shared.registry
    }
    fn usage(&self) -> &Arc<Mutex<Option<Usage>>> {
        &self.shared.usage
    }
    fn boards(&self) -> &RefCell<HashMap<String, Board>> {
        &self.shared.boards
    }
    fn warriv_shown(&self) -> &RefCell<HashMap<String, BTreeSet<String>>> {
        &self.shared.warriv
    }
    fn astir(&self) -> &RefCell<HashSet<String>> {
        &self.shared.astir
    }
    fn warriv_line(&self) -> &RefCell<HashMap<String, (String, Ink)>> {
        &self.shared.warriv_line
    }
    fn holding_still(&self) -> bool {
        self.frozen || self.reload.is_some()
    }
    fn quiet(&self) -> bool {
        self.quiet
    }
    fn local_secs(&self) -> u64 {
        super::local_secs()
    }
    fn chat_sessions(&self) -> bool {
        self.chat_sessions
    }
    fn browser_prompt(&self) -> Option<String> {
        (self.mcp_config.is_some()).then(|| crate::web::AGENT_PROMPT.to_string())
    }
    fn project_dir(&self, key: &str) -> Option<PathBuf> {
        App::project_dir(self, key)
    }
    fn project_of(&self, id: &str) -> Option<String> {
        App::project_of(self, id)
    }
    fn unique_id(&self, base: &str) -> String {
        App::unique_id(self, base)
    }
    fn runs_here(&self, id: &str) -> bool {
        App::runs_here(self, id)
    }
    fn exited(&self, id: &str) -> bool {
        (self.consoles.get(id)).is_some_and(|c| c.exit_code().is_some())
    }
    fn start_session(&mut self, spec: SessionSpec) -> Result<String, String> {
        App::start_session(self, spec)
    }
    fn send_prompt(&mut self, id: &str, text: &str) -> bool {
        App::send_prompt(self, id, text)
    }
    fn press_enter(&mut self, id: &str) {
        if let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) {
            c.write(b"\r".to_vec());
        }
    }
    fn end(&mut self, id: &str) {
        App::end(self, id)
    }
    fn forget(&mut self, id: &str) {
        App::forget(self, id)
    }
    fn reveal(&mut self, id: &str) {
        App::reveal(self, id, false)
    }
    fn show_session(&mut self, key: &str, id: &str) {
        if self.fill_stage(key) {
            if let Some(stage) = &self.stage {
                stage.focus_session(id);
            }
        }
    }
    fn reconcile(&mut self) {
        App::reconcile(self, false)
    }
    fn save(&mut self) {
        App::save(self)
    }
    fn toast(&mut self, kind: Toast, title: &str, text: &str) {
        self.toasts.show(toast_kind(kind), title, text);
    }
    fn toast_click(&mut self, kind: Toast, title: &str, text: &str) {
        // The click answers the runner's question, not an older alert's.
        self.alert_for = None;
        self.update_click = false;
        self.toasts.show(toast_kind(kind), title, text);
    }
    fn sound(&self, loot: crate::loot::Loot) {
        App::sound(self, loot)
    }
    fn lists_changed(&mut self, keys: &[String]) {
        let mut resized = false;
        for c in self.clusters.iter().filter(|c| keys.contains(&c.key)) {
            resized |= c.fit();
        }
        if resized {
            self.arrange();
        }
    }
    fn redraw_tiles(&self) {
        for c in &self.clusters {
            c.invalidate();
        }
    }
    fn waker(&self) -> quests::Waker {
        let notify = self.notify.0 as isize;
        Arc::new(move || post(notify, WM_HORADRIC_KEPT, 0))
    }
    fn ask(&mut self, menu: Menu) {
        ask_for(self, menu)
    }
    fn carry_on(&self, key: &str, holder: &str) {
        push(Input::QuestLog(Ask::Carry(
            key.to_string(),
            holder.to_string(),
        )));
    }
    fn open_list(&self, dir: &Path, rel: &str) {
        crate::watch::open(dir, rel);
    }
    fn recall_ready(&self, dir: &Path, query: &str) -> bool {
        crate::memory::prepare(dir, query)
    }
    fn take_recall(&self, dir: &Path, query: &str) -> Option<String> {
        crate::memory::take_recall(dir, query)
    }
    fn light_pack(&self, key: &str, id: &str, files: &[String]) {
        if files.is_empty() {
            return;
        }
        let pack = horadric_core::glow::Touch::Pack;
        let lit: Vec<_> = files.iter().map(|f| (f.clone(), pack)).collect();
        crate::glows::light(key, id, &lit);
    }
    fn has_stone(&self, key: &str, label: &str) -> bool {
        App::has_stone(self, key, label)
    }
    fn errand_bypasses(&self, id: &str) -> bool {
        App::errand_bypasses(self, id)
    }
    fn errand_of_session(&self, id: &str) -> Option<(String, String)> {
        App::errand_of_session(self, id)
    }
    fn errand_event(&mut self, key: Option<&str>, event: horadric_core::runeword::Event) {
        App::errand_event(self, key, event)
    }
    fn halt_errands(&mut self, key: &str) {
        App::halt_errands(self, key)
    }
    fn tick_stones(&mut self) {
        self.tick_runewords();
        self.tick_errands();
        self.refresh_tomes();
    }
}

fn toast_kind(kind: Toast) -> crate::toast::Kind {
    match kind {
        Toast::Waiting => crate::toast::Kind::Waiting,
        Toast::Done => crate::toast::Kind::Done,
        Toast::Info => crate::toast::Kind::Info,
        Toast::Failed => crate::toast::Kind::Failed,
    }
}

impl App {
    /// The projects the tray's "Warriv drives" lists: each with a quest
    /// log, and each Warriv drives, by name.
    pub(super) fn driven(&self) -> Vec<crate::tray::Driven> {
        let mut keys: Vec<String> = self.shared.boards.borrow().keys().cloned().collect();
        keys.extend(self.drives.keys().cloned());
        keys.sort();
        keys.dedup();
        let mut out: Vec<crate::tray::Driven> = keys
            .into_iter()
            .map(|key| crate::tray::Driven {
                name: project_name(&key),
                drive: self.drive_of(&key),
                key,
            })
            .collect();
        out.sort_by_key(|d| d.name.to_lowercase());
        out
    }
}

/// Queues a menu for the app's window, which shows it outside the app's
/// borrow: a menu runs a modal loop that dispatches the app's messages.
pub(super) fn ask_for(app: &mut App, menu: Menu) {
    app.tasks.menu = Some(menu);
    post(app.notify.0 as isize, WM_HORADRIC_TASK_MENU, 0);
}

/// Shows the menu or dialog the app queued.
pub(super) fn show_menu(menu: Menu) {
    match menu {
        Menu::Item(key, line, title) => item_menu(&key, line, &title),
        Menu::Brief(key, line, title) => brief(&key, line, &title),
        Menu::Mode(key) => mode_menu(&key),
        Menu::Add(key) => {
            let question = ask::Ask {
                title: "New quest",
                prompt: "What should be done? The notes go to the agent with it.",
                initial: "",
                placeholder: "A title for the quest",
                verb: "add it",
                notes: true,
                secret: false,
                pick: None,
            };
            if let Some(a) = ask_quest(&key, &draft_for(&key, None), &question, "") {
                with_app(|app| app.add_task(&key, &a.text, &a.notes));
            }
        }
        Menu::Ship(key) => {
            with_app(|app| app.fill_stage(&key));
            runeword::stone_clicked(&key, ship::STONE);
        }
        Menu::Merge(m) => {
            let into = crate::worktree::checked_out(&m.main).unwrap_or_else(|| "main".into());
            let pressed = super::ask(&crate::dialog::Dialog {
                tone: crate::dialog::Tone::Question,
                title: "Merge completed quest",
                text: &merge_question(&m.title, &m.branch, &into),
                buttons: &["Merge", "Not now"],
                default: 0,
                check: None,
            });
            if pressed == Some(0) {
                with_app(|app| app.merge(&m));
            }
        }
    }
}

/// What to ask before merging a finished item's branch.
fn merge_question(title: &str, branch: &str, into: &str) -> String {
    format!(
        "\"{}\" is finished on the branch {branch}.\n\nMerge it into {into}? A conflict \
         undoes the merge and keeps the branch.",
        tasks::one_line(title)
    )
}

fn item_menu(key: &str, line: usize, title: &str) {
    const START: usize = 1;
    const SHOW: usize = 2;
    const APPROVE: usize = 3;
    const DONE: usize = 4;
    const BACK: usize = 5;
    const EDIT: usize = 6;
    const REWRITE: usize = 7;
    const UP: usize = 8;
    const DOWN: usize = 9;
    const DELETE: usize = 10;
    const LOG: usize = 11;
    const RUN: usize = 12;
    let Some((state, t, (first, last), unapproved)) = with_app(|app| {
        let t = app.task_at(key, line, title)?;
        let unapproved = app
            .project_dir(key)
            .and_then(|d| quests::runner::unapproved_wait(&d, &t).map(str::to_string));
        let boards = app.shared.boards.borrow();
        let board = boards.get(key)?;
        let ends = (
            board.tasks.first().is_some_and(|f| f.line == line),
            board.tasks.last().is_some_and(|l| l.line == line),
        );
        drop(boards);
        Some((app.row_state(&t), t, ends, unapproved))
    })
    .flatten() else {
        return;
    };
    let holder = t.holder.clone();
    let mut items = Vec::new();
    match state {
        RowState::Open => items.push(Item::action(START, "Accept")),
        RowState::Gone => items.push(Item::action(START, "Accept again")),
        _ => items.push(Item::action(SHOW, "Show session")),
    }
    match state {
        RowState::Open => {}
        RowState::Review => items.push(Item::action(APPROVE, "Approve")),
        _ => items.push(Item::action(DONE, "Mark completed")),
    }
    if holder.is_some() {
        items.push(Item::action(BACK, "Put back in the log"));
    }
    if unapproved.is_some() {
        items.push(Item::action(RUN, "Approve and run"));
    }
    items.push(Item::Separator);
    items.push(Item::action(REWRITE, "Edit quest"));
    if !first {
        items.push(Item::action(UP, "Move up"));
    }
    if !last {
        items.push(Item::action(DOWN, "Move down"));
    }
    // A held quest has a session working it, which must be let go first.
    if holder.is_none() || t.mark == Mark::Done {
        items.push(Item::action(DELETE, "Delete quest"));
    }
    items.push(Item::action(EDIT, "Edit the quest log"));
    items.push(Item::action(LOG, "Quest log..."));
    // Outside the app's borrow: the menu's loop dispatches its messages.
    let picked = menu::popup(&items);
    match picked {
        Some(REWRITE) => return rewrite(key, &t),
        Some(LOG) => return push(Input::QuestLog(Ask::Open(key.to_string()))),
        Some(DELETE) if !confirm_delete(title) => return,
        Some(RUN) => {
            let commands: Vec<String> = unapproved.into_iter().collect();
            if confirm_approve(false, &commands) {
                with_app(|app| app.approve_commands(key, &commands));
            }
            return;
        }
        _ => {}
    }
    with_app(|app| match picked {
        Some(START) => app.accept_task(key, line, title),
        Some(SHOW) => app.show_quest(key, &t),
        Some(APPROVE | DONE) => app.set_task(key, line, title, Mark::Done),
        Some(BACK) => app.put_back(key, line, title),
        Some(EDIT) => app.edit_list(key),
        Some(UP) => app.change_list(key, |text| tasks::shift(text, line, title, true)),
        Some(DOWN) => app.change_list(key, |text| tasks::shift(text, line, title, false)),
        Some(DELETE) => app.change_list(key, |text| tasks::remove(text, line, title)),
        _ => {}
    });
}

/// Shows a quest before it starts, to accept, edit or leave.
fn brief(key: &str, line: usize, title: &str) {
    let Some((t, state)) = with_app(|app| {
        let t = app.task_at(key, line, title)?;
        let state = app.row_state(&t);
        Some((t, state))
    })
    .flatten() else {
        return;
    };
    let accept = match state {
        RowState::Gone | RowState::Blocked | RowState::Waits => "Accept again",
        _ => "Accept",
    };
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Question,
        title: "Quest",
        text: &briefing(&t),
        buttons: &[accept, "Edit quest", "Not now"],
        default: 0,
        check: None,
    });
    match pressed {
        Some(0) => {
            with_app(|app| app.accept_task(key, line, title));
        }
        Some(1) => rewrite(key, &t),
        _ => {}
    }
}

/// What a quest's briefing says: its title, then its notes, or that it
/// has none, since the agent gets nothing more than this.
fn briefing(t: &Task) -> String {
    let title = tasks::one_line(&t.title);
    if t.notes.is_empty() {
        format!(
            "{title}

No notes. The agent gets only the title."
        )
    } else {
        format!(
            "{title}

{}",
            t.notes.join(
                "
"
            )
        )
    }
}

/// Asks for a quest's new title and notes, the old ones filled in.
fn rewrite(key: &str, t: &Task) {
    let question = ask::Ask {
        title: "Edit quest",
        prompt: "What should be done? The notes go to the agent with it.",
        initial: &t.title,
        placeholder: "A title for the quest",
        verb: "save it",
        notes: true,
        secret: false,
        pick: None,
    };
    let notes = t.notes.join(
        "
",
    );
    let draft = draft_for(key, Some((t.line, &t.title)));
    let Some(a) = ask_quest(key, &draft, &question, &notes) else {
        return;
    };
    with_app(|app| {
        app.change_list(key, |text| {
            tasks::edit(text, t.line, &t.title, &a.text, &a.notes)
        })
    });
}

/// Where a quest's draft is kept: by project for a new one, and for an
/// edit by its line and title too, so a draft for a quest that has since
/// changed is not filled into another.
fn draft_for(key: &str, edit: Option<(usize, &str)>) -> String {
    match edit {
        None => format!(
            "{key}
new"
        ),
        Some((line, title)) => format!(
            "{key}
edit
{line}
{title}"
        ),
    }
}

/// Whether a draft says more than the input started with, and so is worth
/// filling back in.
fn worth_keeping(d: &ask::Draft, initial: &str, notes: &str) -> bool {
    d.text != initial || d.notes != notes
}

/// Asks for a quest's title and notes beside the project's cluster, filling
/// in the draft kept under `draft`, and keeps what was typed when the
/// input is clicked away from. Answering or Esc drops the draft.
fn ask_quest(key: &str, draft: &str, question: &ask::Ask, notes: &str) -> Option<ask::Answer> {
    let (shared, beside, kept) = with_app(|app| {
        let beside = app.clusters.iter().find(|c| c.key == key).map(|c| c.hwnd);
        let kept = app.tasks.drafts.get(draft).cloned();
        (Rc::clone(&app.shared), beside, kept)
    })?;
    let reply = ask::ask_or_leave(shared, beside, question, notes, kept.as_ref());
    with_app(|app| match &reply {
        ask::Reply::Left(d) if worth_keeping(d, question.initial, notes) => {
            app.tasks.drafts.insert(draft.to_string(), d.clone());
        }
        _ => {
            app.tasks.drafts.remove(draft);
        }
    });
    match reply {
        ask::Reply::Answered(a) => Some(a),
        ask::Reply::Cancelled | ask::Reply::Left(_) => None,
    }
}

/// Whether the human really means to delete a quest. Its notes go with
/// it, and the log may not be committed.
fn confirm_delete(title: &str) -> bool {
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Warning,
        title: "Delete quest",
        text: &format!(
            "Delete \"{}\" and its notes from the quest log?",
            tasks::one_line(title)
        ),
        buttons: &["Delete", "Keep it"],
        default: 1,
        check: None,
    });
    pressed == Some(0)
}

/// Whether the human approves running `commands`, which came with the
/// repository. Shown whole, Cancel the default: this is the consent.
fn confirm_approve(checks: bool, commands: &[String]) -> bool {
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Warning,
        title: if checks { "Approve checks" } else { "Approve and run" },
        text: &quests::runner::approve_text(checks, commands),
        buttons: &["Approve", "Cancel"],
        default: 1,
        check: None,
    });
    pressed == Some(0)
}

/// How many at once the mode menu offers. The config takes up to
/// `tasks::MOST_PARALLEL`.
const AT_ONCE: [usize; 5] = [1, 2, 4, 8, 16];

fn mode_menu(key: &str) {
    const EDIT: usize = 10;
    const LOG: usize = 11;
    const DRIVES: usize = 12;
    const PUBLIC: usize = 13;
    const SUGGEST: usize = 14;
    const CHECKS: usize = 15;
    const ORCHESTRATOR: usize = 16;
    // Plus how many, so each choice of `AT_ONCE` has an id of its own.
    const PARALLEL: usize = 20;
    let Some(board) = with_app(|app| app.shared.boards.borrow().get(key).cloned()) else {
        return;
    };
    let board = board.unwrap_or_default();
    let (drive, stopped) =
        with_app(|app| (app.drive_of(key), app.stopped.contains(key))).unwrap_or_default();
    // Read each time the menu opens: the repository's config may have
    // changed since, and the menu is where the human acts on it.
    let dir = with_app(|app| app.project_dir(key)).flatten();
    let suggested = dir
        .as_ref()
        .and_then(|d| quests::runner::suggestion(d, board.own_trees));
    let unapproved = dir
        .as_ref()
        .and_then(|d| horadric_hooks::tasks::checks(d).err())
        .unwrap_or_default();
    let repo_warriv = dir
        .as_ref()
        .is_some_and(|d| horadric_hooks::tasks::suggested_orchestrator(d));
    let mut items: Vec<Item> = Mode::ALL
        .iter()
        .enumerate()
        .map(|(i, m)| Item::Action {
            id: i + 1,
            label: m.explain().to_string(),
            checked: *m == board.mode,
            bold: false,
        })
        .collect();
    if stopped {
        items.push(Item::Disabled(
            "Warriv was stopped: pick a mode to go on".into(),
        ));
    }
    if let Some(s) = suggested {
        items.push(Item::action(SUGGEST, quests::runner::suggestion_label(s)));
    }
    if !unapproved.is_empty() {
        items.push(Item::action(
            CHECKS,
            quests::runner::approve_checks_label(&unapproved),
        ));
    }
    items.push(Item::Separator);
    items.push(Item::Action {
        id: ORCHESTRATOR,
        label: quests::runner::orchestrator_label(board.orchestrator, repo_warriv).into(),
        checked: board.orchestrator,
        bold: false,
    });
    items.push(Item::Action {
        id: DRIVES,
        label: "Warriv drives".into(),
        checked: drive.is_some(),
        bold: false,
    });
    let public = drive.as_ref().is_some_and(|d| d.ships_public);
    match &drive {
        Some(_) => items.push(Item::Action {
            id: PUBLIC,
            label: "and ships public".into(),
            checked: public,
            bold: false,
        }),
        None => items.push(Item::Disabled("and ships public".into())),
    }
    if let Some(h) = drive.as_ref().and_then(|d| d.held.as_ref()) {
        items.push(Item::Disabled(format!(
            "Shipping held until \"{}\" lands",
            tasks::one_line(&h.quest)
        )));
    }
    items.push(Item::Separator);
    if board.own_trees {
        items.extend(AT_ONCE.iter().map(|&n| Item::Action {
            id: PARALLEL + n,
            label: at_once(n),
            checked: n == board.parallel.max(1),
            bold: false,
        }));
    } else {
        items.push(Item::Disabled(
            "One at a time: several need a git repository".into(),
        ));
    }
    items.push(Item::Separator);
    items.push(Item::action(EDIT, "Edit the quest log"));
    items.push(Item::action(LOG, "Quest log..."));
    let picked = menu::popup(&items);
    if picked == Some(LOG) {
        return push(Input::QuestLog(Ask::Open(key.to_string())));
    }
    if picked == Some(PUBLIC) && !public && !ships_public(key) {
        return;
    }
    if picked == Some(CHECKS) && !confirm_approve(true, &unapproved) {
        return;
    }
    with_app(|app| match picked {
        Some(EDIT) => app.edit_list(key),
        Some(DRIVES) => app.set_drive(key, drive.is_none()),
        Some(PUBLIC) => app.set_ships_public(key, !public),
        Some(SUGGEST) => {
            if let Some(s) = suggested {
                app.take_suggestion(key, s);
            }
        }
        Some(CHECKS) => app.approve_commands(key, &unapproved),
        Some(ORCHESTRATOR) => app.set_orchestrator(key, !board.orchestrator),
        Some(i) if i > PARALLEL => app.set_parallel(key, i - PARALLEL),
        Some(i) => {
            if let Some(m) = Mode::ALL.get(i - 1) {
                app.set_mode(key, *m);
            }
        }
        None => {}
    });
}

/// Asks before Warriv may cut public releases of a project by itself,
/// which every install is offered the moment one is published.
pub(super) fn ships_public(key: &str) -> bool {
    let pressed = super::ask(&crate::dialog::Dialog {
        tone: crate::dialog::Tone::Warning,
        title: "Warriv ships public",
        text: &format!(
            "While it drives {}, Warriv may cut public releases by itself, and every \
             install is offered each one. Let it?",
            project_name(key)
        ),
        buttons: &["Let it ship public", "Cancel"],
        default: 1,
        check: None,
    });
    pressed == Some(0)
}

/// A line of the mode menu that says how many items run side by side.
fn at_once(n: usize) -> String {
    match n {
        1 => "One at a time".into(),
        n => format!("{n} at once, each in its own worktree"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_is_kept_by_project_and_by_the_quest_edited() {
        assert_ne!(draft_for("a", None), draft_for("b", None));
        assert_ne!(draft_for("a", None), draft_for("a", Some((3, "Fix"))));
        assert_ne!(
            draft_for("a", Some((3, "Fix"))),
            draft_for("a", Some((3, "Fix it")))
        );
        assert_ne!(
            draft_for("a", Some((3, "Fix"))),
            draft_for("a", Some((4, "Fix")))
        );
    }

    #[test]
    fn only_a_draft_that_changed_something_is_kept() {
        let d = |text: &str, notes: &str| ask::Draft {
            text: text.into(),
            notes: notes.into(),
        };
        assert!(!worth_keeping(&d("", ""), "", ""));
        assert!(worth_keeping(&d("Fix", ""), "", ""));
        assert!(worth_keeping(&d("", "why"), "", ""));
        assert!(!worth_keeping(&d("Fix", "why"), "Fix", "why"));
        assert!(worth_keeping(&d("Fix", "why not"), "Fix", "why"));
    }

    #[test]
    fn the_menu_says_how_many_run_at_once() {
        assert_eq!(at_once(1), "One at a time");
        assert_eq!(at_once(3), "3 at once, each in its own worktree");
        assert!(AT_ONCE.iter().all(|&n| n <= tasks::MOST_PARALLEL));
    }

    #[test]
    fn a_briefing_gives_the_title_then_the_notes() {
        let mut t = Task {
            line: 3,
            mark: Mark::Open,
            title: "Fix  the	clock".into(),
            holder: None,
            reason: None,
            wait: None,
            since: None,
            ask: None,
            notes: Vec::new(),
        };
        assert_eq!(
            briefing(&t),
            "Fix the clock

No notes. The agent gets only the title."
        );
        t.notes = vec!["It runs fast.".into(), "See main.rs".into()];
        assert_eq!(
            briefing(&t),
            "Fix the clock

It runs fast.
See main.rs"
        );
    }

    #[test]
    fn the_merge_question_names_the_item_and_both_branches() {
        let q = merge_question("Fix the\nlogin", "fix-the-login", "main");
        assert!(q.starts_with("\"Fix the login\" is finished on the branch fix-the-login."));
        assert!(q.ends_with(
            "\n\nMerge it into main? A conflict undoes the merge and keeps the branch."
        ));
    }
}
