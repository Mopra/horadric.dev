//! The Towns section of the Settings window: add a town, rename, hide or
//! remove one, say what it is for and which connectors its account has,
//! give it a Claude login and a browser of its own. See docs/plan/towns.md.
//!
//! A row's click is taken here, as the Vault's are: most ask something,
//! and a question runs a modal loop, so it waits for a timer outside every
//! borrow. The rows themselves are a pure function of [`View`]s the app
//! hands the window, so they can be tested.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use horadric_core::towns::{self as core, SavedTown};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::app::{self, Input};
use crate::ask;
use crate::dialog::{self, Dialog, Tone};
use crate::settings::{Control, Field, Line};
use crate::town;
use crate::window::Shared;

/// A row of the Towns section. The number is the town's place in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Add,
    Heading(u16),
    Name(u16),
    Shown(u16),
    About(u16),
    Connectors(u16),
    Login(u16),
    Browser(u16),
}

/// A town as its rows show it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct View {
    pub saved: SavedTown,
    /// Its folder, as short as it reads.
    pub folder: String,
    /// None for the projects' login; Some(None) for a home of its own no
    /// one is logged in to yet, Some(Some(email)) for who is.
    pub login: Option<Option<String>>,
    /// An identity picked for the town names the browser profile instead.
    pub identity: Option<String>,
}

/// Every town as its rows show it, read now.
pub fn views() -> Vec<View> {
    town::all()
        .into_iter()
        .map(|t| View {
            folder: t.dir.to_string_lossy().into_owned(),
            login: t.home().map(|h| crate::accounts::email_in(&h)),
            identity: crate::vault::identity::pick(&t.key).filter(|s| !s.is_empty()),
            saved: t.saved,
        })
        .collect()
}

pub fn help(row: Row) -> &'static str {
    match row {
        Row::Add => "A place for work that belongs to no project, work or private",
        Row::Heading(_) => "",
        Row::Name(_) => "Rename it, or remove it and keep its folder",
        Row::Shown(_) => "Its tile, while no session runs there",
        Row::About(_) => "What its sessions are told the town is for",
        Row::Connectors(_) => "The claude.ai connectors its account has, for its CLAUDE.md",
        Row::Login(_) => "A Claude account of its own, beside the projects' one",
        Row::Browser(_) => "Its browser pane keeps logins no other project sees",
    }
}

fn label(row: Row) -> &'static str {
    match row {
        Row::Add => "Add a town",
        Row::Heading(_) => "",
        Row::Name(_) => "Name",
        Row::Shown(_) => "Show its tile",
        Row::About(_) => "For",
        Row::Connectors(_) => "Connectors",
        Row::Login(_) => "Claude login",
        Row::Browser(_) => "Own browser",
    }
}

fn line(row: Row, value: String, control: Control) -> Line {
    let field = Field::Town(row);
    Line {
        field,
        label: label(row),
        value,
        control,
        help: field.help(),
    }
}

/// The section's rows for `towns`.
pub fn lines(towns: &[View]) -> Vec<Line> {
    let switch = |on: bool| (if on { "On" } else { "Off" }.to_string(), Control::Switch(on));
    let mut rows = vec![line(Row::Add, "Add\u{2026}".into(), Control::Button)];
    for (i, v) in towns.iter().enumerate() {
        let n = i as u16;
        let t = &v.saved;
        rows.push(line(Row::Heading(n), t.name.clone(), Control::Note));
        rows.push(line(
            Row::Name(n),
            format!("{}, {}", t.name, short(&v.folder)),
            Control::Button,
        ));
        let (word, c) = switch(!t.hidden);
        rows.push(line(Row::Shown(n), word, c));
        let about = match t.about.trim() {
            "" => "Say what it is for".to_string(),
            a => a.to_string(),
        };
        rows.push(line(Row::About(n), about, Control::Button));
        let connectors = match t.connectors.is_empty() {
            true => "None listed".to_string(),
            false => t.connectors.join(", "),
        };
        rows.push(line(Row::Connectors(n), connectors, Control::Button));
        let login = match &v.login {
            None => "The projects' login, give it its own".to_string(),
            Some(None) => "Its own, not logged in yet, log in".to_string(),
            Some(Some(email)) => email.clone(),
        };
        rows.push(line(Row::Login(n), login, Control::Button));
        rows.push(match &v.identity {
            Some(slug) => line(
                Row::Browser(n),
                format!("Its identity's, {slug}"),
                Control::Fixed,
            ),
            None => {
                let (word, c) = switch(t.own_browser);
                line(Row::Browser(n), word, c)
            }
        });
    }
    rows
}

/// The last two parts of a folder, which is what tells two towns apart.
fn short(folder: &str) -> String {
    let parts: Vec<&str> = folder.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    parts[parts.len().saturating_sub(2)..].join("\\")
}

thread_local! {
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
    static PENDING: Cell<Option<Row>> = const { Cell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// What the questions are drawn with, set as the Settings window opens.
pub fn set_shared(shared: Rc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
}

fn shared() -> Option<Rc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

/// A Towns row clicked. What it asks waits for the app's own message loop.
pub fn click(row: Row) {
    if BUSY.with(Cell::get) {
        return;
    }
    PENDING.with(|p| p.set(Some(row)));
    unsafe {
        SetTimer(None, 0, 0, Some(fire));
    }
}

unsafe extern "system" fn fire(_: HWND, _: u32, id: usize, _: u32) {
    let _ = unsafe { KillTimer(None, id) };
    let Some(row) = PENDING.with(Cell::take) else {
        return;
    };
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    act(row);
    BUSY.with(|b| b.set(false));
    app::push(Input::TownsChanged);
}

fn choose(tone: Tone, title: &str, text: &str, buttons: &[&str]) -> Option<usize> {
    let shared = shared()?;
    dialog::show(
        &shared.gpu,
        &shared.metrics,
        &Dialog {
            tone,
            title,
            text,
            buttons,
            default: 0,
            check: None,
        },
    )
}

fn fail(what: &str, why: &str) {
    choose(Tone::Error, what, why, &["OK"]);
}

fn type_in(title: &str, prompt: &str, initial: &str, placeholder: &str, verb: &str) -> Option<String> {
    let shared = shared()?;
    let question = ask::Ask {
        title,
        prompt,
        initial,
        placeholder,
        verb,
        notes: false,
        secret: false,
        pick: None,
    };
    ask::ask(shared, None, &question).map(|a| a.text)
}

/// Changes the town at `i`, by its slug so a list that moved meanwhile
/// changes the right one.
fn edit(i: u16, f: impl FnOnce(&mut SavedTown)) {
    let Some(slug) = town::saved().get(i as usize).map(|t| t.slug.clone()) else {
        return;
    };
    town::change(|all| {
        if let Some(t) = all.iter_mut().find(|t| t.slug == slug) {
            f(t);
        }
    });
}

fn act(row: Row) {
    let towns = town::all();
    let at = |i: u16| towns.get(i as usize);
    match row {
        Row::Add => add(),
        Row::Heading(_) => {}
        Row::Name(i) => {
            if let Some(t) = at(i) {
                name(i, &t.saved);
            }
        }
        Row::Shown(i) => edit(i, |t| t.hidden = !t.hidden),
        Row::Browser(i) => edit(i, |t| t.own_browser = !t.own_browser),
        Row::About(i) => {
            let Some(t) = at(i) else { return };
            let prompt = format!(
                "What {} is for, in a sentence or two. Its sessions read it in the town's CLAUDE.md.",
                t.saved.name
            );
            if let Some(about) = type_in("What the town is for", &prompt, &t.saved.about, "Work mail, chat and tickets", "save") {
                edit(i, |t| t.about = about.trim().to_string());
            }
        }
        Row::Connectors(i) => {
            let Some(t) = at(i) else { return };
            let prompt = format!(
                "The claude.ai connectors {}'s account has, by commas. Add them on claude.ai for that account; this only tells its agents.",
                t.saved.name
            );
            let now = t.saved.connectors.join(", ");
            if let Some(list) = type_in("Connectors", &prompt, &now, "Microsoft 365, Slack, Atlassian", "save") {
                edit(i, |t| t.connectors = core::split_list(&list));
            }
        }
        Row::Login(i) => {
            if let Some(t) = at(i) {
                login(t);
            }
        }
    }
}

fn add() {
    let Some(name) = type_in(
        "Add a town",
        "A name for a place for work that belongs to no project: Work, Private, a client. It gets a folder, a tile and a browser of its own; its Claude login is set in its rows after.",
        "",
        "Name",
        "add it",
    ) else {
        return;
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    town::change(|all| {
        let taken: Vec<&str> = all.iter().map(|t| t.slug.as_str()).collect();
        let slug = core::slug_for(&name, &taken);
        all.push(SavedTown {
            name,
            slug,
            own_browser: true,
            ..Default::default()
        });
    });
}

fn name(i: u16, t: &SavedTown) {
    // Tristram stays one of the towns, so it can be renamed but not removed.
    let buttons: &[&str] = match t.slug == core::TRISTRAM {
        true => &["Rename", "Cancel"],
        false => &["Rename", "Remove", "Cancel"],
    };
    let text = format!(
        "Rename {} or remove it. Removed, its folder and everything in it stay on disk.",
        t.name
    );
    match choose(Tone::Question, &t.name, &text, buttons) {
        Some(0) => {
            if let Some(n) = type_in("Rename the town", "Its folder keeps its name.", &t.name, "Name", "rename") {
                let n = n.trim().to_string();
                if !n.is_empty() {
                    edit(i, |t| t.name = n);
                }
            }
        }
        Some(1) if buttons.len() == 3 => {
            let slug = t.slug.clone();
            town::change(|all| all.retain(|t| t.slug != slug));
        }
        _ => {}
    }
}

/// The folder a home turned off is kept in, so turning it on again finds
/// its login where it was.
const HOME_OFF: &str = "claude.off";

fn login(t: &town::Town) {
    let name = &t.saved.name;
    let home = core::home_in(&t.dir);
    let off = t.dir.join(HOME_OFF);
    if t.home().is_none() {
        let text = format!(
            "{name}'s sessions would run on a Claude account of their own, beside the one the projects use, and see that account's connectors only. A console opens to log in. Accounts never switches it."
        );
        if choose(Tone::Question, "A login of its own", &text, &["Log in", "Cancel"]) != Some(0) {
            return;
        }
        let made = match off.is_dir() {
            true => std::fs::rename(&off, &home),
            false => std::fs::create_dir_all(&home),
        };
        if let Err(e) = made {
            return fail("No login of its own", &format!("{}: {e}", home.display()));
        }
        hooks_into(&home);
        log_in(&home);
        return;
    }
    let text = format!(
        "Log {name} in again, or go back to the projects' login. Its own stays on disk for next time. Sessions running there keep the login they started with."
    );
    match choose(Tone::Question, "Its Claude login", &text, &["Log in", "Use the projects' login", "Cancel"]) {
        Some(0) => log_in(&home),
        Some(1) => {
            if let Err(e) = std::fs::rename(&home, &off) {
                fail(
                    "Still its own login",
                    &format!("{e}. End the town's sessions and try again."),
                );
            }
        }
        _ => {}
    }
}

/// The hooks and the mod, so its sessions move their tiles: they read their
/// settings from this home, never from `~/.claude`.
fn hooks_into(home: &Path) {
    let settings = home.join("settings.json");
    if let Err(e) = horadric_hooks::install::install(&settings, horadric_hooks::port()) {
        fail("No hooks for the town", &format!("{}: {e}", settings.display()));
    }
}

fn log_in(home: &Path) {
    if let Err(e) = crate::accounts::log_in_at(Some(home)) {
        fail("Cannot log in", &e.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(slug: &str) -> View {
        View {
            saved: SavedTown {
                name: slug.to_uppercase(),
                slug: slug.into(),
                ..Default::default()
            },
            folder: format!(r"C:\Users\x\Horadric\towns\{slug}"),
            ..Default::default()
        }
    }

    #[test]
    fn with_no_town_there_is_only_add() {
        let rows = lines(&[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].field, Field::Town(Row::Add));
    }

    #[test]
    fn each_town_has_its_rows_and_says_what_it_is_now() {
        let mut acme = view("acme");
        acme.saved.own_browser = true;
        acme.login = Some(Some("me@acme.example".into()));
        acme.saved.connectors = vec!["Slack".into(), "Atlassian".into()];
        let rows = lines(&[acme, view("globex")]);
        assert_eq!(rows.len(), 1 + 2 * 7);
        assert_eq!(rows[1].value, "ACME");
        assert_eq!(rows[2].value, r"ACME, towns\acme");
        assert_eq!(rows[3].control, Control::Switch(true));
        assert_eq!(rows[5].value, "Slack, Atlassian");
        assert_eq!(rows[6].value, "me@acme.example");
        assert_eq!(rows[7].control, Control::Switch(true));
        assert_eq!(rows[13].value, "The projects' login, give it its own");
        assert_eq!(rows[14].control, Control::Switch(false));
        for r in &rows {
            if !matches!(r.control, Control::Note | Control::Fixed) {
                assert!(!r.help.is_empty(), "{:?}", r.field);
            }
        }
    }

    #[test]
    fn an_identity_picked_names_the_browser() {
        let mut v = view("acme");
        v.identity = Some("work".into());
        let rows = lines(&[v]);
        assert_eq!(rows[7].control, Control::Fixed);
        assert!(rows[7].value.contains("work"));
    }
}
