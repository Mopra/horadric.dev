//! The Vault's rows in the Settings window: a project's identity and a
//! line on what it keeps in the Projects section, the secrets and web
//! logins its sessions get in the Secrets section, and the identities in
//! one of their own. See docs/plan/vault.md, "Whose is what", "Logging in"
//! and "Adding one".
//!
//! A row's click is taken here, not by the app. An identity's rows ask
//! something, and a question runs a modal loop, so their clicks wait for a
//! timer to fire outside every borrow. The Secrets section asks nothing in
//! a window of its own: its forms are rows typed into in place (see
//! [`super::secrets`]). Values are never shown, only names, whose they are
//! and when they were last used.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::session::Phase;
use horadric_core::vault::identity as core;
use horadric_core::vault::secret::Owner;
use horadric_core::vault::web;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::identity::{self, Git, Identity, Resolved, Tool};
use super::secrets::{self, Form, Group, Save, Slot, Ui, View};
use super::store;
use crate::ask;
use crate::dialog::{self, Dialog, Tone};
use crate::field::{self, Field as Text};
use crate::settings::{Control, Field, Line};
use crate::window::Shared;

/// A row of the Vault's, by what it is about. Indices are into the view
/// the rows were drawn from, which is the one a click reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Which identity the project's sessions get.
    Pick,
    /// Running sessions still on the identity they started with.
    LeftBehind,
    /// What the project keeps, counted, which opens the Secrets section.
    Summary,
    /// A secret kept for the project, its identity or Everywhere.
    Secret(u16),
    Login(u16),
    /// A name the project's config.json asks for that is not kept.
    Missing(u16),
    /// The Secrets section's filter by name.
    Filter,
    /// Add a secret, Add a web login.
    Adds,
    /// A group's heading, which folds it.
    Group(Group),
    /// A line of what an opened item says.
    Detail,
    /// One thing the opened item is allowed always for, with its Remove.
    Allowed(u16),
    /// What can be done to the opened item, or Forget waiting to be sure.
    Actions,
    /// What a form is for.
    FormTitle,
    /// A field of the form.
    Input(Slot),
    /// What is wrong with a field of the form.
    Wrong(Slot),
    /// Whose a new item is.
    Whose,
    /// Why a paste or a keystroke was refused.
    Refused,
    /// Why the last change could not be kept.
    Failed,
    FormButtons,
    AddIdentity,
    /// Said under Add when there is no identity yet.
    NoIdentity,
    /// Said in place of the rows for a project on a linked machine.
    Afar,
    /// Said over the rows when the secrets file will not open.
    Problem,
    /// An identity's name over its rows.
    Heading(u16),
    Name(u16),
    Git(u16),
    Claims(u16),
    Tool(u16, Tool),
}

/// The line under a row's name.
pub fn help(row: Row) -> &'static str {
    match row {
        Row::Pick => "Whose git, gh, npm and Vercel logins its new sessions get",
        Row::Summary => "Tokens and web logins its sessions use but never read",
        Row::LeftBehind => "A session keeps the logins it started with until it restarts",
        Row::Secret(_) => "Its sessions use it through secret run, never read it",
        Row::Login(_) => "Its sessions sign in with it in the browser pane, never read it",
        Row::Missing(_) => "Its config.json asks for it and none is kept",
        Row::AddIdentity => "One of your worlds, with git and logins of its own",
        Row::NoIdentity
        | Row::Heading(_)
        | Row::Filter
        | Row::Adds
        | Row::Group(_)
        | Row::Detail
        | Row::Allowed(_)
        | Row::Actions
        | Row::FormTitle
        | Row::Input(_)
        | Row::Wrong(_)
        | Row::Whose
        | Row::Refused
        | Row::Failed
        | Row::FormButtons => "",
        Row::Afar => "Its identity and secrets are that machine's",
        Row::Problem => "Nothing kept shows or can be added until it opens",
        Row::Name(_) => "Rename it, or delete it and its logins",
        Row::Git(_) => "The name and email its commits carry, and its GitHub user",
        Row::Claims(_) => "Projects under these folders are its own unless they pick",
        Row::Tool(_, Tool::Gh) => "The GitHub CLI login its sessions use",
        Row::Tool(_, Tool::Npm) => "The npm registry logins its sessions use",
        Row::Tool(_, Tool::Vercel) => "The Vercel login its sessions use",
    }
}

fn label(row: Row) -> &'static str {
    match row {
        Row::Pick => "Identity",
        Row::Summary => "Secrets",
        Row::LeftBehind => "Running sessions",
        Row::Secret(_) => "Secret",
        Row::Login(_) => "Web login",
        Row::Missing(_) => "Wanted by config.json",
        Row::AddIdentity => "Add an identity",
        Row::Input(slot) => slot.label(),
        Row::Filter => "Find by name",
        Row::Whose => "Whose",
        Row::NoIdentity
        | Row::Heading(_)
        | Row::Adds
        | Row::Group(_)
        | Row::Detail
        | Row::Allowed(_)
        | Row::Actions
        | Row::FormTitle
        | Row::Wrong(_)
        | Row::Refused
        | Row::Failed
        | Row::FormButtons => "",
        Row::Afar => "Vault",
        Row::Problem => "Secrets file",
        Row::Name(_) => "Name",
        Row::Git(_) => "Git",
        Row::Claims(_) => "Folders it claims",
        Row::Tool(_, t) => t.name(),
    }
}

fn line(row: Row, value: String, control: Control) -> Line {
    let field = Field::Vault(row);
    Line {
        field,
        label: label(row).into(),
        value,
        control,
        help: field.help(),
        parts: Vec::new(),
        nested: false,
    }
}

thread_local! {
    static VIEW: RefCell<View> = RefCell::new(View::default());
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
    static PENDING: Cell<Option<Row>> = const { Cell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    /// The Secrets section's folds, filter, opened item and form, for as
    /// long as the Settings window is open.
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

/// What the questions are drawn with, from the Settings window.
pub fn set_shared(shared: Rc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
}

/// Reads everything again before the rows are next drawn: something
/// changed, here, in a console the human logged in from, or by `horadric
/// secret set`.
pub fn stale() {
    VIEW.with(|v| v.borrow_mut().built = false);
}

/// The folder of the project `key`, the town's own for the town.
fn project_dir(key: &str) -> PathBuf {
    if let Some(d) = crate::town::dir_of(key) {
        return d;
    }
    PathBuf::from(key)
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.strip_prefix('\u{feff}')
        .map(String::from)
        .unwrap_or(text)
}

/// The identities kept on this machine. None under test, so a test does
/// not draw whatever this machine's identities file holds.
fn identities() -> Vec<Identity> {
    if cfg!(test) {
        Vec::new()
    } else {
        identity::all()
    }
}

/// Whether the project `key` lives on a linked machine, whose Vault is
/// kept there and not here.
fn afar(key: &str) -> bool {
    horadric_core::afar::split_key(key).is_some()
}

fn build(key: &str) -> View {
    let ids = identities();
    let who = ids
        .iter()
        .map(|i| Tool::ALL.map(|t| i.tools.contains(&t).then(|| identity::who(&i.slug, t))))
        .collect();
    let mut v = View {
        built: true,
        key: key.to_string(),
        ids,
        who,
        ..Default::default()
    };
    // A linked machine's project reads nothing here: its pick would be
    // written into this machine's local.json as a project it does not have.
    if key.is_empty() || afar(key) {
        return v;
    }
    let dir = project_dir(key);
    let config = read_text(&horadric_hooks::tasks::config_file(&dir));
    let resolved = identity::of_project(&dir);
    v.pick = identity::pick(key);
    v.file = core::config_identity(&config);
    let slug = resolved.slug().map(String::from);
    v.resolved = Some(resolved);
    v.problem = store::problem();
    // The listing, not the store itself: a view kept between paints holds
    // no value, password or seed.
    let listing = store::listing(key, slug.as_deref());
    v.fill(key, slug.as_deref(), listing, core::config_secrets(&config));
    v
}

/// Runs `f` on the view, read again first if it is stale or was read for
/// another project. None for `key` keeps the project it has.
fn with_view<R>(key: Option<&str>, f: impl FnOnce(&View) -> R) -> R {
    VIEW.with(|cell| {
        let mut v = cell.borrow_mut();
        let key = key.map_or_else(|| v.key.clone(), String::from);
        if !v.built || v.key != key {
            *v = build(&key);
        }
        f(&v)
    })
}

/// How long ago a unix time was, in the largest whole unit.
pub fn ago(at: u64, now: u64) -> String {
    let s = now.saturating_sub(at);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => match s / 86_400 {
            1 => "a day ago".into(),
            d => format!("{d} days ago"),
        },
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The last part of a path or a URL's host, for a line's room.
pub(super) fn short(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

pub(super) fn host(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, h)| h)
}

/// Some names in a row's room: a couple by name, the rest counted.
fn few(names: &[String]) -> String {
    match names {
        [] => "None".into(),
        [a] => a.clone(),
        [a, b] => format!("{a}, {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// The Projects section's Vault rows for the project `key`, none without
/// one: its identity, and what it keeps counted, which opens the Secrets
/// section.
pub fn project_lines(key: &str) -> Vec<Line> {
    if key.is_empty() {
        return Vec::new();
    }
    if afar(key) {
        return vec![line(
            Row::Afar,
            "Kept on the machine it runs on".into(),
            Control::Note,
        )];
    }
    with_view(Some(key), |v| {
        let mut rows = Vec::new();
        if v.problem.is_some() {
            rows.push(line(
                Row::Problem,
                "Will not open, see Secrets".into(),
                Control::Note,
            ));
        }
        rows.push(line(Row::Pick, pick_value(v), Control::Button));
        if let Some(value) = left_behind_value(v, key) {
            rows.push(line(Row::LeftBehind, value, Control::Button));
        }
        rows.push(line(Row::Summary, secrets::summary(v), Control::Button));
        rows
    })
}

thread_local! {
    /// The last count, with when and for which project: the rows are asked
    /// for at every paint, and each session's identity is a file read.
    static BEHIND: RefCell<Option<(String, std::time::Instant, Vec<String>)>> = const { RefCell::new(None) };
}

/// The slug each running agent session of the project `key` started with
/// where it is not the one the project gets now, "" for none. A plain
/// terminal is not counted, since Restart them leaves it be.
fn left_behind(key: &str) -> Vec<String> {
    let fresh = BEHIND.with(|b| {
        b.borrow()
            .as_ref()
            .filter(|(k, at, _)| k == key && at.elapsed() < std::time::Duration::from_secs(3))
            .map(|(_, _, list)| list.clone())
    });
    if let Some(list) = fresh {
        return list;
    }
    let sessions: Vec<(String, String)> = shared()
        .and_then(|sh| {
            let r = sh.registry.lock().ok()?;
            Some(
                r.all()
                    .filter(|s| crate::project::project_key(s) == key)
                    .filter(|s| !s.shell && s.background.is_none() && s.brother.is_none())
                    .filter(|s| !matches!(s.phase, Phase::Paused | Phase::Ended))
                    .map(|s| (s.id.clone(), s.cwd.clone()))
                    .collect(),
            )
        })
        .unwrap_or_default();
    let list: Vec<String> = sessions
        .iter()
        .filter(|(id, cwd)| identity::left_behind(id, Path::new(cwd)))
        .filter_map(|(id, _)| identity::started_as(id))
        .collect();
    BEHIND.with(|b| {
        *b.borrow_mut() = Some((key.to_string(), std::time::Instant::now(), list.clone()));
    });
    list
}

fn left_behind_value(v: &View, key: &str) -> Option<String> {
    let old = left_behind(key);
    let first = old.first()?;
    let whose = match old.iter().all(|s| s == first) {
        true if first.is_empty() => "the machine's logins".to_string(),
        true => v.identity_name(first),
        false => "an old identity".to_string(),
    };
    let n = old.len();
    let sessions = if n == 1 {
        "1 session"
    } else {
        &format!("{n} sessions")
    };
    Some(format!("{sessions} still on {whose}, restart them"))
}

/// Whether an identity can be made by the name config.json gives: only
/// one already in slug form, since the slug made would not be the name it
/// is asked for by, and the project would stay without it.
fn makeable(file: &str) -> bool {
    core::slug(file) == file
}

fn pick_value(v: &View) -> String {
    match v.resolved.as_ref() {
        Some(Resolved::Identity(slug)) => {
            let name = v.identity_name(slug);
            if v.pick.as_deref() == Some(slug.as_str()) {
                name
            } else if v
                .file
                .as_deref()
                .is_some_and(|f| f.eq_ignore_ascii_case(slug))
            {
                format!("{name}, as config.json says")
            } else {
                format!("{name}, by its folder")
            }
        }
        Some(Resolved::Unknown(slug)) if makeable(slug) => {
            format!("{slug} in config.json, not on this PC, make it")
        }
        Some(Resolved::Unknown(slug)) => format!(
            "{slug} in config.json is not a slug: fix its identity to one like {}",
            core::slug(slug)
        ),
        _ if v.pick.as_deref() == Some("") => "None, picked".into(),
        _ => "None, the machine's logins".into(),
    }
}

/// The Identities section's rows.
pub fn identity_lines() -> Vec<Line> {
    with_view(None, |v| {
        let mut rows = vec![line(
            Row::AddIdentity,
            "Add\u{2026}".into(),
            Control::Button,
        )];
        if v.ids.is_empty() {
            rows.push(line(
                Row::NoIdentity,
                "None yet: every project's sessions use this PC's own logins".into(),
                Control::Note,
            ));
        }
        for (i, id) in v.ids.iter().enumerate() {
            let n = i as u16;
            rows.push(line(Row::Heading(n), id.name.clone(), Control::Note));
            rows.push(line(
                Row::Name(n),
                format!("{}, {}", id.name, id.slug),
                Control::Button,
            ));
            let git = match &id.git {
                Some(g) => {
                    let mut s = format!("{} <{}>", g.name, g.email);
                    if let Some(u) = &g.github_user {
                        s.push_str(&format!(", GitHub {u}"));
                    }
                    s
                }
                None => "This PC's, set its own".into(),
            };
            rows.push(line(Row::Git(n), git, Control::Button));
            let claims: Vec<String> = id.claims.iter().map(|c| short(c).to_string()).collect();
            rows.push(line(Row::Claims(n), few(&claims), Control::Button));
            for (t, tool) in Tool::ALL.into_iter().enumerate() {
                let value = match v.who.get(i).and_then(|w| w[t].clone()) {
                    None => "This PC's login, log in".to_string(),
                    Some(None) => "Not logged in, log in".to_string(),
                    Some(Some(w)) if w.is_empty() => "Logged in, ask who".to_string(),
                    Some(Some(w)) => w,
                };
                rows.push(line(Row::Tool(n, tool), value, Control::Button));
            }
        }
        rows
    })
}

/// The Secrets section's rows for the project `key`, under its picker.
pub fn secrets_lines(key: &str) -> Vec<Line> {
    if key.is_empty() {
        return Vec::new();
    }
    if afar(key) {
        return vec![line(
            Row::Afar,
            "Kept on the machine it runs on".into(),
            Control::Note,
        )];
    }
    let now = now();
    with_view(Some(key), |v| {
        UI.with(|u| secrets::lines(v, &u.borrow(), now))
    })
}

/// Every item the project `key` keeps or misses, one row each, for the
/// window's search to find by name.
pub fn secret_items(key: &str) -> Vec<Line> {
    if key.is_empty() || afar(key) {
        return Vec::new();
    }
    let now = now();
    with_view(Some(key), |v| secrets::items(v, now))
}

/// What the Settings window does after the Secrets section took a click
/// or a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    Nothing,
    /// The keyboard goes to this row.
    Focus(Row),
}

/// A change to the sealed store a click asks for, made once every borrow
/// of the view is let go: a change reads the view again.
enum Deed {
    Save(Save),
    Forget(secrets::ItemId),
    /// Stops allowing one Always of an item.
    Unallow(secrets::ItemId, String),
}

/// The keyboard to the open form, at its first empty field.
fn to_form(ui: &Ui) -> After {
    ui.form
        .as_ref()
        .map_or(After::Nothing, |f| After::Focus(Row::Input(f.first())))
}

/// Starts `form`, or, when a draft is open, goes back to that one.
fn start(ui: &mut Ui, form: Form) -> After {
    ui.start(form);
    to_form(ui)
}

fn clicked(v: &View, ui: &mut Ui, row: Row, part: Option<usize>) -> (After, Option<Deed>) {
    let expanded = ui.expanded.clone();
    let kept = expanded
        .as_ref()
        .and_then(|id| v.find(id).map(|i| (id.clone(), i)));
    let nothing = (After::Nothing, None);
    match (row, part) {
        (Row::Adds, Some(0)) => (start(ui, Form::add_secret("")), None),
        (Row::Adds, Some(1)) => (start(ui, Form::add_login()), None),
        (Row::Group(g), _) => {
            ui.fold(g);
            nothing
        }
        (Row::Secret(_) | Row::Login(_), _) => {
            if let Some(id) = secrets::item_at(v, row) {
                ui.open(id);
            }
            (After::Focus(row), None)
        }
        (Row::Missing(i), _) => match v.missing.get(i as usize) {
            Some(name) => (start(ui, Form::add_secret(name)), None),
            None => nothing,
        },
        (Row::Allowed(j), Some(0)) => {
            let Some((id, i)) = kept else {
                return nothing;
            };
            let entry = match id.kind {
                store::Kind::Secret => v.secrets.get(i as usize),
                store::Kind::WebLogin => v.logins.get(i as usize),
            }
            .and_then(|k| k.allowed.get(j as usize).cloned());
            match entry {
                Some(e) => (After::Nothing, Some(Deed::Unallow(id, e))),
                None => nothing,
            }
        }
        (Row::Actions, Some(j)) => {
            let Some((id, _)) = kept else {
                return nothing;
            };
            let login = id.kind == store::Kind::WebLogin;
            match (ui.confirm, login, j) {
                (true, _, 0) => (After::Nothing, Some(Deed::Forget(id))),
                (true, _, _) => {
                    ui.confirm = false;
                    (After::Focus(Row::Actions), None)
                }
                (false, false, 0) => (start(ui, Form::replace(id, Slot::Value)), None),
                (false, true, 0) => (start(ui, Form::replace(id, Slot::Password)), None),
                (false, true, 1) => (start(ui, Form::replace(id, Slot::Seed)), None),
                (false, _, _) => {
                    ui.confirm = true;
                    (After::Focus(Row::Actions), None)
                }
            }
        }
        (Row::Whose, Some(j)) => {
            let offered = secrets::offers(v);
            if let (Some(form), Some((w, _))) = (ui.form.as_mut(), offered.get(j)) {
                form.whose = *w;
                form.edited(Slot::Whose);
            }
            nothing
        }
        (Row::FormButtons, Some(0)) => save(v, ui),
        (Row::FormButtons, Some(_)) => {
            ui.form = None;
            nothing
        }
        (Row::Input(_), Some(1)) => {
            if let Some(f) = ui.form.as_mut() {
                f.shown = !f.shown;
            }
            nothing
        }
        (Row::Input(_) | Row::Filter, _) => (After::Focus(row), None),
        _ => nothing,
    }
}

/// Save pressed, or Enter in the form: what to keep, or every problem
/// shown and the keyboard at the first.
fn save(v: &View, ui: &mut Ui) -> (After, Option<Deed>) {
    let Some(form) = ui.form.as_mut() else {
        return (After::Nothing, None);
    };
    let offered = secrets::offers(v);
    match secrets::decide(form, &v.key, v.slug().as_deref(), &offered) {
        Ok(save) => (After::Nothing, Some(Deed::Save(save))),
        Err(wrong) => {
            form.tried = true;
            let at = wrong
                .iter()
                .map(|(s, _)| *s)
                .find(|s| *s != Slot::Whose)
                .map_or(After::Focus(Row::FormButtons), |s| {
                    After::Focus(Row::Input(s))
                });
            (at, None)
        }
    }
}

/// Keeps what `deed` asks in the sealed store.
fn apply(deed: &Deed) -> Result<(), String> {
    let same =
        |name: &str, owner: &Owner, id: &secrets::ItemId| id.name == name && id.owner == *owner;
    match deed {
        Deed::Save(save) => keep(save),
        Deed::Forget(id) => store::change(|v| match id.kind {
            store::Kind::Secret => v.secrets.retain(|s| !same(&s.name, &s.owner, id)),
            store::Kind::WebLogin => v.logins.retain(|l| !same(&l.name, &l.owner, id)),
        }),
        Deed::Unallow(id, entry) => store::change(|v| match id.kind {
            store::Kind::Secret => v
                .secrets
                .iter_mut()
                .filter(|s| same(&s.name, &s.owner, id))
                .for_each(|s| s.allowed.retain(|a| a != entry)),
            store::Kind::WebLogin => v
                .logins
                .iter_mut()
                .filter(|l| same(&l.name, &l.owner, id))
                .for_each(|l| l.allowed.retain(|a| a != entry)),
        }),
    }
}

/// Seals what a form saves, as the questions it replaced did: a new item
/// takes the place of one of the same name and owner, allowed for
/// nothing yet, and a new value is a new secret whose Always answers are
/// asked again.
fn keep(save: &Save) -> Result<(), String> {
    match save {
        Save::Secret { name, owner, value } => store::change(|v| {
            v.secrets
                .retain(|s| !(s.name == *name && s.owner == *owner));
            v.secrets.push(store::Secret {
                name: name.clone(),
                owner: owner.clone(),
                value: value.clone(),
                allowed: Vec::new(),
                last_used: None,
            });
        }),
        Save::Login {
            name,
            owner,
            origins,
            user,
            password,
            seed,
        } => store::change(|v| {
            v.logins.retain(|l| !(l.name == *name && l.owner == *owner));
            v.logins.push(store::WebLogin {
                name: name.clone(),
                owner: owner.clone(),
                origins: origins.clone(),
                user: user.clone(),
                password: password.clone(),
                totp: seed.clone(),
                allowed: Vec::new(),
                last_used: None,
            });
        }),
        Save::Value { item, value } => store::change(|v| {
            for s in v
                .secrets
                .iter_mut()
                .filter(|s| s.name == item.name && s.owner == item.owner)
            {
                s.value = value.clone();
                s.allowed.clear();
            }
        }),
        Save::Password { item, password } => store::change(|v| {
            for l in v
                .logins
                .iter_mut()
                .filter(|l| l.name == item.name && l.owner == item.owner)
            {
                l.password = password.clone();
            }
        }),
        Save::Seed { item, seed } => store::change(|v| {
            for l in v
                .logins
                .iter_mut()
                .filter(|l| l.name == item.name && l.owner == item.owner)
            {
                l.totp = seed.clone();
            }
        }),
    }
}

/// The item a saved form is about, to open once it is kept.
fn saved_item(save: &Save) -> secrets::ItemId {
    let id = |kind, name: &String, owner: &Owner| secrets::ItemId {
        kind,
        name: name.clone(),
        owner: owner.clone(),
    };
    match save {
        Save::Secret { name, owner, .. } => id(store::Kind::Secret, name, owner),
        Save::Login { name, owner, .. } => id(store::Kind::WebLogin, name, owner),
        Save::Value { item, .. } | Save::Password { item, .. } | Save::Seed { item, .. } => {
            item.clone()
        }
    }
}

/// Does what a deed asks and says how it went, out of every borrow.
fn carry_out(deed: Deed) {
    let result = apply(&deed);
    UI.with(|u| {
        let mut u = u.borrow_mut();
        match (&result, &deed) {
            (Err(e), _) => u.failure = Some(format!("Not kept: {e}")),
            (Ok(()), Deed::Save(save)) => {
                u.form = None;
                u.failure = None;
                u.reveal(saved_item(save));
            }
            (Ok(()), Deed::Forget(_)) => {
                u.expanded = None;
                u.confirm = false;
                u.failure = None;
            }
            (Ok(()), Deed::Unallow(..)) => u.failure = None,
        }
    });
    stale();
}

/// A row of the Secrets section clicked, on its part `part` when it has
/// parts.
pub fn secrets_click(row: Row, part: Option<usize>) -> After {
    let (after, deed) = with_view(None, |v| {
        UI.with(|u| clicked(v, &mut u.borrow_mut(), row, part))
    });
    if let Some(d) = deed {
        carry_out(d);
    }
    after
}

/// Enter in a field of the section: a form's saves it.
pub fn enter(row: Row) -> After {
    if !matches!(row, Row::Input(_)) {
        return After::Nothing;
    }
    secrets_click(Row::FormButtons, Some(0))
}

/// Esc with the keyboard on `row`. A form's rows cancel it and the
/// filter empties; false when Esc means nothing there.
pub fn escape(row: Row) -> bool {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        match row {
            Row::Input(_) | Row::Whose | Row::FormButtons if u.form.is_some() => {
                u.form = None;
                true
            }
            Row::Filter if !u.filter.text.is_empty() => {
                u.filter.wipe();
                true
            }
            _ => false,
        }
    })
}

/// Whether Esc with the keyboard on `row`, a button of the open form,
/// cancels that form.
pub fn escape_cancels(row: Row) -> bool {
    matches!(row, Row::Whose | Row::FormButtons) && UI.with(|u| u.borrow().form.is_some())
}

/// The first field of the draft open, if one is.
pub fn draft() -> Option<Row> {
    UI.with(|u| u.borrow().form.as_ref().map(|f| Row::Input(f.first())))
}

/// A row the window's search found, shown in the section: the filter
/// emptied, its group unfolded and an item opened.
pub fn reveal(row: Row) -> After {
    let id = with_view(None, |v| secrets::item_at(v, row));
    UI.with(|u| {
        let mut u = u.borrow_mut();
        match id {
            Some(id) => u.reveal(id),
            None => {
                u.filter.wipe();
                u.folded.retain(|g| *g != Group::Missing);
            }
        }
    });
    After::Focus(row)
}

/// The Settings window closed: folds and filter are forgotten, and a draft
/// with them, its fields zeroed as it goes.
pub fn closed() {
    let old = UI.with(|u| std::mem::take(&mut *u.borrow_mut()));
    drop(old);
}

/// What the window draws of a field of the section.
pub struct InputLook {
    /// As drawn: dots for a secret not shown.
    pub text: String,
    /// In UTF-16 units, as DirectWrite counts.
    pub caret: u32,
    pub anchor: u32,
    pub placeholder: &'static str,
}

pub fn input_look(row: Row) -> Option<InputLook> {
    UI.with(|u| {
        let u = u.borrow();
        let (t, hidden, placeholder) = match row {
            Row::Filter => (&u.filter, false, "Part of a name"),
            Row::Input(slot) => {
                let f = u.form.as_ref()?;
                (
                    f.field(slot)?,
                    slot.secret() && !f.shown,
                    slot.placeholder(&f.purpose),
                )
            }
            _ => return None,
        };
        Some(InputLook {
            text: secrets::display(t, hidden),
            caret: field::utf16_at(&t.text, t.caret),
            anchor: field::utf16_at(&t.text, t.anchor),
            placeholder,
        })
    })
}

/// Runs `f` on the field of `row`, told whether it holds a secret. What
/// `f` gives back as a refusal is said under the form.
fn edit_with(row: Row, f: impl FnOnce(&mut Text, bool) -> Option<String>) {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        match row {
            Row::Filter => {
                f(&mut u.filter, false);
            }
            Row::Input(slot) => {
                let Some(form) = u.form.as_mut() else {
                    return;
                };
                let Some(t) = form.field_mut(slot) else {
                    return;
                };
                let refused = f(t, slot.secret());
                form.edited(slot);
                form.refusal = refused;
            }
            _ => {}
        }
    });
}

/// A key that edits the field of `row`: the caret moved, a character
/// erased, undone.
pub fn input_edit(row: Row, f: impl FnOnce(&mut Text)) {
    edit_with(row, |t, _| {
        f(t);
        None
    });
}

/// Text typed into the field of `row`. A secret's field full says so
/// rather than cut the value short.
pub fn input_typed(row: Row, s: &str) {
    edit_with(row, |t, secret| {
        if secret && t.room() == 0 {
            return Some(ask::too_long(ask::MAX_SECRET));
        }
        t.insert(s);
        None
    });
}

/// A paste into the field of `row`. A secret is taken whole or refused
/// with why, as the question it replaced did.
pub fn input_paste(row: Row, s: &str) {
    edit_with(row, |t, secret| {
        if !secret {
            t.insert(s);
            return None;
        }
        match ask::secret_paste(s, t.room()) {
            Ok(s) => {
                t.insert(&s);
                None
            }
            Err(why) => Some(why),
        }
    });
}

/// The selection of the field of `row` for the clipboard, cut out of it
/// when `cut`. None for a secret, which never leaves its field.
pub fn input_copy(row: Row, cut: bool) -> Option<String> {
    let mut out = None;
    edit_with(row, |t, secret| {
        if !secret && t.caret != t.anchor {
            out = Some(if cut {
                t.cut()
            } else {
                t.selected().to_string()
            });
        }
        None
    });
    out
}

/// Puts the caret of the field of `row` at UTF-16 offset `at`, carrying
/// the selection along when `extend`.
pub fn input_caret(row: Row, at: u32, extend: bool) {
    input_edit(row, |t| {
        let b = field::byte_at(&t.text, at);
        t.set_caret(b, extend);
    });
}

/// A Vault row clicked. What it asks waits for the app's own message loop,
/// out of every borrow.
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
    stale();
    crate::settings::vault_changed();
}

fn shared() -> Option<Rc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

/// Buttons, the one pressed if any.
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

pub(crate) fn done(what: &str, result: Result<(), String>) {
    if let Err(e) = result {
        fail(what, &e);
    }
}

/// A line typed, None when cancelled.
fn type_in(
    title: &str,
    prompt: &str,
    initial: &str,
    placeholder: &str,
    verb: &str,
    secret: bool,
) -> Option<String> {
    let shared = shared()?;
    let question = ask::Ask {
        title,
        prompt,
        initial,
        placeholder,
        verb,
        notes: false,
        secret,
        pick: None,
    };
    ask::ask(shared, None, &question).map(|a| a.text)
}

/// What [`pick_one`] got: one of the offers by its place, or text typed.
enum Picked {
    Offer(usize),
    Typed(String),
}

/// The labels `offers` show, made unique by their detail where two would
/// read the same, so the one picked is the one meant.
fn unique_labels(offers: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(offers.len());
    for (i, (l, d)) in offers.iter().enumerate() {
        let twin = offers
            .iter()
            .enumerate()
            .any(|(j, (o, _))| j != i && o.eq_ignore_ascii_case(l));
        let mut label = if twin && !d.is_empty() {
            format!("{l} ({d})")
        } else {
            l.clone()
        };
        while out.iter().any(|o| o.eq_ignore_ascii_case(&label)) {
            label.push_str(&format!(" #{}", i + 1));
        }
        out.push(label);
    }
    out
}

/// One of `offers` (label, detail) picked as it is typed, or with `free`
/// anything typed.
fn pick_one(
    title: &str,
    prompt: &str,
    verb: &str,
    offers: &[(String, String)],
    free: bool,
) -> Option<Picked> {
    let shared = shared()?;
    let labels = unique_labels(offers);
    let offers: Vec<(String, String)> = labels
        .iter()
        .zip(offers)
        .map(|(l, (_, d))| (l.clone(), d.clone()))
        .collect();
    let suggest = |text: &str| {
        let t = text.trim().to_lowercase();
        offers
            .iter()
            .filter(|(l, d)| l.to_lowercase().contains(&t) || d.to_lowercase().contains(&t))
            .take(ask::LIST_ROWS)
            .map(|(l, d)| ask::Suggestion {
                label: l.clone(),
                detail: d.clone(),
                value: l.clone(),
            })
            .collect()
    };
    let check = |text: &str| {
        let t = text.trim();
        (!free && !offers.iter().any(|(l, _)| l.eq_ignore_ascii_case(t)))
            .then(|| "Pick one from the list".to_string())
    };
    let pick = ask::Pick {
        suggest: &suggest,
        check: &check,
        browse: false,
        glyph: '\u{E77B}',
        paths: false,
    };
    let question = ask::Ask {
        title,
        prompt,
        initial: "",
        placeholder: "",
        verb,
        notes: false,
        secret: false,
        pick: Some(&pick),
    };
    let text = ask::ask(shared, None, &question)?.text;
    let text = text.trim();
    Some(
        offers
            .iter()
            .position(|(l, _)| l.eq_ignore_ascii_case(text))
            .map_or_else(|| Picked::Typed(text.to_string()), Picked::Offer),
    )
}

fn act(row: Row) {
    let key = VIEW.with(|v| v.borrow().key.clone());
    match row {
        Row::Pick => pick_identity(&key),
        Row::LeftBehind => restart_left_behind(&key),
        Row::AddIdentity => {
            if let Some(name) = type_in(
                "Add an identity",
                "A name for one of your worlds: Work, Personal, a client. Its git and logins are set in its rows after.",
                "",
                "Name",
                "add it",
                false,
            ) {
                if !name.trim().is_empty() {
                    done("Add an identity", identity::create(&name).map(drop));
                }
            }
        }
        Row::NoIdentity
        | Row::Heading(_)
        | Row::Afar
        | Row::Problem
        | Row::Summary
        | Row::Secret(_)
        | Row::Login(_)
        | Row::Missing(_)
        | Row::Filter
        | Row::Adds
        | Row::Group(_)
        | Row::Detail
        | Row::Allowed(_)
        | Row::Actions
        | Row::FormTitle
        | Row::Input(_)
        | Row::Wrong(_)
        | Row::Whose
        | Row::Refused
        | Row::Failed
        | Row::FormButtons => {}
        Row::Name(i) | Row::Git(i) | Row::Claims(i) | Row::Tool(i, _) => {
            let Some((id, who)) = with_view(None, |v| {
                let id = v.ids.get(i as usize)?.clone();
                Some((id, v.who.get(i as usize).cloned().unwrap_or_default()))
            }) else {
                return;
            };
            match row {
                Row::Name(_) => name_of(&id),
                Row::Git(_) => git_of(&id),
                Row::Claims(_) => claims_of(&id),
                Row::Tool(_, tool) => {
                    let t = Tool::ALL.iter().position(|x| *x == tool).unwrap_or(0);
                    tool_of(&id, tool, who[t].clone());
                }
                _ => {}
            }
        }
    }
}

fn restart_left_behind(key: &str) {
    let n = left_behind(key).len();
    if n == 0 {
        return;
    }
    let text = format!(
        "{n} of this project's sessions started before its identity changed and still use the old logins. Each stops once its turn is done and carries on in the same conversation with the new ones."
    );
    if choose(
        Tone::Question,
        "Restart them",
        &text,
        &["Restart them", "Cancel"],
    ) == Some(0)
    {
        crate::app::push(crate::app::Input::RestartLeftBehind(key.to_string()));
        BEHIND.with(|b| *b.borrow_mut() = None);
    }
}

/// The identity the project or town `key` has now. A town goes only by a
/// pick made for it, as its browser does (see `town::profile_for`).
fn current(key: &str, ids: &[Identity]) -> Option<String> {
    let slug = if crate::town::is(key) {
        identity::pick(key).filter(|s| !s.is_empty())
    } else {
        identity::of_project(Path::new(key))
            .slug()
            .map(String::from)
    }?;
    ids.iter().any(|i| i.slug == slug).then_some(slug)
}

const MENU_NONE: usize = 0;
const MENU_NEW: usize = 1;
const MENU_MANAGE: usize = 2;
const MENU_IDS: usize = 3;
/// How many ids [`menu_item`] may use from its base.
pub const MENU_SPAN: usize = 100;

/// The Identity line of a project's menu and of its browser's: the
/// identities to pick from with the one it has ticked, and making one,
/// so giving a project an identity is two clicks from where the human
/// already is. None for a project on another machine.
pub fn menu_item(key: &str, base: usize) -> Option<crate::menu::Item> {
    use crate::menu::Item;
    if key.is_empty() || afar(key) {
        return None;
    }
    let ids = identity::all();
    let now = current(key, &ids);
    let tick = |id: usize, label: String, checked: bool| Item::Action {
        id: base + id,
        label,
        checked,
        bold: false,
    };
    let mut lines: Vec<Item> = ids
        .iter()
        .take(MENU_SPAN - MENU_IDS)
        .enumerate()
        .map(|(i, id)| {
            tick(
                MENU_IDS + i,
                id.name.clone(),
                now.as_deref() == Some(&id.slug),
            )
        })
        .collect();
    lines.push(tick(
        MENU_NONE,
        "None, this PC's own logins".into(),
        now.is_none(),
    ));
    lines.push(Item::Separator);
    lines.push(Item::action(base + MENU_NEW, "New identity\u{2026}"));
    lines.push(Item::action(
        base + MENU_MANAGE,
        "Manage identities\u{2026}",
    ));
    let name = now
        .and_then(|s| ids.iter().find(|i| i.slug == s))
        .map_or("none", |i| i.name.as_str());
    Some(Item::Submenu(format!("Identity: {name}"), lines))
}

/// Does what was picked from [`menu_item`]'s lines. False when `picked`
/// is not one of them.
pub fn menu_picked(key: &str, base: usize, picked: usize) -> bool {
    if !(base..base + MENU_SPAN).contains(&picked) {
        return false;
    }
    let result = match picked - base {
        MENU_MANAGE => {
            crate::app::open_identities();
            return true;
        }
        MENU_NONE => identity::set_pick(key, Some("")),
        MENU_NEW => {
            let Some(name) = type_in(
                "New identity",
                "A name for one of your worlds: Work, Personal, a client. This project gets it now, with a browser of its own; other projects can share it.",
                "",
                "Name",
                "make it",
                false,
            )
            .filter(|n| !n.trim().is_empty()) else {
                return true;
            };
            identity::create(&name).and_then(|id| identity::set_pick(key, Some(&id.slug)))
        }
        i => match identity::all().get(i - MENU_IDS) {
            Some(id) => identity::set_pick(key, Some(&id.slug)),
            None => return true,
        },
    };
    let ok = result.is_ok();
    done("Identity", result);
    if ok {
        changed(key);
    }
    true
}

/// The project's identity was set: Settings reads it again, and the app
/// moves its browser and its sessions over.
fn changed(key: &str) {
    stale();
    BEHIND.with(|b| *b.borrow_mut() = None);
    crate::app::push(crate::app::Input::IdentityChanged(key.to_string()));
}

enum Choice {
    Auto,
    Nobody,
    Id(String),
    Make(String),
}

fn pick_identity(key: &str) {
    if key.is_empty() || afar(key) {
        return;
    }
    let (ids, file) = with_view(Some(key), |v| (v.ids.clone(), v.file.clone()));
    let mut offers: Vec<(String, String, Choice)> = ids
        .iter()
        .map(|i| (i.name.clone(), i.slug.clone(), Choice::Id(i.slug.clone())))
        .collect();
    offers.push((
        "None".into(),
        "this PC's own logins, whatever config.json says".into(),
        Choice::Nobody,
    ));
    let auto = match core::resolve(None, file.as_deref(), key, &ids) {
        Resolved::Identity(s) => ids
            .iter()
            .find(|i| i.slug == s)
            .map_or(s.clone(), |i| i.name.clone()),
        Resolved::Unknown(s) => format!("{s}, not on this PC"),
        Resolved::None => "none".into(),
    };
    offers.push((
        "As config.json or its folder says".into(),
        auto,
        Choice::Auto,
    ));
    if let Some(slug) =
        file.filter(|f| makeable(f) && !ids.iter().any(|i| i.slug.eq_ignore_ascii_case(f)))
    {
        offers.push((
            format!("Make identity {slug}"),
            "as config.json names it".into(),
            Choice::Make(slug),
        ));
    }
    let shown: Vec<(String, String)> = offers
        .iter()
        .map(|(l, d, _)| (l.clone(), d.clone()))
        .collect();
    let Some(Picked::Offer(i)) = pick_one(
        "Identity",
        "Whose logins its sessions get. Sessions running now keep the one they started with; new ones get this.",
        "pick it",
        &shown,
        false,
    ) else {
        return;
    };
    let Some((_, _, choice)) = offers.into_iter().nth(i) else {
        return;
    };
    let result = match choice {
        Choice::Auto => identity::set_pick(key, None),
        Choice::Nobody => identity::set_pick(key, Some("")),
        Choice::Id(slug) => identity::set_pick(key, Some(&slug)),
        Choice::Make(slug) => {
            identity::create_with_slug(&slug, &slug).and_then(|_| identity::set_pick(key, None))
        }
    };
    let ok = result.is_ok();
    done("Identity", result);
    if ok {
        changed(key);
    }
}

/// How an Always reads in the list: a secret's by its program's file name,
/// with the project and the content it was given for, since the same
/// program may be allowed for two projects or as it was before an update.
/// A web login's is a project key.
pub(super) fn allowed_offer(entry: &str) -> (String, String) {
    let parts = entry
        .strip_prefix("sha256:")
        .and_then(|rest| rest.split_once('|'))
        .and_then(|(hash, rest)| {
            rest.rsplit_once('|')
                .map(|(project, program)| (hash, project, program))
        });
    match parts {
        // Kept rather than dropped, so the human sees what was allowed;
        // a program no longer on the list is never run on it.
        Some((hash, project, program)) => {
            let used = horadric_core::vault::secret::always_allowed(
                horadric_core::vault::secret::allowed_program(program),
            );
            let mut detail = format!(
                "for {}, as it was at {}",
                short(project),
                hash.get(..8).unwrap_or(hash)
            );
            if !used {
                detail.push_str(
                    ", no longer used: only programs Horadric knows can be allowed always",
                );
            }
            (short(program).to_string(), detail)
        }
        None => (short(entry).to_string(), entry.to_string()),
    }
}

/// Whether `seed` reads as a one time code seed. A site's is at least 80
/// bits, 16 base32 characters; shorter is a typo or the wrong text copied,
/// and would make codes the site never accepts.
pub(super) fn seed_ok(seed: &str) -> bool {
    let chars = seed
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '='))
        .count();
    chars >= 16 && web::totp(seed, 0).is_some()
}

/// The origins a typed site holds: one or more, by spaces or commas, each
/// a URL or a bare host taken as https. Why not, for the first that will
/// not do.
pub fn parse_sites(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in text
        .split([' ', ',', ';', '\n', '\t'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let url = if part.contains("://") {
            part.to_string()
        } else {
            format!("https://{part}")
        };
        match web::origin(&url) {
            Some(o) if web::valid_origin(&o) => {
                if !out.contains(&o) {
                    out.push(o);
                }
            }
            Some(o) => {
                return Err(format!(
                    "{o} will not do: https, or plain http only on this PC"
                ))
            }
            None => return Err(format!("{part} is not a site's address")),
        }
    }
    if out.is_empty() {
        return Err("Type the address of the page you sign in on".into());
    }
    Ok(out)
}

/// A name for a login on `origin`: the part of its host before the top
/// level domain, upper case. `https://app.example.com` is EXAMPLE.
pub fn name_for(origin: &str) -> String {
    let host = host(origin);
    let host = host
        .rsplit_once(':')
        .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
        .map_or(host, |(h, _)| h);
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    let main = match parts.as_slice() {
        [] => "",
        [one] => one,
        [.., name, _] => name,
    };
    let mut name: String = main
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        name.insert_str(0, "SITE_");
    }
    name.truncate(64);
    name
}

fn name_of(id: &Identity) {
    let pressed = choose(
        Tone::Question,
        &id.name,
        &format!(
            "Its slug is {}, which config.json files name it by and which stays.",
            id.slug
        ),
        &["Rename", "Delete", "Keep"],
    );
    match pressed {
        Some(0) => {
            if let Some(name) = type_in(
                "Rename identity",
                "Its new name. The slug stays.",
                &id.name,
                "",
                "rename",
                false,
            ) {
                if !name.trim().is_empty() {
                    done("Rename identity", identity::rename(&id.slug, &name));
                }
            }
        }
        Some(1) => delete_identity(id),
        _ => {}
    }
}

/// Deletes `id` after asking, and the secrets and web logins kept as its
/// with it: left behind they would be nobody's, never used and never
/// shown, yet still sealed on this PC.
fn delete_identity(id: &Identity) {
    const TITLE: &str = "Delete identity";
    let owner = Owner::Identity(id.slug.clone());
    let listed = store::listing("", Some(&id.slug));
    let theirs = |kind| {
        listed
            .iter()
            .filter(|l| l.kind == kind && l.owner == owner)
            .count()
    };
    let (secrets, logins) = (theirs(store::Kind::Secret), theirs(store::Kind::WebLogin));
    let mut text = format!(
        "Deletes {} and its folder of logins on this PC. Projects that were its get this PC's own logins in their next sessions; sessions already running keep it until restarted.",
        id.name
    );
    let kept = secrets + logins;
    let buttons: &[&str] = if kept == 0 {
        &["Delete it", "Keep it"]
    } else {
        text.push_str(&format!(
            " It has {} kept as its own, which go with it.",
            counted(secrets, logins)
        ));
        &["Forget them too", "Cancel"]
    };
    if choose(Tone::Warning, TITLE, &text, buttons) != Some(0) {
        return;
    }
    // The identity goes first: a delete that fails must leave its
    // secrets where they were, not orphaned on an identity still there.
    if let Err(e) = identity::delete(&id.slug) {
        return fail(TITLE, &e);
    }
    let forgotten = match kept {
        0 => Ok(()),
        _ => store::change(|v| {
            v.secrets.retain(|s| s.owner != owner);
            v.logins.retain(|l| l.owner != owner);
        }),
    };
    done(TITLE, forgotten);
}

/// "2 secrets and a web login", for what an identity keeps.
fn counted(secrets: usize, logins: usize) -> String {
    let one = |n: usize, what: &str| match n {
        1 => format!("a {what}"),
        n => format!("{n} {what}s"),
    };
    match (secrets, logins) {
        (s, 0) => one(s, "secret"),
        (0, l) => one(l, "web login"),
        (s, l) => format!("{} and {}", one(s, "secret"), one(l, "web login")),
    }
}

fn git_of(id: &Identity) {
    const TITLE: &str = "Git";
    let old = id.git.clone().unwrap_or_default();
    let (send, accounts) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(identity::github_accounts());
    });
    let prompt = format!(
        "The name {}'s commits carry. Empty to use this PC's git as it is.",
        id.name
    );
    let Some(name) = type_in(TITLE, &prompt, &old.name, "Full name", "go on", false) else {
        return;
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        done(TITLE, identity::set_git(&id.slug, None));
        return;
    }
    let Some(email) = type_in(
        TITLE,
        "The email its commits carry.",
        &old.email,
        "name@example.com",
        "go on",
        false,
    ) else {
        return;
    };
    // Asked of Git Credential Manager while the name and email were typed:
    // it is a program run, too slow for the UI thread. Not back yet means
    // the user is typed in instead.
    let accounts = accounts.try_recv().unwrap_or_default();
    let prompt = "The GitHub user its pushes go as, from the ones Git Credential Manager keeps. Empty for whichever it picks.";
    let user = if accounts.is_empty() {
        type_in(
            TITLE,
            prompt,
            old.github_user.as_deref().unwrap_or(""),
            "GitHub user",
            "keep it",
            false,
        )
    } else {
        let offers: Vec<(String, String)> = accounts
            .iter()
            .map(|a| (a.clone(), String::new()))
            .collect();
        pick_one(TITLE, prompt, "keep it", &offers, true).map(|p| match p {
            Picked::Offer(i) => accounts[i].clone(),
            Picked::Typed(t) => t,
        })
    };
    let Some(user) = user else {
        return;
    };
    let user = user.trim();
    done(
        TITLE,
        identity::set_git(
            &id.slug,
            Some(Git {
                name,
                email: email.trim().to_string(),
                github_user: (!user.is_empty()).then(|| user.to_string()),
            }),
        ),
    );
}

fn folders_in(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn claims_of(id: &Identity) {
    const TITLE: &str = "Folders it claims";
    let text = if id.claims.is_empty() {
        format!("{} claims no folder. A project under one it claims is its own from its first session, unless the project picks.", id.name)
    } else {
        format!(
            "{} claims {}. The longest claim wins, and a project's own pick beats any.",
            id.name,
            id.claims.join(", ")
        )
    };
    let mut buttons = vec!["Add a folder"];
    if !id.claims.is_empty() {
        buttons.push("Remove one");
    }
    buttons.push("Done");
    match choose(Tone::Question, TITLE, &text, &buttons).map(|i| buttons[i]) {
        Some("Add a folder") => {
            let Some(shared) = shared() else {
                return;
            };
            let home = std::env::var("USERPROFILE").ok();
            let suggest = |text: &str| {
                crate::paths::offers(text, &[], home.as_deref(), folders_in, ask::LIST_ROWS)
                    .into_iter()
                    .map(|o| ask::Suggestion {
                        label: o.label,
                        detail: o.detail,
                        value: o.value,
                    })
                    .collect()
            };
            let check = |text: &str| {
                let dir = crate::paths::expand(text.trim(), home.as_deref());
                if dir.is_empty() {
                    Some("Type a folder".to_string())
                } else if !Path::new(&dir).is_dir() {
                    Some(format!("There is no folder {dir}"))
                } else {
                    None
                }
            };
            let pick = ask::Pick {
                suggest: &suggest,
                check: &check,
                browse: false,
                glyph: '\u{E8B7}',
                paths: true,
            };
            let question = ask::Ask {
                title: TITLE,
                prompt: "A folder whose projects are its own.",
                initial: "",
                placeholder: "C:\\path\\to\\folder",
                verb: "claim it",
                notes: false,
                secret: false,
                pick: Some(&pick),
            };
            if let Some(a) = ask::ask(shared, None, &question) {
                let dir = crate::paths::expand(a.text.trim(), home.as_deref());
                done(TITLE, identity::add_claim(&id.slug, &dir));
            }
        }
        Some("Remove one") => {
            let offers: Vec<(String, String)> = id
                .claims
                .iter()
                .map(|c| (c.clone(), String::new()))
                .collect();
            if let Some(Picked::Offer(i)) = pick_one(
                TITLE,
                "Which folder it claims no more.",
                "remove it",
                &offers,
                false,
            ) {
                done(TITLE, identity::remove_claim(&id.slug, &id.claims[i]));
            }
        }
        _ => {}
    }
}

fn tool_of(id: &Identity, tool: Tool, who: Option<Option<String>>) {
    let title = format!("{} for {}", tool.name(), id.name);
    let (text, buttons): (String, &[&str]) = match &who {
        None => (
            format!(
                "{}'s sessions use this PC's own {} login. Log in gives it one of its own, in a console of its own.",
                id.name,
                tool.name()
            ),
            &["Log in", "Keep"][..],
        ),
        Some(None) => (
            format!("Nobody is logged in to {} for {}. Remove points its sessions back at this PC's login.", tool.name(), id.name),
            &["Log in", "Remove", "Keep"][..],
        ),
        Some(Some(w)) if w.is_empty() => (
            "Logged in, but Vercel does not keep a name in its files. Who is it asks Vercel.".to_string(),
            &["Who is it", "Log in", "Remove", "Keep"][..],
        ),
        Some(Some(w)) => (
            format!("Logged in as {w}. Log in again to change it; Remove points its sessions back at this PC's login."),
            &["Log in", "Remove", "Keep"][..],
        ),
    };
    match choose(Tone::Question, &title, &text, buttons).map(|i| buttons[i]) {
        Some("Log in") => done(&title, identity::log_in(&id.slug, tool)),
        Some("Remove") => done(&title, identity::remove_tool(&id.slug, tool)),
        Some("Who is it") => {
            // A network call, off the UI thread; the row reads the name
            // from the file it leaves.
            let slug = id.slug.clone();
            std::thread::spawn(move || {
                identity::vercel_whoami(&slug);
                crate::settings::vault_changed();
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_are_reduced_to_origins() {
        assert_eq!(
            parse_sites("https://app.example.com/login?next=/ app.example.com").unwrap(),
            ["https://app.example.com"]
        );
        assert_eq!(
            parse_sites("http://localhost:3000, https://a.example").unwrap(),
            ["http://localhost:3000", "https://a.example"]
        );
        assert!(parse_sites("http://example.com").is_err());
        assert!(parse_sites("ftp://example.com").is_err());
        assert!(parse_sites("  ").is_err());
    }

    #[test]
    fn a_login_is_named_by_its_site() {
        assert_eq!(name_for("https://app.example.com"), "EXAMPLE");
        assert_eq!(name_for("https://my-site.example.com"), "EXAMPLE");
        assert_eq!(name_for("http://localhost:3000"), "LOCALHOST");
        assert_eq!(name_for("https://1password.com"), "SITE_1PASSWORD");
    }

    #[test]
    fn twin_labels_are_told_apart_by_their_detail() {
        let offers = [
            ("main.exe".to_string(), "C:\\a\\main.exe".to_string()),
            ("main.exe".to_string(), "C:\\b\\main.exe".to_string()),
            ("git.exe".to_string(), "C:\\git.exe".to_string()),
        ];
        assert_eq!(
            unique_labels(&offers),
            [
                "main.exe (C:\\a\\main.exe)",
                "main.exe (C:\\b\\main.exe)",
                "git.exe"
            ]
        );
        let same = [
            ("x".to_string(), String::new()),
            ("X".to_string(), String::new()),
        ];
        assert_eq!(unique_labels(&same), ["x", "X #2"]);
    }

    #[test]
    fn an_always_reads_by_program_project_and_content() {
        let a = horadric_core::vault::secret::approval(
            "c:/work/app",
            "c:/tools/aws.exe s3 ls",
            "ABCDEF0123456789",
        );
        assert_eq!(
            allowed_offer(&a),
            (
                "aws.exe s3 ls".to_string(),
                "for app, as it was at abcdef01".to_string()
            )
        );
        // One for a program no longer allowed Always says so.
        let gh = horadric_core::vault::secret::approval(
            "c:/work/app",
            "c:/tools/gh.exe pr create",
            "ab",
        );
        assert!(allowed_offer(&gh).1.contains("no longer used"));
        assert_eq!(allowed_offer("c:/work/site").0, "site");
    }

    #[test]
    fn only_a_slug_in_config_json_is_made() {
        assert!(makeable("opti-people"));
        assert!(!makeable("opti_people"));
    }

    #[test]
    fn a_seed_is_at_least_sixteen_base32_characters() {
        assert!(seed_ok("JBSWY3DPEHPK3PXP"));
        assert!(seed_ok("JBSW Y3DP EHPK 3PXP"));
        assert!(!seed_ok("JBSWY3DP"));
    }

    #[test]
    fn what_an_identity_keeps_is_counted() {
        assert_eq!(counted(2, 1), "2 secrets and a web login");
        assert_eq!(counted(1, 0), "a secret");
        assert_eq!(counted(0, 3), "3 web logins");
    }

    #[test]
    fn ago_says_the_largest_whole_unit() {
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 125), "2 min ago");
        assert_eq!(ago(0, 7300), "2 h ago");
        assert_eq!(ago(0, 90_000), "a day ago");
        assert_eq!(ago(0, 3 * 86_400), "3 days ago");
        assert_eq!(ago(200, 100), "just now");
    }
}
