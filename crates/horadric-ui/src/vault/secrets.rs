//! The Secrets section of the Settings window, as a model with no window
//! in it: the picked project's secrets and web logins in groups that fold,
//! a name filter, an item opened in place with what it is allowed for and
//! what can be done to it, and the forms that add or replace one, every
//! field at once. See docs/plan/vault.md, "Adding one".
//!
//! Nothing here holds a value but a [`Form`]'s fields while the human types
//! one, and a [`Save`] for the moment it takes to seal it. Both write zeros
//! over what they held when they go.

use std::borrow::Cow;

use horadric_core::vault::secret::{valid_name, Owner};

use super::identity::{Identity, Resolved};
use super::settings::{ago, allowed_offer, host, name_for, parse_sites, seed_ok, Row};
use super::store::{Kind, Listed, Used};
use crate::field::{self, Field as Text};
use crate::settings::{Control, Field, Line};

/// A kept secret or web login as the rows show it: never its value.
#[derive(Clone, Debug, Default)]
pub struct Kept {
    pub name: String,
    pub owner: Option<Owner>,
    /// Another of the same name beats it for this project.
    pub beaten: bool,
    /// Programs for a secret, project keys for a web login.
    pub allowed: Vec<String>,
    pub last: Option<Used>,
    pub origins: Vec<String>,
    pub user: String,
    pub totp: bool,
}

impl Kept {
    pub fn owner(&self) -> Owner {
        self.owner.clone().unwrap_or(Owner::Everywhere)
    }
}

/// Everything the rows show, read once and again after a change, not at
/// every paint: the identities file and the sealed store are files.
#[derive(Default)]
pub struct View {
    pub built: bool,
    /// The project it was read for, "" for none.
    pub key: String,
    pub resolved: Option<Resolved>,
    /// The human's pick on this machine, "" for none.
    pub pick: Option<String>,
    /// The slug the project's config.json names.
    pub file: Option<String>,
    pub secrets: Vec<Kept>,
    pub logins: Vec<Kept>,
    pub missing: Vec<String>,
    /// Why the secrets file would not open, which leaves every row empty.
    pub problem: Option<String>,
    pub ids: Vec<Identity>,
    /// Per identity, in `Tool::ALL`'s order: None when the tool is not
    /// its own, Some(None) when it is but nobody is logged in.
    pub who: Vec<[Option<Option<String>>; 3]>,
}

impl View {
    pub fn identity_name(&self, slug: &str) -> String {
        self.ids
            .iter()
            .find(|i| i.slug == slug)
            .map_or_else(|| slug.to_string(), |i| i.name.clone())
    }

    pub fn whose(&self, owner: &Owner) -> String {
        match owner {
            Owner::Project(_) => "this project's".into(),
            Owner::Identity(slug) => format!("{}'s", self.identity_name(slug)),
            Owner::Everywhere => "everywhere".into(),
        }
    }

    pub fn slug(&self) -> Option<String> {
        self.resolved
            .as_ref()
            .and_then(|r| r.slug())
            .map(String::from)
    }

    /// Takes the store's listing for the project `key` with the identity
    /// `slug`, and the names its config.json asks for: what is the
    /// project's, its identity's or Everywhere's, the project's own first,
    /// and the names asked for that none of them has.
    pub fn fill(
        &mut self,
        key: &str,
        slug: Option<&str>,
        listing: Vec<Listed>,
        wanted: Vec<String>,
    ) {
        let ours = |o: &Owner| match o {
            Owner::Project(k) => k == key,
            Owner::Identity(s) => Some(s.as_str()) == slug,
            Owner::Everywhere => true,
        };
        let (mut secrets, mut logins): (Vec<Kept>, Vec<Kept>) = (Vec::new(), Vec::new());
        for l in listing {
            if !ours(&l.owner) {
                continue;
            }
            let kind = l.kind;
            let k = Kept {
                name: l.name,
                owner: Some(l.owner),
                beaten: l.beaten,
                allowed: l.allowed,
                last: l.last_used,
                origins: l.origins,
                user: l.user,
                totp: l.has_totp,
            };
            match kind {
                Kind::Secret => secrets.push(k),
                Kind::WebLogin => logins.push(k),
            }
        }
        for list in [&mut secrets, &mut logins] {
            list.sort_by(|a, b| {
                (rank(&a.owner()), a.name.as_str()).cmp(&(rank(&b.owner()), b.name.as_str()))
            });
        }
        self.missing = wanted
            .into_iter()
            .filter(|n| !secrets.iter().chain(&logins).any(|k| k.name == *n))
            .collect();
        self.secrets = secrets;
        self.logins = logins;
    }

    fn item(&self, kind: Kind, i: u16) -> Option<&Kept> {
        match kind {
            Kind::Secret => self.secrets.get(i as usize),
            Kind::WebLogin => self.logins.get(i as usize),
        }
    }

    /// Where the item `id` stands in the view, None when it is not in it.
    pub fn find(&self, id: &ItemId) -> Option<u16> {
        let list = match id.kind {
            Kind::Secret => &self.secrets,
            Kind::WebLogin => &self.logins,
        };
        list.iter()
            .position(|k| k.name == id.name && k.owner() == id.owner)
            .map(|i| i as u16)
    }
}

fn rank(owner: &Owner) -> u8 {
    match owner {
        Owner::Project(_) => 0,
        Owner::Identity(_) => 1,
        Owner::Everywhere => 2,
    }
}

/// One kept item by what it is, which outlasts the view's order: a view
/// read again after a change may hold it at another place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemId {
    pub kind: Kind,
    pub name: String,
    pub owner: Owner,
}

/// The item a row is about, by its place in the view.
pub fn item_at(v: &View, row: Row) -> Option<ItemId> {
    let (kind, i) = match row {
        Row::Secret(i) => (Kind::Secret, i),
        Row::Login(i) => (Kind::WebLogin, i),
        _ => return None,
    };
    let k = v.item(kind, i)?;
    Some(ItemId {
        kind,
        name: k.name.clone(),
        owner: k.owner(),
    })
}

/// The groups the section shows its items in, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Project,
    Identity,
    Everywhere,
    Logins,
    Missing,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Project,
        Group::Identity,
        Group::Everywhere,
        Group::Logins,
        Group::Missing,
    ];

    fn title(self, v: &View) -> String {
        match self {
            Group::Project => "This project".into(),
            Group::Identity => v
                .slug()
                .map_or_else(|| "Its identity".into(), |s| v.identity_name(&s)),
            Group::Everywhere => "Everywhere".into(),
            Group::Logins => "Web logins".into(),
            Group::Missing => "Missing: named in config.json, not kept".into(),
        }
    }

    /// The group the item `id` stands in.
    fn of(id: &ItemId) -> Group {
        match (id.kind, &id.owner) {
            (Kind::WebLogin, _) => Group::Logins,
            (_, Owner::Project(_)) => Group::Project,
            (_, Owner::Identity(_)) => Group::Identity,
            (_, Owner::Everywhere) => Group::Everywhere,
        }
    }
}

/// Whether `name` is one the filter `filter`, lower case and trimmed,
/// lets through.
fn passes(name: &str, filter: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(filter)
}

/// The places in the view of the items in group `g` that the filter lets
/// through: secrets for an owner's group, logins, or missing names.
pub fn members(v: &View, g: Group, filter: &str) -> Vec<u16> {
    let filter = filter.trim().to_lowercase();
    let keep = |name: &str| passes(name, &filter);
    let owned = |want: fn(&Owner) -> bool| -> Vec<u16> {
        v.secrets
            .iter()
            .enumerate()
            .filter(|(_, k)| want(&k.owner()) && keep(&k.name))
            .map(|(i, _)| i as u16)
            .collect()
    };
    match g {
        Group::Project => owned(|o| matches!(o, Owner::Project(_))),
        Group::Identity => owned(|o| matches!(o, Owner::Identity(_))),
        Group::Everywhere => owned(|o| matches!(o, Owner::Everywhere)),
        Group::Logins => (0..v.logins.len())
            .filter(|i| keep(&v.logins[*i].name))
            .map(|i| i as u16)
            .collect(),
        Group::Missing => (0..v.missing.len())
            .filter(|i| keep(&v.missing[*i]))
            .map(|i| i as u16)
            .collect(),
    }
}

/// The row of item `i` of group `g`.
fn member_row(g: Group, i: u16) -> Row {
    match g {
        Group::Logins => Row::Login(i),
        Group::Missing => Row::Missing(i),
        _ => Row::Secret(i),
    }
}

/// "5 secrets, 2 web logins, 1 missing", for the Projects section's row.
pub fn summary(v: &View) -> String {
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(v.secrets.len(), "secret", "secrets"),
        count(v.logins.len(), "web login", "web logins"),
        count(v.missing.len(), "missing", "missing"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "None kept, add one".into()
    } else {
        parts.join(", ")
    }
}

/// A field of a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Name,
    Value,
    Sites,
    User,
    Password,
    Seed,
    /// Not a field: whose it is, which a problem can be about.
    Whose,
}

impl Slot {
    pub fn label(self) -> &'static str {
        match self {
            Slot::Name => "Name",
            Slot::Value => "Value",
            Slot::Sites => "Sites",
            Slot::User => "User",
            Slot::Password => "Password",
            Slot::Seed => "One time code",
            Slot::Whose => "Whose",
        }
    }

    /// Drawn as dots, kept off the clipboard, pasted whole or not at all.
    pub fn secret(self) -> bool {
        matches!(self, Slot::Value | Slot::Password | Slot::Seed)
    }

    fn max(self) -> usize {
        match self {
            Slot::Name => 64,
            Slot::Value | Slot::Password => crate::ask::MAX_SECRET,
            Slot::Sites => 2000,
            Slot::User | Slot::Seed => 256,
            Slot::Whose => 0,
        }
    }

    /// Shown in the field while it is empty.
    pub fn placeholder(self, purpose: &Purpose) -> &'static str {
        match (self, purpose) {
            (Slot::Name, _) => "GITHUB_TOKEN",
            (Slot::Value, _) => "Paste it here, one line",
            (Slot::Sites, _) => "https://app.example.com",
            (Slot::User, _) => "The name or email you sign in with",
            (Slot::Seed, Purpose::Replace(..)) => "The new seed, or none to take it away",
            (Slot::Seed, _) => "Optional: the text under the QR code",
            _ => "",
        }
    }
}

/// Whose a new item is, as the form's buttons offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    Project,
    Identity,
    Everywhere,
}

/// The project a form was started in, kept with it: a draft saves where
/// it was begun, whatever project is picked when Save is pressed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Home {
    pub key: String,
    /// Its identity's slug and name, when it has one.
    pub slug: Option<String>,
    pub identity: Option<String>,
    /// Its name, as the form's title says it.
    pub project: String,
}

impl Home {
    pub fn of(v: &View) -> Home {
        let slug = v.slug();
        Home {
            key: v.key.clone(),
            identity: slug.as_ref().map(|s| v.identity_name(s)),
            slug,
            project: super::settings::short(&v.key).to_string(),
        }
    }

    /// Whose `owner` is, in words: a project, an identity, or every one.
    fn owner_words(&self, owner: &Owner) -> String {
        match owner {
            Owner::Project(k) => super::settings::short(k).to_string(),
            Owner::Identity(s) if Some(s) == self.slug.as_ref() => {
                self.identity.clone().unwrap_or_else(|| s.clone())
            }
            Owner::Identity(s) => s.clone(),
            Owner::Everywhere => "every project".into(),
        }
    }
}

/// The owners offered for the project `home`: its identity only when it
/// has one.
pub fn offers(home: &Home) -> Vec<(Whose, String)> {
    let mut out = vec![(Whose::Project, "This project".to_string())];
    if let Some(name) = &home.identity {
        out.push((Whose::Identity, name.clone()));
    }
    out.push((Whose::Everywhere, "Everywhere".into()));
    out
}

fn owner_for(w: Whose, key: &str, slug: Option<&str>) -> Option<Owner> {
    match w {
        Whose::Project if !key.is_empty() => Some(Owner::Project(key.to_string())),
        Whose::Project => None,
        Whose::Identity => slug.map(|s| Owner::Identity(s.to_string())),
        Whose::Everywhere => Some(Owner::Everywhere),
    }
}

/// What a form is for.
#[derive(Clone, PartialEq)]
pub enum Purpose {
    AddSecret,
    AddLogin,
    /// A new value, password or seed for a kept item.
    Replace(ItemId, Slot),
}

/// A form open in the section: its fields, typed into in place, and
/// whose the item will be. It lasts until it is saved or cancelled or the
/// window closes, whatever is clicked meanwhile. No Debug: its fields
/// hold what the human types.
pub struct Form {
    pub purpose: Purpose,
    /// The project it was started in, which it saves for.
    pub home: Home,
    /// Counts every change, so a value shown in clear is laid out again
    /// only when it changes, not copied out at every paint.
    pub rev: u64,
    pub fields: Vec<(Slot, Text)>,
    pub whose: Whose,
    /// The secret fields show what is typed.
    pub shown: bool,
    /// Save was pressed: every problem shows, not only those of fields
    /// typed in.
    pub tried: bool,
    /// The name was typed, so a web login's no longer follows its site.
    pub named: bool,
    /// Why the last paste or keystroke was refused.
    pub refusal: Option<String>,
}

impl Drop for Form {
    fn drop(&mut self) {
        for (_, f) in &mut self.fields {
            f.wipe();
        }
    }
}

impl Form {
    fn with(home: &Home, purpose: Purpose, slots: &[Slot]) -> Self {
        Form {
            purpose,
            home: home.clone(),
            rev: 0,
            fields: slots
                .iter()
                .map(|s| (*s, Text::new("", false, s.max())))
                .collect(),
            whose: Whose::Project,
            shown: false,
            tried: false,
            named: false,
            refusal: None,
        }
    }

    /// Adding a secret, named `name` when config.json asked for it.
    pub fn add_secret(home: &Home, name: &str) -> Self {
        let mut f = Form::with(home, Purpose::AddSecret, &[Slot::Name, Slot::Value]);
        if let Some(t) = f.field_mut(Slot::Name) {
            t.insert(name);
        }
        f.named = !name.is_empty();
        f
    }

    pub fn add_login(home: &Home) -> Self {
        Form::with(
            home,
            Purpose::AddLogin,
            &[
                Slot::Sites,
                Slot::Name,
                Slot::User,
                Slot::Password,
                Slot::Seed,
            ],
        )
    }

    /// A new `slot` for the kept item `item`.
    pub fn replace(home: &Home, item: ItemId, slot: Slot) -> Self {
        Form::with(home, Purpose::Replace(item, slot), &[slot])
    }

    pub fn field(&self, slot: Slot) -> Option<&Text> {
        self.fields.iter().find(|(s, _)| *s == slot).map(|(_, t)| t)
    }

    pub fn field_mut(&mut self, slot: Slot) -> Option<&mut Text> {
        self.fields
            .iter_mut()
            .find(|(s, _)| *s == slot)
            .map(|(_, t)| t)
    }

    fn text(&self, slot: Slot) -> &str {
        self.field(slot).map_or("", |t| t.text.as_str())
    }

    /// The field the keyboard starts in: the first still empty.
    pub fn first(&self) -> Slot {
        self.fields
            .iter()
            .find(|(_, t)| t.text.is_empty())
            .or(self.fields.first())
            .map_or(Slot::Name, |(s, _)| *s)
    }

    /// The item it replaces something of.
    pub fn item(&self) -> Option<&ItemId> {
        match &self.purpose {
            Purpose::Replace(id, _) => Some(id),
            _ => None,
        }
    }

    /// What it is for, naming the project it saves for, or for a
    /// replacement whose item it is, so a draft seen from another project
    /// says where it goes.
    fn title(&self) -> String {
        let p = &self.home.project;
        match &self.purpose {
            Purpose::AddSecret => {
                format!("A new secret for {p}: its sessions use it, never read it")
            }
            Purpose::AddLogin => format!("A new web login for {p}: its sessions never read it"),
            Purpose::Replace(id, slot) => {
                let what = match slot {
                    Slot::Value => "value",
                    Slot::Password => "password",
                    _ => "one time code seed",
                };
                let whose = self.home.owner_words(&id.owner);
                let mut t = format!("A new {what} for {}, kept for {whose}", id.name);
                if *slot == Slot::Value {
                    t.push_str(". Programs allowed always are asked again");
                }
                t
            }
        }
    }

    /// A field was typed in: a web login's name follows its first site
    /// until the name is typed itself.
    pub fn edited(&mut self, slot: Slot) {
        self.refusal = None;
        self.rev += 1;
        match slot {
            Slot::Name => self.named = !self.text(Slot::Name).is_empty(),
            Slot::Sites if self.purpose == Purpose::AddLogin && !self.named => {
                let name = parse_sites(self.text(Slot::Sites))
                    .ok()
                    .and_then(|o| o.first().map(|o| name_for(o)))
                    .unwrap_or_default();
                if let Some(t) = self.field_mut(Slot::Name) {
                    t.select_all();
                    t.insert(&name);
                }
            }
            _ => {}
        }
    }
}

/// What a form saves, the values in it until it is dropped.
#[derive(PartialEq)]
pub enum Save {
    Secret {
        name: String,
        owner: Owner,
        value: String,
    },
    Login {
        name: String,
        owner: Owner,
        origins: Vec<String>,
        user: String,
        password: String,
        seed: Option<String>,
    },
    Value {
        item: ItemId,
        value: String,
    },
    Password {
        item: ItemId,
        password: String,
    },
    /// None takes the kept seed away.
    Seed {
        item: ItemId,
        seed: Option<String>,
    },
}

/// Names only: a value never reaches a log or a panic message.
impl std::fmt::Debug for Save {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Save::Secret { name, owner, .. } => write!(f, "Secret({name}, {owner:?})"),
            Save::Login { name, owner, .. } => write!(f, "Login({name}, {owner:?})"),
            Save::Value { item, .. } => write!(f, "Value({})", item.name),
            Save::Password { item, .. } => write!(f, "Password({})", item.name),
            Save::Seed { item, seed } => write!(f, "Seed({}, kept {})", item.name, seed.is_some()),
        }
    }
}

impl Drop for Save {
    fn drop(&mut self) {
        let zero = |s: &mut String| field::zero(s);
        match self {
            Save::Secret { value, .. } | Save::Value { value, .. } => zero(value),
            Save::Login { password, seed, .. } => {
                zero(password);
                if let Some(s) = seed {
                    zero(s);
                }
            }
            Save::Password { password, .. } => zero(password),
            Save::Seed { seed, .. } => {
                if let Some(s) = seed {
                    zero(s);
                }
            }
        }
    }
}

/// Everything wrong with the form, by field.
pub fn problems(form: &Form) -> Vec<(Slot, String)> {
    let offered = offers(&form.home);
    let mut out = Vec::new();
    let replacing = matches!(form.purpose, Purpose::Replace(..));
    for (slot, t) in &form.fields {
        let text = t.text.as_str();
        let why = match slot {
            Slot::Name if text.trim().is_empty() => Some("Give it a name".to_string()),
            Slot::Name if !valid_name(text.trim()) => Some(
                "Upper case letters, digits and _, a letter first, at most 64, as GITHUB_TOKEN"
                    .to_string(),
            ),
            Slot::Value if text.trim().is_empty() => Some("Type or paste its value".into()),
            Slot::Sites => parse_sites(text).err(),
            Slot::Password if text.is_empty() => Some("Type or paste the password".into()),
            Slot::Seed => seed_problem(text, replacing),
            _ => None,
        };
        if let Some(w) = why {
            out.push((*slot, w));
        }
    }
    if !replacing && !offered.iter().any(|(w, _)| *w == form.whose) {
        out.push((Slot::Whose, "Pick whose it is".into()));
    }
    out
}

fn seed_problem(text: &str, replacing: bool) -> Option<String> {
    let t = text.trim();
    match t {
        "" if replacing => Some("Type the new seed, or none to take it away".into()),
        "" => None,
        t if replacing && t.eq_ignore_ascii_case("none") => None,
        t if seed_ok(t) => None,
        _ => Some("Not a seed: at least 16 letters A to Z and digits 2 to 7".into()),
    }
}

/// The problems to show now: those of the fields typed in, and after Save
/// was pressed every one.
pub fn shown_problems(form: &Form) -> Vec<(Slot, String)> {
    problems(form)
        .into_iter()
        .filter(|(s, _)| form.tried || !form.text(*s).trim().is_empty())
        .collect()
}

/// What Save does with the form, for the project it was started in, or
/// what stops it. Each value is copied once, trimmed, straight into the
/// Save, which zeroes it when dropped.
pub fn decide(form: &Form) -> Result<Save, Vec<(Slot, String)>> {
    let wrong = problems(form);
    if !wrong.is_empty() {
        return Err(wrong);
    }
    let home = &form.home;
    let trimmed = |s: Slot| form.text(s).trim().to_string();
    let owner = || {
        owner_for(form.whose, &home.key, home.slug.as_deref())
            .ok_or_else(|| vec![(Slot::Whose, "Pick whose it is".to_string())])
    };
    let seed = |keep_none: bool| {
        let t = form.text(Slot::Seed).trim();
        let none = keep_none && t.eq_ignore_ascii_case("none");
        (!t.is_empty() && !none).then(|| t.to_string())
    };
    Ok(match &form.purpose {
        Purpose::AddSecret => Save::Secret {
            name: trimmed(Slot::Name),
            owner: owner()?,
            value: trimmed(Slot::Value),
        },
        Purpose::AddLogin => Save::Login {
            name: trimmed(Slot::Name),
            owner: owner()?,
            origins: parse_sites(form.text(Slot::Sites)).unwrap_or_default(),
            user: trimmed(Slot::User),
            password: form.text(Slot::Password).to_string(),
            seed: seed(false),
        },
        Purpose::Replace(item, Slot::Value) => Save::Value {
            item: item.clone(),
            value: trimmed(Slot::Value),
        },
        Purpose::Replace(item, Slot::Password) => Save::Password {
            item: item.clone(),
            password: form.text(Slot::Password).to_string(),
        },
        Purpose::Replace(item, _) => Save::Seed {
            item: item.clone(),
            seed: seed(true),
        },
    })
}

/// A field's text as drawn: as it is, or one dot for each UTF-16 unit so
/// the caret's offsets still line up.
pub fn display(t: &Text, hidden: bool) -> String {
    if !hidden {
        return t.text.clone();
    }
    t.text
        .chars()
        .flat_map(|c| (0..c.len_utf16()).map(|_| '\u{2022}'))
        .collect()
}

/// The section's state while the Settings window is open: which groups
/// are folded, the filter, the item opened and the form being filled in.
/// No Debug, for the form's sake.
pub struct Ui {
    pub folded: Vec<Group>,
    pub filter: Text,
    pub expanded: Option<ItemId>,
    /// The opened item's Forget was pressed and waits to be confirmed.
    pub confirm: bool,
    pub form: Option<Form>,
    /// Why the last change to the store failed.
    pub failure: Option<String>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            folded: Vec::new(),
            filter: Text::new("", false, 64),
            expanded: None,
            confirm: false,
            form: None,
            failure: None,
        }
    }
}

impl Ui {
    pub fn fold(&mut self, g: Group) {
        match self.folded.iter().position(|x| *x == g) {
            Some(i) => {
                self.folded.remove(i);
            }
            None => self.folded.push(g),
        }
    }

    /// Opens the item `id`, or closes it when it is open.
    pub fn open(&mut self, id: ItemId) {
        self.confirm = false;
        self.failure = None;
        self.expanded = match &self.expanded {
            Some(e) if *e == id => None,
            _ => Some(id),
        };
    }

    /// Starts `form`, unless one is open already: a draft is never
    /// dropped for another. False then.
    pub fn start(&mut self, form: Form) -> bool {
        if self.form.is_some() {
            return false;
        }
        self.failure = None;
        self.form = Some(form);
        true
    }

    /// Shows the item `id` wherever it is: no filter, its group unfolded,
    /// it opened.
    pub fn reveal(&mut self, id: ItemId) {
        self.filter.wipe();
        self.folded.retain(|g| *g != Group::of(&id));
        self.confirm = false;
        self.expanded = Some(id);
    }
}

fn line(row: Row, label: impl Into<Cow<'static, str>>, value: String, control: Control) -> Line {
    Line {
        field: Field::Vault(row),
        label: label.into(),
        value,
        control,
        help: "",
        parts: Vec::new(),
        nested: false,
    }
}

fn note(row: Row, value: String) -> Line {
    line(row, "", value, Control::Note)
}

fn nested(mut l: Line) -> Line {
    l.nested = true;
    l
}

fn buttons(row: Row, value: String, parts: &[&str]) -> Line {
    Line {
        parts: parts.iter().map(|p| p.to_string()).collect(),
        ..line(row, "", value, Control::Buttons)
    }
}

/// How long ago it was used, and by whom for what.
fn last_use(last: &Option<Used>, now: u64) -> String {
    match last {
        Some(u) => format!(
            "Last used {} by session {} for {}",
            ago(u.at, now),
            u.session.get(..8).unwrap_or(&u.session),
            super::settings::short(&u.what)
        ),
        None => "Never used yet".into(),
    }
}

fn used_short(last: &Option<Used>, now: u64) -> String {
    match last {
        Some(u) => format!("used {}", ago(u.at, now)),
        None => "never used".into(),
    }
}

/// The row of a kept item or a missing name.
fn item_line(v: &View, row: Row, open: bool, now: u64) -> Option<Line> {
    let l = match row {
        Row::Secret(i) => {
            let k = v.secrets.get(i as usize)?;
            let mut value = used_short(&k.last, now);
            if k.beaten {
                value.push_str(", another wins");
            }
            line(row, k.name.clone(), value, Control::Item(open))
        }
        Row::Login(i) => {
            let k = v.logins.get(i as usize)?;
            let site = k.origins.first().map_or("", |o| host(o));
            let mut value = format!(
                "{site}, {}, {}",
                v.whose(&k.owner()),
                used_short(&k.last, now)
            );
            if k.beaten {
                value.push_str(", another wins");
            }
            line(row, k.name.clone(), value, Control::Item(open))
        }
        Row::Missing(i) => {
            let n = v.missing.get(i as usize)?;
            line(
                row,
                n.clone(),
                "not kept, add it".into(),
                Control::Item(false),
            )
        }
        _ => return None,
    };
    Some(l)
}

/// The rows of an opened item: whose it is, when it was last used, what
/// it is allowed for, each with its own Remove, and what can be done.
fn detail(v: &View, kind: Kind, k: &Kept, ui: &Ui, now: u64, out: &mut Vec<Line>) {
    let mut says = |s: String| out.push(nested(note(Row::Detail, s)));
    let whose = match k.owner() {
        Owner::Project(_) => "Kept for this project".to_string(),
        Owner::Identity(s) => format!(
            "Kept for {}, so for every project of it",
            v.identity_name(&s)
        ),
        Owner::Everywhere => "Kept for every project".to_string(),
    };
    says(whose);
    if kind == Kind::WebLogin {
        let sites: Vec<&str> = k.origins.iter().map(|o| host(o)).collect();
        says(format!("Signs in to {} as {}", sites.join(", "), k.user));
        says(if k.totp {
            "Keeps a one time code seed too".into()
        } else {
            "No one time code seed".into()
        });
    }
    says(last_use(&k.last, now));
    if k.beaten {
        says("Another of the same name wins for this project".into());
    }
    match (kind, k.allowed.is_empty()) {
        (Kind::Secret, true) => says("Allowed always for no program: every use asks".into()),
        (Kind::WebLogin, true) => says("Allowed always for no project: every fill asks".into()),
        (Kind::Secret, false) => says("Allowed always for these, until removed:".into()),
        (Kind::WebLogin, false) => says("Allowed always for these projects, until removed:".into()),
    }
    for (j, a) in k.allowed.iter().enumerate() {
        let (what, detail) = allowed_offer(a);
        out.push(nested(Line {
            parts: vec!["Remove".into()],
            ..line(Row::Allowed(j as u16), what, detail, Control::Removable)
        }));
    }
    let actions = if ui.confirm {
        buttons(
            Row::Actions,
            format!("Forget {}?", k.name),
            &confirm_parts(kind),
        )
    } else if kind == Kind::Secret {
        buttons(Row::Actions, String::new(), &["Replace value", "Forget"])
    } else {
        buttons(
            Row::Actions,
            String::new(),
            &["Replace password", "Replace seed", "Forget"],
        )
    };
    out.push(nested(actions));
}

/// The buttons Forget turns into, Keep it where Forget stood: a second
/// click or Enter on the same spot keeps, never forgets. A secret's Forget
/// is its second button, a web login's its last, at the right.
pub fn confirm_parts(kind: Kind) -> [&'static str; 2] {
    match kind {
        Kind::Secret => ["Keep it", "Forget it"],
        Kind::WebLogin => ["Forget it", "Keep it"],
    }
}

/// Which of [`confirm_parts`] keeps.
pub fn keep_part(kind: Kind) -> usize {
    confirm_parts(kind)
        .iter()
        .position(|p| *p == "Keep it")
        .unwrap_or(0)
}

/// A form's rows: what it is for, its fields each with what is wrong with
/// it under it, whose it is, and Save and Cancel.
pub fn form_lines(form: &Form, indent: bool, out: &mut Vec<Line>) {
    let offered = offers(&form.home);
    let wrong = shown_problems(form);
    let mut push = |l: Line| out.push(if indent { nested(l) } else { l });
    push(note(Row::FormTitle, form.title()));
    let says = |slot: Slot| {
        wrong
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, w)| line(Row::Wrong(slot), "", w.clone(), Control::Error))
    };
    for (slot, _) in &form.fields {
        push(line(
            Row::Input(*slot),
            slot.label(),
            String::new(),
            Control::Input {
                secret: slot.secret(),
                shown: form.shown,
            },
        ));
        if let Some(l) = says(*slot) {
            push(l);
        }
    }
    if form.item().is_none() {
        let at = offered
            .iter()
            .position(|(w, _)| *w == form.whose)
            .unwrap_or(usize::MAX);
        push(Line {
            parts: offered.iter().map(|(_, n)| n.clone()).collect(),
            ..line(Row::Whose, "Whose", String::new(), Control::Segments(at))
        });
        if let Some(l) = says(Slot::Whose) {
            push(l);
        }
    }
    if let Some(why) = &form.refusal {
        push(line(Row::Refused, "", why.clone(), Control::Error));
    }
    push(buttons(
        Row::FormButtons,
        "Enter saves, Esc cancels".into(),
        &["Save", "Cancel"],
    ));
}

/// The Secrets section's rows for the project of `v`, below its picker.
pub fn lines(v: &View, ui: &Ui, now: u64) -> Vec<Line> {
    let mut out = Vec::new();
    if let Some(why) = &v.problem {
        out.push(line(
            Row::Problem,
            "",
            format!("The secrets file will not open: {why}"),
            Control::Error,
        ));
    }
    out.push(line(
        Row::Filter,
        "Find by name",
        String::new(),
        Control::Input {
            secret: false,
            shown: false,
        },
    ));
    out.push(buttons(
        Row::Adds,
        String::new(),
        &["Add a secret", "Add a web login"],
    ));
    let replacing = ui.form.as_ref().and_then(Form::item);
    // A draft shows where it was started, under its item, and at the top
    // when that item is not in this project's view: it is never hidden.
    let at_item = replacing.and_then(|id| v.find(id).map(|i| (id, i)));
    if let (Some(form), None) = (&ui.form, at_item) {
        form_lines(form, false, &mut out);
    }
    if let Some(why) = &ui.failure {
        out.push(line(Row::Failed, "", why.clone(), Control::Error));
    }
    let filter = ui.filter.text.trim().to_lowercase();
    let mut any = false;
    for g in Group::ALL {
        let mut ids = members(v, g, &filter);
        let forced = at_item.filter(|(id, _)| Group::of(id) == g);
        if let Some((_, i)) = forced {
            if !ids.contains(&i) {
                ids.push(i);
                ids.sort_unstable();
            }
        }
        if ids.is_empty() {
            continue;
        }
        any = true;
        let open = !ui.folded.contains(&g) || !filter.is_empty() || forced.is_some();
        out.push(line(
            Row::Group(g),
            g.title(v),
            ids.len().to_string(),
            Control::Fold(open),
        ));
        if !open {
            continue;
        }
        for i in ids {
            let row = member_row(g, i);
            let id = item_at(v, row);
            let expanded = id.is_some() && ui.expanded == id;
            let drafting = id.is_some() && replacing == id.as_ref();
            let Some(l) = item_line(v, row, expanded || drafting, now) else {
                continue;
            };
            out.push(l);
            let Some(id) = id else {
                continue;
            };
            if let (true, Some(k)) = (expanded, v.item(id.kind, i)) {
                detail(v, id.kind, k, ui, now, &mut out);
            }
            if let (true, Some(form)) = (drafting, &ui.form) {
                form_lines(form, true, &mut out);
            }
        }
    }
    if !any {
        let said = if filter.is_empty() {
            "Nothing kept for this project yet"
        } else {
            "Nothing kept here has that in its name"
        };
        out.push(note(Row::Detail, said.into()));
    }
    out
}

/// Every kept item and missing name of the view, one row each, for the
/// window's search to find by name whatever is folded or filtered.
pub fn items(v: &View, now: u64) -> Vec<Line> {
    let rows = (0..v.secrets.len())
        .map(|i| Row::Secret(i as u16))
        .chain((0..v.logins.len()).map(|i| Row::Login(i as u16)))
        .chain((0..v.missing.len()).map(|i| Row::Missing(i as u16)));
    rows.filter_map(|r| item_line(v, r, false, now)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "c:/work/app";

    fn listed(name: &str, owner: Owner, kind: Kind) -> Listed {
        Listed {
            name: name.into(),
            owner,
            kind,
            allowed: Vec::new(),
            last_used: None,
            origins: if kind == Kind::WebLogin {
                vec!["https://app.example.com".into()]
            } else {
                Vec::new()
            },
            user: if kind == Kind::WebLogin {
                "me@example.com".into()
            } else {
                String::new()
            },
            has_totp: false,
            beaten: false,
        }
    }

    /// A project with the identity acme, and kept for it: two secrets of
    /// its own, one of acme's, one Everywhere, a web login, and things
    /// kept for others that it must not see. Its config.json asks for
    /// TOKEN and NPM_TOKEN.
    fn view() -> View {
        let mut v = View {
            built: true,
            key: KEY.into(),
            resolved: Some(Resolved::Identity("acme".into())),
            ids: vec![Identity {
                slug: "acme".into(),
                name: "Acme".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut used = listed("TOKEN", Owner::Project(KEY.into()), Kind::Secret);
        used.last_used = Some(Used {
            at: 1000,
            session: "0123456789abcdef".into(),
            what: "c:/tools/aws.exe".into(),
        });
        used.allowed = vec!["c:/work/app".into()];
        let mut beaten = listed("TOKEN", Owner::Everywhere, Kind::Secret);
        beaten.beaten = true;
        let listing = vec![
            listed("ZED", Owner::Project(KEY.into()), Kind::Secret),
            used,
            listed("GH", Owner::Identity("acme".into()), Kind::Secret),
            beaten,
            listed("OTHER", Owner::Project("c:/work/site".into()), Kind::Secret),
            listed("THEIRS", Owner::Identity("bob".into()), Kind::Secret),
            listed("EXAMPLE", Owner::Everywhere, Kind::WebLogin),
        ];
        v.fill(
            KEY,
            Some("acme"),
            listing,
            vec!["TOKEN".into(), "NPM_TOKEN".into()],
        );
        v
    }

    fn rows(lines: &[Line]) -> Vec<Row> {
        lines
            .iter()
            .map(|l| match l.field {
                Field::Vault(r) => r,
                f => panic!("{f:?} is not the Vault's"),
            })
            .collect()
    }

    #[test]
    fn a_view_keeps_only_what_the_project_sees_its_own_first() {
        let v = view();
        let names: Vec<&str> = v.secrets.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["TOKEN", "ZED", "GH", "TOKEN"]);
        assert_eq!(v.logins.len(), 1);
        // TOKEN is kept, so only NPM_TOKEN is missing.
        assert_eq!(v.missing, ["NPM_TOKEN"]);
        assert_eq!(summary(&v), "4 secrets, 1 web login, 1 missing");
        assert_eq!(summary(&View::default()), "None kept, add one");
    }

    #[test]
    fn items_stand_in_groups_by_whose_they_are() {
        let v = view();
        assert_eq!(members(&v, Group::Project, ""), [0, 1]);
        assert_eq!(members(&v, Group::Identity, ""), [2]);
        assert_eq!(members(&v, Group::Everywhere, ""), [3]);
        assert_eq!(members(&v, Group::Logins, ""), [0]);
        assert_eq!(members(&v, Group::Missing, ""), [0]);
        // The filter goes by name, whatever the case.
        assert_eq!(members(&v, Group::Project, " tok"), [0]);
        assert!(members(&v, Group::Identity, "tok").is_empty());
        assert_eq!(Group::Identity.title(&v), "Acme");
    }

    #[test]
    fn the_section_reads_as_groups_with_their_counts() {
        let v = view();
        let ui = Ui::default();
        let l = lines(&v, &ui, 5000);
        assert_eq!(
            rows(&l),
            [
                Row::Filter,
                Row::Adds,
                Row::Group(Group::Project),
                Row::Secret(0),
                Row::Secret(1),
                Row::Group(Group::Identity),
                Row::Secret(2),
                Row::Group(Group::Everywhere),
                Row::Secret(3),
                Row::Group(Group::Logins),
                Row::Login(0),
                Row::Group(Group::Missing),
                Row::Missing(0),
            ]
        );
        assert_eq!(l[2].label, "This project");
        assert_eq!(l[2].value, "2");
        assert_eq!(l[2].control, Control::Fold(true));
        assert_eq!(l[3].label, "TOKEN");
        assert_eq!(l[3].value, "used 1 h ago");
        assert_eq!(l[8].value, "never used, another wins");
        assert_eq!(l[10].value, "app.example.com, everywhere, never used");
        // No row says a value: there is none in the view to say.
        assert!(l.iter().all(|x| !x.value.contains("hunter2")));
    }

    #[test]
    fn a_folded_group_keeps_its_heading_and_a_filter_opens_it() {
        let v = view();
        let mut ui = Ui::default();
        ui.fold(Group::Project);
        let l = lines(&v, &ui, 0);
        assert_eq!(
            rows(&l)[2..4],
            [Row::Group(Group::Project), Row::Group(Group::Identity)]
        );
        assert_eq!(l[2].control, Control::Fold(false));
        ui.filter.insert("zed");
        let l = lines(&v, &ui, 0);
        assert_eq!(rows(&l)[2..], [Row::Group(Group::Project), Row::Secret(1)]);
        ui.filter.select_all();
        ui.filter.insert("nothing");
        assert_eq!(rows(&lines(&v, &ui, 0))[2..], [Row::Detail]);
        ui.fold(Group::Project);
        assert!(ui.folded.is_empty());
    }

    #[test]
    fn an_opened_item_says_whose_when_and_what_it_is_allowed_for() {
        let v = view();
        let mut ui = Ui::default();
        ui.open(item_at(&v, Row::Secret(0)).unwrap());
        let l = lines(&v, &ui, 5000);
        let at = rows(&l).iter().position(|r| *r == Row::Secret(0)).unwrap();
        assert_eq!(l[at].control, Control::Item(true));
        let block: Vec<&Line> = l[at + 1..].iter().take_while(|x| x.nested).collect();
        assert_eq!(block[0].value, "Kept for this project");
        assert_eq!(
            block[1].value,
            "Last used 1 h ago by session 01234567 for aws.exe"
        );
        let allowed = block
            .iter()
            .find(|x| x.control == Control::Removable)
            .unwrap();
        assert_eq!(allowed.field, Field::Vault(Row::Allowed(0)));
        assert_eq!(allowed.parts, ["Remove"]);
        let actions = block.last().unwrap();
        assert_eq!(actions.parts, ["Replace value", "Forget"]);
        // Forget asks in place.
        ui.confirm = true;
        let l = lines(&v, &ui, 5000);
        let actions = l
            .iter()
            .find(|x| x.field == Field::Vault(Row::Actions))
            .unwrap();
        assert_eq!(actions.value, "Forget TOKEN?");
        // Keep it stands where Forget stood, the second button.
        assert_eq!(actions.parts, ["Keep it", "Forget it"]);
        assert_eq!(keep_part(Kind::Secret), 0);
        // Open again closes.
        ui.open(item_at(&v, Row::Secret(0)).unwrap());
        assert!(ui.expanded.is_none() && !ui.confirm);
    }

    #[test]
    fn a_web_login_offers_its_password_and_seed() {
        let v = view();
        let mut ui = Ui::default();
        ui.open(item_at(&v, Row::Login(0)).unwrap());
        let l = lines(&v, &ui, 0);
        assert!(l
            .iter()
            .any(|x| x.value == "Signs in to app.example.com as me@example.com"));
        let actions = l
            .iter()
            .find(|x| x.field == Field::Vault(Row::Actions))
            .unwrap();
        assert_eq!(
            actions.parts,
            ["Replace password", "Replace seed", "Forget"]
        );
        // A login's Forget is its last button, at the right: Keep it
        // stands there.
        assert_eq!(confirm_parts(Kind::WebLogin), ["Forget it", "Keep it"]);
        assert_eq!(keep_part(Kind::WebLogin), 1);
    }

    #[test]
    fn an_add_form_shows_every_field_at_once_and_one_draft_at_a_time() {
        let v = view();
        let mut ui = Ui::default();
        let home = Home::of(&v);
        assert!(ui.start(Form::add_login(&home)));
        assert!(!ui.start(Form::add_secret(&home, "")));
        let l = lines(&v, &ui, 0);
        assert_eq!(
            rows(&l)[2..10],
            [
                Row::FormTitle,
                Row::Input(Slot::Sites),
                Row::Input(Slot::Name),
                Row::Input(Slot::User),
                Row::Input(Slot::Password),
                Row::Input(Slot::Seed),
                Row::Whose,
                Row::FormButtons,
            ]
        );
        assert_eq!(
            l[6].control,
            Control::Input {
                secret: true,
                shown: false
            }
        );
        assert_eq!(l[8].parts, ["This project", "Acme", "Everywhere"]);
        assert_eq!(l[8].control, Control::Segments(0));
        // Nothing typed, nothing said wrong until Save is pressed.
        assert!(!l.iter().any(|x| x.control == Control::Error));
        ui.form.as_mut().unwrap().tried = true;
        let l = lines(&v, &ui, 0);
        assert!(rows(&l).contains(&Row::Wrong(Slot::Sites)));
        assert!(rows(&l).contains(&Row::Wrong(Slot::Password)));
        assert!(!rows(&l).contains(&Row::Wrong(Slot::Seed)));
    }

    #[test]
    fn a_replace_form_stands_under_its_item_even_folded_or_filtered() {
        let v = view();
        let mut ui = Ui::default();
        let id = item_at(&v, Row::Secret(2)).unwrap();
        ui.start(Form::replace(&Home::of(&v), id, Slot::Value));
        ui.fold(Group::Identity);
        ui.filter.insert("zed");
        let l = lines(&v, &ui, 0);
        let r = rows(&l);
        let at = r.iter().position(|x| *x == Row::Secret(2)).unwrap();
        assert_eq!(r[at + 1..at + 3], [Row::FormTitle, Row::Input(Slot::Value)]);
        assert!(l[at + 1].nested);
        assert_eq!(l[at].control, Control::Item(true));
        // In a project that has no such item it stands at the top.
        let mut other = view();
        other.secrets.remove(2);
        let l = lines(&other, &ui, 0);
        let r = rows(&l);
        assert_eq!(r[2..4], [Row::FormTitle, Row::Input(Slot::Value)]);
        // Its title says whose the item is, seen from elsewhere.
        assert!(l[2].value.starts_with("A new value for GH, kept for Acme"));
    }

    fn typed(form: &mut Form, slot: Slot, text: &str) {
        let t = form.field_mut(slot).unwrap();
        t.select_all();
        t.insert(text);
        form.edited(slot);
    }

    #[test]
    fn a_secret_is_saved_once_its_name_and_value_will_do() {
        let home = Home::of(&view());
        let mut f = Form::add_secret(&home, "");
        typed(&mut f, Slot::Name, "github token");
        let wrong = shown_problems(&f);
        assert_eq!(wrong.len(), 1);
        assert_eq!(wrong[0].0, Slot::Name);
        assert!(decide(&f).is_err());
        typed(&mut f, Slot::Name, "GITHUB_TOKEN");
        typed(&mut f, Slot::Value, "  ghp_abc  ");
        f.whose = Whose::Identity;
        assert_eq!(
            decide(&f).unwrap(),
            Save::Secret {
                name: "GITHUB_TOKEN".into(),
                owner: Owner::Identity("acme".into()),
                value: "ghp_abc".into(),
            }
        );
        // A Debug of what is saved names it, never says its value.
        assert!(!format!("{:?}", decide(&f).unwrap()).contains("ghp"));
        assert!(!format!("{:?}", f.field(Slot::Value).unwrap()).contains("ghp"));
    }

    #[test]
    fn a_draft_saves_for_the_project_it_was_begun_in() {
        let home = Home::of(&view());
        assert_eq!(home.project, "app");
        let mut f = Form::add_secret(&home, "TOKEN");
        typed(&mut f, Slot::Value, "v");
        assert!(f.title().starts_with("A new secret for app"));
        // Whatever project is picked when Save is pressed, the draft knows
        // its own.
        let Save::Secret { owner, .. } = &decide(&f).unwrap() else {
            panic!("not a secret");
        };
        assert_eq!(owner, &Owner::Project(KEY.into()));
        // Begun where there is no identity, it cannot be the identity's.
        let mut f = Form::add_secret(&Home::of(&View::default()), "TOKEN");
        typed(&mut f, Slot::Value, "v");
        f.whose = Whose::Identity;
        assert_eq!(decide(&f).unwrap_err()[0].0, Slot::Whose);
    }

    #[test]
    fn a_web_login_is_named_by_its_site_until_named_by_hand() {
        let mut f = Form::add_login(&Home::of(&view()));
        typed(&mut f, Slot::Sites, "app.example.com/login");
        assert_eq!(f.text(Slot::Name), "EXAMPLE");
        typed(&mut f, Slot::Name, "MINE");
        typed(&mut f, Slot::Sites, "https://other.org");
        assert_eq!(f.text(Slot::Name), "MINE");
        typed(&mut f, Slot::Password, " pw ");
        typed(&mut f, Slot::Seed, "JBSWY3DP");
        assert_eq!(shown_problems(&f)[0].0, Slot::Seed);
        typed(&mut f, Slot::Seed, "JBSWY3DPEHPK3PXP");
        let Save::Login {
            origins,
            password,
            seed,
            owner,
            ..
        } = &decide(&f).unwrap()
        else {
            panic!("not a login");
        };
        assert_eq!(origins, &["https://other.org"]);
        // A password is kept as typed, spaces and all, as it always was.
        assert_eq!(password, " pw ");
        assert_eq!(seed.as_deref(), Some("JBSWY3DPEHPK3PXP"));
        assert_eq!(owner, &Owner::Project(KEY.into()));
    }

    #[test]
    fn a_replaced_seed_can_be_taken_away_but_not_left_empty() {
        let v = view();
        let id = item_at(&v, Row::Login(0)).unwrap();
        let mut f = Form::replace(&Home::of(&v), id.clone(), Slot::Seed);
        assert!(decide(&f).is_err());
        typed(&mut f, Slot::Seed, "None");
        assert_eq!(
            decide(&f).unwrap(),
            Save::Seed {
                item: id,
                seed: None
            }
        );
        // Every change is counted, for the window's layout of a clear
        // value.
        assert!(f.rev > 0);
    }

    #[test]
    fn a_secret_field_is_drawn_as_one_dot_a_utf16_unit() {
        let mut t = Text::new("", false, 100);
        t.insert("a\u{1F511}b");
        assert_eq!(display(&t, true), "\u{2022}".repeat(4));
        assert_eq!(display(&t, false), "a\u{1F511}b");
    }

    #[test]
    fn a_revealed_item_is_unfiltered_unfolded_and_opened() {
        let v = view();
        let mut ui = Ui::default();
        ui.fold(Group::Logins);
        ui.filter.insert("x");
        let id = item_at(&v, Row::Login(0)).unwrap();
        ui.reveal(id.clone());
        assert!(ui.filter.text.is_empty() && ui.folded.is_empty());
        assert_eq!(ui.expanded, Some(id));
        assert_eq!(items(&v, 0).len(), 6);
    }
}
