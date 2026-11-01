//! The quest log runner and Warriv, for both apps. The logic is
//! [`runner::Runner`] and [`warriv::Warriv`], traits every [`Host`] has by
//! a blanket impl, so the Windows app and the Mac app run one runner
//! rather than two that drift apart. What each app gives them, its
//! sessions, its notifications and the state it draws, is [`Host`].
//!
//! The menus and dialogs that drive a quest log are each app's own.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use horadric_core::warriv::Drive;
use horadric_core::worktree::Worktree;
use horadric_core::{Registry, Usage};

use crate::board::{Board, Ink};

pub(crate) mod runner;
pub(crate) mod warriv;

pub(crate) use runner::{Menu, Runner, State};
// Only the Windows stones name it; the Mac's menu goes through `Menu`.
#[cfg(windows)]
pub(crate) use runner::Merge;
pub(crate) use warriv::Warriv;

/// How a notification looks, as each app shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toast {
    /// A session or a quest waits on you.
    Waiting,
    /// Something finished or went through.
    Done,
    Info,
    Failed,
}

/// Wakes the app's main thread from another, to hear what a thread left
/// in the runner's state: a kept branch or a landing.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the runner and Warriv need from the app they run in.
pub(crate) trait Host: Sized {
    /// What the runner keeps between looks.
    fn tasks(&self) -> &State;
    fn tasks_mut(&mut self) -> &mut State;
    /// The projects Warriv drives, by key, saved with the app.
    fn drives(&self) -> &BTreeMap<String, Drive>;
    fn drives_mut(&mut self) -> &mut BTreeMap<String, Drive>;
    /// The projects whose drive the human stopped, which start nothing.
    fn stopped(&self) -> &HashSet<String>;
    fn stopped_mut(&mut self) -> &mut HashSet<String>;
    /// The projects the human closed, whose lists the runner leaves.
    fn closed(&self) -> &HashSet<String>;
    /// The recent folders, whose lists are read with no session in them.
    fn recent(&self) -> &[String];
    /// The keys of the tiles shown, whose lists are read even with no
    /// session in them and their folder gone from the recent list.
    fn tiles(&self) -> Vec<String>;

    /// What the hooks say of each session.
    fn registry(&self) -> &Arc<Mutex<Registry>>;
    /// The account's usage as the status line last reported it.
    fn usage(&self) -> &Arc<Mutex<Option<Usage>>>;
    /// Each project's list as last read, by key, which the tiles draw.
    fn boards(&self) -> &RefCell<HashMap<String, Board>>;
    /// The quests Warriv has in hand, by project, drawn as its.
    fn warriv_shown(&self) -> &RefCell<HashMap<String, BTreeSet<String>>>;
    /// The projects whose Warriv is awake.
    fn astir(&self) -> &RefCell<HashSet<String>>;
    /// Warriv's line on each project's quests tile.
    fn warriv_line(&self) -> &RefCell<HashMap<String, (String, Ink)>>;

    /// The app is quitting or reloading, and the runner starts nothing.
    fn holding_still(&self) -> bool;
    /// The human asked for no notifications.
    fn quiet(&self) -> bool;
    /// Seconds since local midnight, for Warriv's watch hours.
    fn local_secs(&self) -> u64;
    /// New sessions open as a Folio rather than a terminal.
    fn chat_sessions(&self) -> bool;
    /// What an agent is told about the browser pane, when it has one.
    fn browser_prompt(&self) -> Option<String>;

    /// The folder of the project `key`, its main tree.
    fn project_dir(&self, key: &str) -> Option<PathBuf>;
    /// The project session `id` is shown in, by key.
    fn project_of(&self, id: &str) -> Option<String>;
    /// An id like `base` that no session has.
    fn unique_id(&self, base: &str) -> String;
    /// Session `id` runs in this app, its terminal or Folio here and live.
    fn runs_here(&self, id: &str) -> bool;
    /// Session `id`'s program has exited.
    fn exited(&self, id: &str) -> bool;
    /// Starts a session, its id once it runs.
    fn start_session(&mut self, spec: SessionSpec) -> Result<String, String>;
    /// Types `text` into session `id` between turns. False when it could
    /// not, which never starts an agent.
    fn send_prompt(&mut self, id: &str, text: &str) -> bool;
    /// The Enter of a prompt typed a moment ago, for a terminal that took
    /// the text as typed.
    fn press_enter(&mut self, id: &str);
    /// Ends session `id`, its tile and all.
    fn end(&mut self, id: &str);
    /// Lets go of session `id` once its work is done: it ends.
    fn forget(&mut self, id: &str);
    /// Shows session `id` on the stage.
    fn reveal(&mut self, id: &str);
    /// Brings the project `key` onto the stage with session `id` typing.
    fn show_session(&mut self, key: &str, id: &str);
    /// Makes the windows match the registry and the lists.
    fn reconcile(&mut self);
    fn save(&mut self);

    /// Tells the human. The runner asks [`Host::quiet`] first where a
    /// quiet app should not hear it.
    fn toast(&mut self, kind: Toast, title: &str, text: &str);
    /// Tells the human of something a click on the notification answers:
    /// [`State::merge_for`] or [`State::ship_for`], set just after.
    fn toast_click(&mut self, kind: Toast, title: &str, text: &str);
    fn sound(&self, loot: crate::loot::Loot);
    /// The lists of these projects changed, so their tiles may need
    /// another size.
    fn lists_changed(&mut self, keys: &[String]);
    /// Every tile draws again, for a landed branch's gold.
    fn redraw_tiles(&self);
    fn waker(&self) -> Waker;
    /// Shows a menu or a dialog the runner asks for, once the app is free.
    fn ask(&mut self, menu: Menu);
    /// Carries on the conversation of a quest's gone session `holder`.
    fn carry_on(&self, key: &str, holder: &str);
    /// Opens the quest log `rel` of the project at `dir` in an editor.
    fn open_list(&self, dir: &Path, rel: &str);
    /// Starts memory recall for a quest about to start, true once it has
    /// answered. An app with no memory answers at once.
    fn recall_ready(&self, _dir: &Path, _query: &str) -> bool {
        true
    }
    /// What memory recalled for a quest, as [`Host::recall_ready`] asked.
    fn take_recall(&self, _dir: &Path, _query: &str) -> Option<String> {
        None
    }
    /// Lights the files a quest's context pack gave session `id`.
    fn light_pack(&self, _key: &str, _id: &str, _files: &[String]) {}

    // Runewords and errands, which only Windows has so far.
    fn has_stone(&self, _key: &str, _label: &str) -> bool {
        false
    }
    fn errand_bypasses(&self, _id: &str) -> bool {
        false
    }
    /// The project and the stone of errand session `id`.
    fn errand_of_session(&self, _id: &str) -> Option<(String, String)> {
        None
    }
    fn errand_event(&mut self, _key: Option<&str>, _event: horadric_core::runeword::Event) {}
    fn halt_errands(&mut self, _key: &str) {}
    /// Once a second, with the runner's look: the stones and errands.
    fn tick_stones(&mut self) {}
}

/// Where a new session works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workplace {
    /// In the folder it is given.
    Here,
    /// In a worktree of its own unless the project works in one tree, as
    /// a session the human starts does.
    Project,
    /// In a worktree of its own wherever there is a repository.
    OwnTree,
}

/// A Claude session to start with [`Host::start_session`].
pub(crate) struct SessionSpec {
    /// A new one when None. A tile's id carries that tile on.
    pub id: Option<String>,
    /// What its tile is called, the folder's name when None. A tile carried
    /// on keeps its own.
    pub name: Option<String>,
    pub dir: PathBuf,
    /// Its own arguments, as a terminal's command line holds them.
    pub args: Vec<String>,
    /// The conversation it carries on.
    pub resume: Option<String>,
    /// Its first prompt, a quest's or an errand's first step.
    pub prompt: Option<String>,
    pub place: Workplace,
    /// The branch of a worktree of its own, the name when None.
    pub branch: Option<String>,
    /// A Folio rather than a terminal.
    pub chat: bool,
    /// Shown on the stage with the keyboard.
    pub show: bool,
    /// Started by automation, which tells of a failure its own way: no
    /// dialog waits for a human who may be away, once per retry.
    pub quiet: bool,
}

impl SessionSpec {
    /// A new session in `dir`, a Folio when `chat`, with nothing more.
    pub(crate) fn new(dir: PathBuf, chat: bool) -> SessionSpec {
        SessionSpec {
            id: None,
            name: None,
            dir,
            args: Vec::new(),
            resume: None,
            prompt: None,
            place: Workplace::Here,
            branch: None,
            chat,
            show: false,
            quiet: false,
        }
    }
}

/// The environment session `id` in `cwd` gets beside its tag, terminal or
/// Folio: its worktree's ports, and which quest log `horadric quest` works
/// on.
pub(crate) fn session_env(
    id: &str,
    own_tree: Option<&Worktree>,
    cwd: &Path,
) -> Vec<(String, String)> {
    own_tree
        .map(|w| {
            let mut env = w
                .ports
                .map(horadric_core::worktree::env)
                .unwrap_or_default();
            // `horadric quest` finds the list in the main tree.
            env.push((
                horadric_hooks::TASKS_ENV.into(),
                crate::project::folder_key(&cwd.to_string_lossy()),
            ));
            env
        })
        // Empty, so one this Horadric was started with, as a dev instance
        // from a quest's worktree is, never sends `horadric quest` to
        // another project's list. Warriv works on its own project's list
        // from wherever its shell has wandered.
        .unwrap_or_else(|| {
            let main = match horadric_core::warriv::is_warriv(id) {
                true => crate::project::folder_key(&cwd.to_string_lossy()),
                false => String::new(),
            };
            vec![(horadric_hooks::TASKS_ENV.into(), main)]
        })
}
