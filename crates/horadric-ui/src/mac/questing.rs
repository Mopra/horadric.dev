//! The quest log on the Mac: what the app gives the runner and Warriv
//! shared with Windows ([`crate::quests`]), quest sessions started in a
//! worktree of their own, and the project menu's Quest log, where the Mac
//! drives a list until it has a quests tile.
//!
//! Quests run in terminals here, not Folios: the Mac's Folio has no slash
//! commands or sign in card yet, and a quest started while nobody watches
//! must not wait on one. Stones and errands stay Windows only, so the
//! [`Host`] defaults for them stand.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use horadric_core::agent::Agent;
use horadric_core::tasks::{self, Mark, Mode};
use horadric_core::warriv::Drive;
use horadric_core::worktree::{Ports, Worktree};
use horadric_core::{Registry, SavedState, Usage};
use objc2_app_kit::NSApplication;

use super::{App, Run};
use crate::board::{Board, Ink, RowState};
use crate::mac::dialog;
use crate::mac::menu::Item;
use crate::quests::runner::{
    approve_checks_label, approve_text, orchestrator_label, suggestion, suggestion_label,
    unapproved_wait,
};
use crate::quests::{self, Host, Menu, Runner, SessionSpec, Toast, Warriv, Workplace};
use horadric_hooks::tasks as file;
use crate::{console, store};

/// How many of the runner's words the menu keeps.
const TOLD: usize = 5;
/// The most quests a project's menu lists, so a long log still fits.
const LISTED: usize = 30;
/// Tags in the project menu from here are the Quest log's.
pub(super) const QUEST_TAG: usize = 1000;
/// Per listed quest, room for its actions.
const PER_QUEST: usize = 8;
const ACCEPT: usize = 1;
const SHOW: usize = 2;
const DONE: usize = 3;
const BACK: usize = 4;
const UP: usize = 5;
const DOWN: usize = 6;
const RUN: usize = 7;
// Tags past the quests, by what they do.
const ADD: usize = QUEST_TAG + LISTED * PER_QUEST;
const GIVE: usize = ADD + 1;
const EDIT: usize = ADD + 2;
const DRIVES: usize = ADD + 3;
const PUBLIC: usize = ADD + 4;
const MERGE: usize = ADD + 5;
const SUGGEST: usize = ADD + 6;
const CHECKS: usize = ADD + 7;
const ORCHESTRATOR: usize = ADD + 8;
const MODE: usize = ADD + 10;
const PARALLEL: usize = ADD + 20;
const AT_ONCE: [usize; 5] = [1, 2, 4, 8, 16];

/// What the Mac keeps for its quest logs: the runner's own state, Warriv's
/// drives, and what a quests tile would draw.
#[derive(Default)]
pub(super) struct Quests {
    state: quests::State,
    drives: BTreeMap<String, Drive>,
    stopped: HashSet<String>,
    /// Always empty: a Mac cluster goes with its last session, so there is
    /// no closed project whose list the runner should leave.
    closed: HashSet<String>,
    boards: RefCell<HashMap<String, Board>>,
    warriv: RefCell<HashMap<String, BTreeSet<String>>>,
    astir: RefCell<HashSet<String>>,
    warriv_line: RefCell<HashMap<String, (String, Ink)>>,
    /// Worktrees just added for sessions about to launch, with their
    /// setup commands, by session.
    new_trees: HashMap<String, (Worktree, Vec<String>)>,
    /// What the runner told lately, newest first. The Mac has no
    /// notifications of its own yet, so the menu shows them.
    told: VecDeque<String>,
}

impl Quests {
    pub(super) fn new(saved: &SavedState) -> Quests {
        Quests {
            state: quests::State::after(&saved.warriv_heard),
            drives: saved.drives.clone(),
            stopped: saved.stopped.iter().cloned().collect(),
            ..Quests::default()
        }
    }

    /// Writes what the next run carries on into `s`.
    pub(super) fn keep(&self, s: &mut SavedState) {
        s.drives = self.drives.clone();
        s.stopped = self.stopped.iter().cloned().collect();
        s.stopped.sort();
        s.warriv_heard = self.state.warriv.heard();
    }

    /// The worktree added for session `id`, taken by its launch.
    pub(super) fn take_tree(&mut self, id: &str) -> Option<(Worktree, Vec<String>)> {
        self.new_trees.remove(id)
    }
}

impl Host for App {
    fn tasks(&self) -> &quests::State {
        &self.quests.state
    }
    fn tasks_mut(&mut self) -> &mut quests::State {
        &mut self.quests.state
    }
    fn drives(&self) -> &BTreeMap<String, Drive> {
        &self.quests.drives
    }
    fn drives_mut(&mut self) -> &mut BTreeMap<String, Drive> {
        &mut self.quests.drives
    }
    fn stopped(&self) -> &HashSet<String> {
        &self.quests.stopped
    }
    fn stopped_mut(&mut self) -> &mut HashSet<String> {
        &mut self.quests.stopped
    }
    fn closed(&self) -> &HashSet<String> {
        &self.quests.closed
    }
    fn recent(&self) -> &[String] {
        &self.recent
    }
    fn tiles(&self) -> Vec<String> {
        self.clusters.iter().map(|c| c.key.clone()).collect()
    }
    fn registry(&self) -> &Arc<Mutex<Registry>> {
        &self.registry
    }
    fn usage(&self) -> &Arc<Mutex<Option<Usage>>> {
        &self.usage
    }
    fn boards(&self) -> &RefCell<HashMap<String, Board>> {
        &self.quests.boards
    }
    fn warriv_shown(&self) -> &RefCell<HashMap<String, BTreeSet<String>>> {
        &self.quests.warriv
    }
    fn astir(&self) -> &RefCell<HashSet<String>> {
        &self.quests.astir
    }
    fn warriv_line(&self) -> &RefCell<HashMap<String, (String, Ink)>> {
        &self.quests.warriv_line
    }
    fn holding_still(&self) -> bool {
        self.quit || self.frozen
    }
    fn quiet(&self) -> bool {
        false
    }
    fn local_secs(&self) -> u64 {
        local_secs()
    }
    fn chat_sessions(&self) -> bool {
        false
    }
    fn browser_prompt(&self) -> Option<String> {
        // The pane's flags carry its prompt only once its config is
        // written; Claude Code takes one system prompt, so it goes in the
        // runner's.
        (crate::mac::web::claude_args().iter())
            .any(|a| a.starts_with("--append-system-prompt"))
            .then(|| crate::AGENT_PROMPT.to_string())
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
        (self.consoles.get(id)).is_some_and(|c| c.exit_code().is_none())
    }
    fn exited(&self, id: &str) -> bool {
        (self.consoles.get(id)).is_some_and(|c| c.exit_code().is_some())
    }
    fn start_session(&mut self, spec: SessionSpec) -> Result<String, String> {
        self.start_quest_session(spec)
    }
    fn send_prompt(&mut self, id: &str, text: &str) -> bool {
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        if c.folio.is_some() {
            return crate::mac::folio::prompt(id, text);
        }
        // The Enter comes a moment later, so the agent takes the text as
        // typed rather than as one paste with a newline.
        c.write(text.as_bytes().to_vec());
        let at = std::time::Instant::now();
        self.quests.state.enters.push((id.to_string(), at));
        true
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
        if let Some(key) = App::project_of(self, id) {
            self.show_project(&key, Some(id));
        }
    }
    fn show_session(&mut self, key: &str, id: &str) {
        self.show_project(key, Some(id));
    }
    fn reconcile(&mut self) {
        App::reconcile(self)
    }
    fn save(&mut self) {
        App::save(self)
    }
    fn toast(&mut self, kind: Toast, title: &str, text: &str) {
        dialog::notify(title, text);
        let line = match text.lines().next() {
            Some(first) if !first.is_empty() => format!("{title}: {first}"),
            _ => title.to_string(),
        };
        self.quests.told.push_front(line);
        self.quests.told.truncate(TOLD);
        if matches!(kind, Toast::Waiting | Toast::Failed) {
            let app = NSApplication::sharedApplication(self.mtm);
            if !app.isActive() {
                app.requestUserAttention(
                    objc2_app_kit::NSRequestUserAttentionType::InformationalRequest,
                );
            }
        }
    }
    fn toast_click(&mut self, kind: Toast, title: &str, text: &str) {
        // No notification to click here: the merge it offers is in the
        // project's Quest log menu.
        self.toast(kind, title, text);
    }
    fn sound(&self, _loot: crate::loot::Loot) {}
    fn lists_changed(&mut self, _keys: &[String]) {}
    fn redraw_tiles(&self) {
        for c in &self.clusters {
            c.redraw();
        }
    }
    fn waker(&self) -> quests::Waker {
        // The tick hears it within a second too, should the app be busy.
        Arc::new(|| {
            crate::mac::queue::post(|| {
                let _ = super::with(|app| app.offer_merges());
            })
        })
    }
    fn ask(&mut self, menu: Menu) {
        self.quests.state.menu = Some(menu);
        crate::mac::queue::post(|| {
            let _ = super::with(App::show_asked);
        });
    }
    fn carry_on(&self, _key: &str, holder: &str) {
        // A quest's session gone here is resumed in its tile when it was
        // paused; otherwise its conversation stays in Claude Code's history.
        let holder = holder.to_string();
        crate::mac::queue::post(move || {
            let _ = super::with(|app| {
                if let Err(e) = app.resume(&holder, true) {
                    dialog::error("Horadric could not carry the quest on", &e);
                }
            });
        });
    }
    fn open_list(&self, dir: &Path, rel: &str) {
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(dir.join(rel))
            .spawn();
    }
}

impl App {
    /// Once a second, with the app's tick: the runner's look, and what its
    /// threads left.
    pub(super) fn tick_quests(&mut self) {
        self.tick_tasks();
        self.offer_merges();
        self.show_asked();
    }

    /// `horadric quest` changed a list, or told something through it.
    pub(super) fn quest_command(&mut self, t: &horadric_hooks::listener::TasksChanged) {
        if let (Some(quest), Some(tell)) = (&t.quest, &t.tell) {
            self.hear_tell(&t.dir, quest, tell, t.by.as_deref());
        }
        if let (Some(quest), Some(fix)) = (&t.quest, &t.fix) {
            self.hear_fix(&t.dir, quest, fix);
        }
        if let (Some(quest), Some(overlap)) = (&t.quest, &t.overlap) {
            self.overlap_event(Path::new(&t.dir), quest, overlap.clone());
        }
        self.refresh_boards(true);
        self.run_tasks();
    }

    /// Starts a session the runner or Warriv asked for, in a terminal,
    /// in a worktree of its own when its place says so.
    fn start_quest_session(&mut self, spec: SessionSpec) -> Result<String, String> {
        let SessionSpec {
            id,
            name,
            dir,
            args,
            resume,
            prompt,
            place,
            branch,
            show,
            ..
        } = spec;
        if !dir.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        let folder = (dir.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let id = id.unwrap_or_else(|| App::unique_id(self, name.as_deref().unwrap_or(&folder)));
        let known = (self.registry.lock().ok()).and_then(|r| r.get(&id).map(|s| s.name.clone()));
        let existed = known.is_some();
        let branch = branch.or_else(|| name.clone()).unwrap_or("session".into());
        let name = known.or(name).unwrap_or(folder);
        let own = Agent::Claude.carry_on(resume.as_deref(), &args);
        let dir = match place {
            Workplace::Project | Workplace::OwnTree if !existed && resume.is_none() => {
                self.own_tree(&id, &branch, dir, &own, place == Workplace::OwnTree)
            }
            _ => dir,
        };
        if let Some(prompt) = prompt {
            self.quests.state.prompts.insert(id.clone(), prompt);
        }
        let started = self.launch(&id, &name, dir, own, Run::Agent(Agent::Claude));
        if let Err(e) = started {
            self.quests.state.prompts.remove(&id);
            // A worktree the session never started in holds nothing.
            if let Some((w, _)) = self.quests.new_trees.remove(&id) {
                crate::worktree::remove(w);
            }
            return Err(e);
        }
        if show {
            if let Some(key) = App::project_of(self, &id) {
                self.show_project(&key, Some(&id));
            }
        }
        Ok(id)
    }

    /// Where a new session starts: a worktree of its own added from `cwd`,
    /// or `cwd` itself when the project works in one tree and none was
    /// `asked` for, or it is in no repository.
    fn own_tree(
        &mut self,
        id: &str,
        branch: &str,
        cwd: PathBuf,
        args: &[String],
        asked: bool,
    ) -> PathBuf {
        if Agent::Claude.carries_on(args) {
            return cwd;
        }
        let taken: Vec<Ports> = (self.registry.lock())
            .map(|r| r.all().filter_map(|s| s.worktree.as_ref()?.ports).collect())
            .unwrap_or_default();
        match crate::worktree::add(&cwd, branch, &taken, asked) {
            Ok(Some(fresh)) => {
                // The project is where it was asked for, not the worktree.
                self.remember(&cwd);
                (self.quests.new_trees).insert(id.to_string(), (fresh.worktree, fresh.setup));
                fresh.cwd
            }
            Ok(None) => cwd,
            Err(e) => {
                eprintln!("horadric: no worktree for {id}, it shares the main tree: {e}");
                cwd
            }
        }
    }

    /// What goes before a Claude Code session's own `args` this time, as
    /// Windows gives it: the defaults (prompts bypassed for a quest in its
    /// own worktree), the quest's model, the status line and a quest's lean
    /// connectors in `--settings`, the browser pane's tools, what the
    /// runner tells it, and last its first prompt, which is positional.
    pub(super) fn claude_extra(
        &mut self,
        id: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Vec<String> {
        if !console::is_claude(program) {
            // A shell put in its place with `HORADRIC_AGENT`.
            self.record_agent_launch(id, "other");
            return Vec::new();
        }
        let bypass = self.bypasses_prompts(id);
        // The quest's model counts as the session's own choice.
        let model = self.quest_model_args(id, args);
        let own: Vec<String> = args.iter().chain(&model).cloned().collect();
        let mut extra = self.defaults.flags_for(&own, bypass);
        extra.extend(model);
        if !args.iter().any(|a| a == "--settings") {
            let settings = match self.quest_lean(id).filter(|l| l != &serde_json::json!({})) {
                Some(lean) => {
                    store::write_session_settings(id, self.status_settings.as_deref(), &lean)
                        .or_else(|| self.status_settings.clone())
                }
                None => self.status_settings.clone(),
            };
            if let Some(path) = settings {
                extra.extend(["--settings".into(), path.to_string_lossy().into_owned()]);
            }
        }
        let mut web = crate::mac::web::claude_args();
        // Its prompt goes in the runner's, which `task_args` gives.
        web.retain(|a| !a.starts_with("--append-system-prompt"));
        extra.extend(web);
        extra.extend(self.task_args(id, program, cwd));
        self.record_launch(id, args, &extra);
        if let Some(prompt) = self.quests.state.prompts.remove(id) {
            extra.push(prompt);
        }
        extra
    }

    /// Shows the dialog the runner asked for, once the app is free.
    fn show_asked(&mut self) {
        let Some(asked) = self.quests.state.menu.take() else {
            return;
        };
        match asked {
            Menu::Merge(m) => {
                let into = crate::worktree::checked_out(&m.main).unwrap_or_else(|| "main".into());
                let text = format!(
                    "\"{}\" is finished on the branch {}.\n\nMerge it into {into}? A conflict \
                     undoes the merge and keeps the branch.",
                    tasks::one_line(&m.title),
                    m.branch
                );
                if dialog::confirm("Merge completed quest", &text, "Merge") {
                    self.merge(&m);
                }
            }
            Menu::Brief(key, line, title) => {
                let notes = (self.task_at(&key, line, &title))
                    .map(|t| t.notes.join("\n"))
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "No notes. The agent gets only the title.".into());
                if dialog::confirm(&tasks::one_line(&title), &notes, "Accept") {
                    self.accept_task(&key, line, &title);
                }
            }
            Menu::Add(key) => self.add_quest(&key),
            // The Mac has no quests tile to anchor these, or stones: the
            // project menu's Quest log holds what they would.
            Menu::Item(..) | Menu::Mode(_) | Menu::Ship(_) => {}
        }
    }

    fn add_quest(&mut self, key: &str) {
        let asked = dialog::ask(
            "New quest",
            "What should be done? The agent gets the title as its quest.",
            "",
        );
        if let Some(title) = asked.filter(|t| !t.trim().is_empty()) {
            self.add_task(key, &title, "");
        }
    }

    /// The project menu's Quest log: each quest with what can be done
    /// with it, how the log runs, Warriv's drive, and what the runner said
    /// lately.
    pub(super) fn quest_items(&self, key: &str) -> Vec<Item> {
        let board = self.quests.boards.borrow().get(key).cloned();
        let Some(board) = board else {
            return vec![
                Item::pick("New quest\u{2026}", ADD),
                Item::pick("Give quests", GIVE),
            ];
        };
        let dir = self.project_dir(key);
        let mut items = Vec::new();
        let listed: Vec<_> = (board.tasks.iter())
            .filter(|t| t.mark != Mark::Done || t.holder.is_some())
            .take(LISTED)
            .collect();
        let last = listed.len().saturating_sub(1);
        for (i, t) in listed.iter().enumerate() {
            let base = QUEST_TAG + i * PER_QUEST;
            let state = self.row_state(t);
            let mut sub = Vec::new();
            match state {
                RowState::Open => sub.push(Item::pick("Accept", base + ACCEPT)),
                RowState::Gone => sub.push(Item::pick("Accept again", base + ACCEPT)),
                _ => sub.push(Item::pick("Show session", base + SHOW)),
            }
            match state {
                RowState::Open => {}
                RowState::Review => sub.push(Item::pick("Approve", base + DONE)),
                _ => sub.push(Item::pick("Mark completed", base + DONE)),
            }
            if t.holder.is_some() {
                sub.push(Item::pick("Put back in the log", base + BACK));
            }
            if let Some(c) = dir.as_ref().and_then(|d| unapproved_wait(d, t)) {
                sub.push(Item::disabled(format!(
                    "Waits on a command not approved on this machine: {c}"
                )));
                sub.push(Item::pick("Approve and run", base + RUN));
            }
            if i > 0 {
                sub.push(Item::pick("Move up", base + UP));
            }
            if i < last {
                sub.push(Item::pick("Move down", base + DOWN));
            }
            let title = format!("{} {}", mark_of(state), tasks::one_line(&t.title));
            items.push(Item::Sub(title, sub));
        }
        if listed.is_empty() {
            items.push(Item::disabled("No quests left"));
        }
        items.push(Item::Separator);
        items.push(Item::pick("New quest\u{2026}", ADD));
        items.push(Item::pick("Give quests", GIVE));
        if let Some(m) = &self.quests.state.merge_for {
            items.push(Item::pick(format!("Merge {}\u{2026}", m.branch), MERGE));
        }
        items.push(Item::Separator);
        for (i, m) in Mode::ALL.iter().enumerate() {
            items.push(Item::checked(m.explain(), MODE + i, *m == board.mode));
        }
        let drive = self.drive_of(key);
        if self.quests.stopped.contains(key) {
            items.push(Item::disabled("Warriv was stopped: pick a mode to go on"));
        }
        if let Some(s) = dir.as_ref().and_then(|d| suggestion(d, board.own_trees)) {
            items.push(Item::pick(suggestion_label(s), SUGGEST));
        }
        if let Some(not) = dir.as_ref().and_then(|d| file::checks(d).err()) {
            items.push(Item::pick(approve_checks_label(&not), CHECKS));
        }
        let repo_warriv = dir.as_ref().is_some_and(|d| file::suggested_orchestrator(d));
        items.push(Item::checked(
            orchestrator_label(board.orchestrator, repo_warriv),
            ORCHESTRATOR,
            board.orchestrator,
        ));
        items.push(Item::checked("Warriv drives", DRIVES, drive.is_some()));
        match &drive {
            Some(d) => items.push(Item::checked("and ships public", PUBLIC, d.ships_public)),
            None => items.push(Item::disabled("and ships public")),
        }
        if board.own_trees {
            let at_once = AT_ONCE.iter().map(|&n| {
                let label = match n {
                    1 => "One at a time".to_string(),
                    n => format!("{n} at once, each in its own worktree"),
                };
                Item::checked(label, PARALLEL + n, n == board.parallel.max(1))
            });
            items.push(Item::Sub("At once".into(), at_once.collect()));
        }
        items.push(Item::Separator);
        items.push(Item::pick("Edit the quest log", EDIT));
        if !self.quests.told.is_empty() {
            items.push(Item::Separator);
            items.extend(self.quests.told.iter().map(|t| Item::disabled(t.clone())));
        }
        items
    }

    /// A tag from [`App::quest_items`] picked.
    pub(super) fn quest_pick(&mut self, key: &str, tag: usize) {
        match tag {
            ADD => self.add_quest(key),
            GIVE => self.give_quests(key),
            EDIT => self.edit_list(key),
            MERGE => {
                if let Some(m) = self.quests.state.merge_for.clone() {
                    self.ask(Menu::Merge(m));
                }
            }
            ORCHESTRATOR => {
                let on = self.quests.boards.borrow().get(key).is_some_and(|b| b.orchestrator);
                self.set_orchestrator(key, !on);
            }
            DRIVES => {
                let on = self.drive_of(key).is_none();
                self.set_drive(key, on);
            }
            PUBLIC => {
                let public = self.drive_of(key).is_some_and(|d| d.ships_public);
                let ok = public
                    || dialog::confirm(
                        "Warriv ships public",
                        &format!(
                            "While it drives {}, Warriv may cut public releases by itself, \
                             and every install is offered each one. Let it?",
                            crate::project::project_name(key)
                        ),
                        "Let it ship public",
                    );
                if ok {
                    self.set_ships_public(key, !public);
                }
            }
            SUGGEST => {
                let dir = self.project_dir(key);
                let own = self.quests.boards.borrow().get(key).is_some_and(|b| b.own_trees);
                if let Some(s) = dir.and_then(|d| suggestion(&d, own)) {
                    self.take_suggestion(key, s);
                }
            }
            CHECKS => {
                let not = self.project_dir(key).and_then(|d| file::checks(&d).err());
                if let Some(not) = not.filter(|n| confirm_approve(true, n)) {
                    self.approve_commands(key, &not);
                }
            }
            t if t >= PARALLEL => self.set_parallel(key, t - PARALLEL),
            t if t >= MODE => {
                if let Some(m) = Mode::ALL.get(t - MODE) {
                    self.set_mode(key, *m);
                }
            }
            t if (QUEST_TAG..ADD).contains(&t) => self.quest_action(key, t - QUEST_TAG),
            _ => {}
        }
    }

    fn quest_action(&mut self, key: &str, at: usize) {
        let (i, action) = (at / PER_QUEST, at % PER_QUEST);
        let board = self.quests.boards.borrow().get(key).cloned();
        let Some(t) = board.and_then(|b| {
            (b.tasks.into_iter())
                .filter(|t| t.mark != Mark::Done || t.holder.is_some())
                .nth(i)
        }) else {
            return;
        };
        let (line, title) = (t.line, t.title.clone());
        match action {
            ACCEPT => self.accept_task(key, line, &title),
            SHOW => self.show_quest(key, &t),
            DONE => self.set_task(key, line, &title, Mark::Done),
            BACK => self.put_back(key, line, &title),
            UP => self.change_list(key, |text| tasks::shift(text, line, &title, true)),
            DOWN => self.change_list(key, |text| tasks::shift(text, line, &title, false)),
            RUN => {
                let dir = self.project_dir(key);
                let c = dir.and_then(|d| unapproved_wait(&d, &t).map(str::to_string));
                if let Some(c) = c.map(|c| vec![c]).filter(|c| confirm_approve(false, c)) {
                    self.approve_commands(key, &c);
                }
            }
            _ => {}
        }
    }
}

/// Whether the human approves running `commands`, which came with the
/// repository. Shown whole, and Return cancels: this is the consent.
fn confirm_approve(checks: bool, commands: &[String]) -> bool {
    let title = if checks { "Approve checks" } else { "Approve and run" };
    dialog::choose(title, &approve_text(checks, commands), "Approve", "Cancel")
}

/// How a quest stands, as a mark before its title in the menu.
fn mark_of(state: RowState) -> &'static str {
    match state {
        RowState::Open => "\u{25CB}",
        RowState::Review => "\u{2713}",
        RowState::Gone => "\u{2205}",
        RowState::Blocked | RowState::Waits => "\u{29D7}",
        _ => "\u{25CF}",
    }
}

/// Seconds since local midnight.
fn local_secs() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: `localtime_r` fills the `tm` it is given and keeps nothing.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return 0;
    }
    (tm.tm_hour as u64) * 3600 + (tm.tm_min as u64) * 60 + tm.tm_sec as u64
}
