//! The composer's menus, without any window: the `/` list, `@` files, and
//! the model, effort and mode pickers, with the fuzzy match that filters
//! them as you type.

use horadric_core::folio::SessionInfo;

/// Commands the Folio cannot run: they draw their own screens in the
/// terminal. They show in the `/` list so they are not missed, marked.
pub const TERMINAL_ONLY: &[&str] = &[
    "login",
    "logout",
    "config",
    "theme",
    "doctor",
    "ide",
    "terminal-setup",
    "plugin",
    "agents",
    "hooks",
    "permissions",
];

/// Shown in a menu at most; the rest are a keystroke away.
pub const SHOWN: usize = 8;
/// Files matched at most, so a huge tree stays quick to filter.
const FILES_MAX: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Slash,
    Files,
    Model,
    Effort,
    Mode,
    /// The background tasks running now; picking one does nothing.
    Tasks,
}

impl MenuKind {
    /// Opened from a pill under the text rather than by typing.
    pub fn is_picker(self) -> bool {
        matches!(
            self,
            MenuKind::Model | MenuKind::Effort | MenuKind::Mode | MenuKind::Tasks
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    /// What follows the label in faint, such as a command's arguments.
    pub hint: String,
    /// What picking it puts in or sends.
    pub value: String,
    /// Opens in the terminal only.
    pub terminal: bool,
    /// The one in use now, for the pickers.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub kind: MenuKind,
    pub items: Vec<Item>,
    pub selected: usize,
    /// The first item shown.
    pub top: usize,
    /// Where the `/` or `@` it completes starts in the text.
    pub start: usize,
}

impl Menu {
    pub fn new(kind: MenuKind, items: Vec<Item>, start: usize) -> Menu {
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        let mut m = Menu {
            kind,
            items,
            selected,
            top: 0,
            start,
        };
        m.keep_in_view();
        m
    }

    pub fn up(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + self.items.len() - 1) % self.items.len();
            self.keep_in_view();
        }
    }

    pub fn down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
            self.keep_in_view();
        }
    }

    fn keep_in_view(&mut self) {
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + SHOWN {
            self.top = self.selected + 1 - SHOWN;
        }
    }

    /// Scrolls the list `rows` down (up when negative), as the wheel
    /// does, taking the pick along so Enter never picks what is hidden.
    pub fn scroll(&mut self, rows: isize) {
        let most = self.items.len().saturating_sub(SHOWN);
        self.top = self.top.saturating_add_signed(rows).min(most);
        self.selected = self
            .selected
            .clamp(self.top, (self.top + SHOWN).saturating_sub(1));
    }

    pub fn picked(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    /// The items drawn, with their index.
    pub fn shown(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items.iter().enumerate().skip(self.top).take(SHOWN)
    }
}

/// How well `query` matches `text`, or None when its letters are not all
/// in it, in order. Higher is better: letters at the start of the text or
/// of a word count more, and letters together more than letters apart.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let starts = (0..t.len()).filter(|&i| t[i] == q[0]);
    // Between equal matches the shorter text is the likelier pick.
    let short = (t.len() as i32) / 8;
    starts
        .filter_map(|s| from(&q, &t, s))
        .max()
        .map(|s| s - short)
}

/// The greedy match of `q` in `t` from `start`.
fn from(q: &[char], t: &[char], start: usize) -> Option<i32> {
    let boundary =
        |i: usize| i == 0 || matches!(t[i - 1], '/' | '\\' | '_' | '-' | '.' | ' ' | ':');
    let mut total = 0;
    let mut at = start;
    let mut last: Option<usize> = None;
    for &c in q {
        let i = (at..t.len()).find(|&i| t[i] == c)?;
        total += 1;
        if i == 0 {
            total += 15;
        } else if boundary(i) {
            total += 10;
        }
        match last {
            Some(l) if l + 1 == i => total += 8,
            Some(l) => total -= ((i - l - 1) as i32).min(6),
            None => total -= (i as i32).min(10) / 2,
        }
        last = Some(i);
        at = i + 1;
    }
    Some(total)
}

/// `items` that match `query`, best first, the original order breaking
/// ties. `score_of` scores one item against the query.
pub fn rank<T>(query: &str, items: &[T], score_of: impl Fn(&T, &str) -> Option<i32>) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| score_of(it, query).map(|s| (s, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// The better of matching all of `text` and matching `strong`, a part of
/// it that counts more, such as a file's name in its path.
pub fn best(query: &str, text: &str, strong: Option<&str>) -> Option<i32> {
    let weak = score(query, text);
    let strong = strong.and_then(|s| score(query, s)).map(|s| s + 20);
    weak.max(strong)
}

/// The `/` list for what follows the slash: the session's commands,
/// skills and agents, and the terminal only commands, fuzzy filtered.
pub fn slash_items(info: &SessionInfo, typed: &str) -> Vec<Item> {
    let mut all: Vec<Item> = info
        .slash_commands_for("")
        .into_iter()
        .map(|c| Item {
            terminal: TERMINAL_ONLY.contains(&c.name.as_str()),
            detail: if c.description.is_empty() && c.skill {
                "Skill".to_string()
            } else {
                c.description
            },
            hint: c.argument_hint,
            value: c.name.clone(),
            label: format!("/{}", c.name),
            current: false,
        })
        .collect();
    for a in &info.agents {
        if !all.iter().any(|i| i.value == *a) {
            all.push(Item {
                label: format!("@agent-{a}"),
                detail: "Agent".to_string(),
                value: format!("@agent-{a}"),
                ..Default::default()
            });
        }
    }
    for name in TERMINAL_ONLY {
        if !all.iter().any(|i| i.value == *name) {
            all.push(Item {
                label: format!("/{name}"),
                value: name.to_string(),
                terminal: true,
                ..Default::default()
            });
        }
    }
    for i in all.iter_mut().filter(|i| i.terminal) {
        i.detail = "Opens in terminal".to_string();
    }
    let order = rank(typed, &all, |i, q| best(q, &i.label, Some(&i.value)));
    order.into_iter().map(|i| all[i].clone()).collect()
}

/// The `@` list: project files and the folders they are in, fuzzy
/// filtered, the name of a file counting more than its folders.
pub fn file_items(files: &[String], typed: &str) -> Vec<Item> {
    let order = rank(typed, files, |p, q| best(q, p, Some(name_of(p))));
    order
        .into_iter()
        .take(FILES_MAX)
        .map(|i| {
            let p = &files[i];
            let folder = if p.ends_with('/') { "/" } else { "" };
            Item {
                label: format!("{}{folder}", name_of(p)),
                detail: p.clone(),
                value: p.clone(),
                ..Default::default()
            }
        })
        .collect()
}

fn name_of(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    p.rsplit('/').next().unwrap_or(p)
}

/// Files as git lists them, with every folder they are in after them, a
/// folder ending in `/`.
pub fn with_folders(files: Vec<String>) -> Vec<String> {
    let mut folders: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        let mut at = 0;
        while let Some(i) = f[at..].find('/') {
            let dir = &f[..at + i + 1];
            if seen.insert(dir.to_string()) {
                folders.push(dir.to_string());
            }
            at += i + 1;
        }
    }
    let mut all = files;
    all.extend(folders);
    all
}

/// How a path goes into a prompt: as it is, or quoted when it has a space.
pub fn mention(path: &str) -> String {
    if path.contains(' ') {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_must_all_be_there_in_order() {
        assert!(score("cmp", "compact").is_some());
        assert!(score("pmc", "compact").is_none());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn a_start_beats_a_middle_and_together_beats_apart() {
        let start = score("com", "compact").unwrap();
        let middle = score("com", "recompile").unwrap();
        assert!(start > middle);
        let together = score("pact", "compact").unwrap();
        let apart = score("pact", "pxxaxxcxxt").unwrap();
        assert!(together > apart);
    }

    #[test]
    fn a_word_start_counts_more() {
        let word = score("m", "folio/mod.rs").unwrap();
        let inside = score("m", "folio/com.rs").unwrap();
        assert!(word > inside);
    }

    #[test]
    fn ranking_keeps_the_order_on_ties() {
        let items = vec!["ab".to_string(), "ab".to_string(), "xyz".to_string()];
        let r = rank("ab", &items, |s, q| best(q, s, None));
        assert_eq!(r, vec![0, 1]);
    }

    #[test]
    fn a_file_s_name_counts_more_than_its_folders() {
        let files = vec![
            "composer/thing.rs".to_string(),
            "src/composer.rs".to_string(),
        ];
        let items = file_items(&files, "composer");
        assert_eq!(items[0].value, "src/composer.rs");
        assert_eq!(items[0].label, "composer.rs");
    }

    #[test]
    fn folders_follow_their_files_once() {
        let all = with_folders(vec!["a/b/c.rs".into(), "a/d.rs".into()]);
        assert_eq!(all, vec!["a/b/c.rs", "a/d.rs", "a/", "a/b/"]);
    }

    #[test]
    fn a_path_with_a_space_is_quoted() {
        assert_eq!(mention("src/a.rs"), "@src/a.rs");
        assert_eq!(mention("my docs/a.md"), "@\"my docs/a.md\"");
    }

    #[test]
    fn the_slash_list_marks_terminal_commands_and_filters() {
        let info = SessionInfo {
            slash_commands: vec!["compact".into(), "login".into(), "review".into()],
            skills: vec!["review".into()],
            agents: vec!["planner".into()],
            ..Default::default()
        };
        let all = slash_items(&info, "");
        let login = all.iter().find(|i| i.value == "login").unwrap();
        assert!(login.terminal);
        assert_eq!(login.detail, "Opens in terminal");
        assert!(all.iter().any(|i| i.value == "theme" && i.terminal));
        assert!(all.iter().any(|i| i.label == "@agent-planner"));
        assert_eq!(
            all.iter().find(|i| i.value == "review").unwrap().detail,
            "Skill"
        );
        let some = slash_items(&info, "cmp");
        assert_eq!(some[0].value, "compact");
        let co = slash_items(&info, "co");
        assert_eq!(co[0].value, "compact");
    }

    #[test]
    fn a_menu_wraps_and_scrolls_with_the_selection() {
        let items: Vec<Item> = (0..12)
            .map(|i| Item {
                label: i.to_string(),
                ..Default::default()
            })
            .collect();
        let mut m = Menu::new(MenuKind::Files, items, 0);
        m.up();
        assert_eq!(m.selected, 11);
        assert_eq!(m.top, 11 + 1 - SHOWN);
        m.down();
        assert_eq!((m.selected, m.top), (0, 0));
    }

    #[test]
    fn a_picker_starts_on_what_is_in_use() {
        let items = vec![
            Item::default(),
            Item {
                current: true,
                ..Default::default()
            },
        ];
        assert_eq!(Menu::new(MenuKind::Model, items, 0).selected, 1);
    }

    #[test]
    fn the_wheel_scrolls_a_menu_and_takes_the_pick_along() {
        let items: Vec<Item> = (0..20)
            .map(|i| Item {
                label: i.to_string(),
                ..Default::default()
            })
            .collect();
        let mut m = Menu::new(MenuKind::Slash, items, 0);
        m.scroll(3);
        assert_eq!((m.top, m.selected), (3, 3));
        m.scroll(100);
        assert_eq!(m.top, 20 - SHOWN);
        m.scroll(-100);
        assert_eq!(m.top, 0);
        assert_eq!(m.selected, SHOWN - 1);
    }
}
