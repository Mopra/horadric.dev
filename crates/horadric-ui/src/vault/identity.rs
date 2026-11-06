//! Identities on this machine: where they are kept, which one a project
//! gets, the environment its sessions start with, and logging a tool in.
//! What it all means is `horadric_core::vault::identity`.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use horadric_core::vault::identity::{Git, Identity, Resolved, Tool};
use horadric_core::vault::identity as core;

use crate::console;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn file() -> Option<PathBuf> {
    Some(horadric_hooks::state_dir()?.join("identities.json"))
}

fn read_text(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

/// Every identity this install has, none when the file is missing or
/// unreadable.
pub fn all() -> Vec<Identity> {
    file()
        .and_then(|f| serde_json::from_str(&read_text(&f)).ok())
        .unwrap_or_default()
}

/// One identity by its slug.
pub fn get(slug: &str) -> Option<Identity> {
    all().into_iter().find(|i| i.slug == slug)
}

/// Writes the whole list, through a file beside it so a reader never sees
/// half of it.
pub fn save(all: &[Identity]) -> Result<(), String> {
    let path = file().ok_or("no state folder")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut text = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    text.push('\n');
    let tmp = path.with_extension("horadric-tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Reads the list, lets `f` change it and saves it.
pub fn change<T>(f: impl FnOnce(&mut Vec<Identity>) -> T) -> Result<T, String> {
    let mut list = all();
    let out = f(&mut list);
    save(&list)?;
    Ok(out)
}

/// Changes the identity `slug`, an error when there is none.
fn change_one(slug: &str, f: impl FnOnce(&mut Identity)) -> Result<(), String> {
    change(|list| list.iter_mut().find(|i| i.slug == slug).map(f))?
        .ok_or_else(|| format!("no identity {slug}"))
}

/// Makes an identity called `name` with a slug of its own.
pub fn create(name: &str) -> Result<Identity, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("an identity needs a name".into());
    }
    change(|list| {
        let id = Identity {
            name: name.into(),
            slug: core::new_slug(name, list),
            ..Default::default()
        };
        list.push(id.clone());
        id
    })
}

/// Makes an identity with the slug a project's config names, for the
/// Projects row's offer when this install lacks it.
pub fn create_with_slug(name: &str, slug: &str) -> Result<Identity, String> {
    let slug = core::slug(slug);
    let name = match name.trim() {
        "" => slug.clone(),
        n => n.to_string(),
    };
    change(|list| {
        if list.iter().any(|i| i.slug == slug) {
            return Err(format!("there is already an identity {slug}"));
        }
        let id = Identity {
            name,
            slug,
            ..Default::default()
        };
        list.push(id.clone());
        Ok(id)
    })?
}

/// Renames it. The slug stays: its folder and config files name it.
pub fn rename(slug: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("an identity needs a name".into());
    }
    change_one(slug, |i| i.name = name.into())
}

/// Forgets the identity and its folder of logins. Tokens a tool keeps in
/// Windows Credential Manager (`gh`'s) stay there, as a logout would not
/// have run.
pub fn delete(slug: &str) -> Result<(), String> {
    change(|list| list.retain(|i| i.slug != slug))?;
    let d = dir(slug);
    if d.is_dir() {
        fs::remove_dir_all(&d).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Sets git's name, email and GitHub user; None takes git off the
/// identity, so its sessions get the machine's.
pub fn set_git(slug: &str, git: Option<Git>) -> Result<(), String> {
    change_one(slug, |i| i.git = git)
}

/// Adds a folder whose projects are the identity's unless they pick.
pub fn add_claim(slug: &str, folder: &str) -> Result<(), String> {
    change_one(slug, |i| i.claims = core::with_claim(&i.claims, folder))
}

pub fn remove_claim(slug: &str, folder: &str) -> Result<(), String> {
    change_one(slug, |i| i.claims = core::without_claim(&i.claims, folder))
}

/// Stops pointing `tool` at the identity's folder, so its sessions use
/// the machine's login again. The folder is kept, for a change of mind.
pub fn remove_tool(slug: &str, tool: Tool) -> Result<(), String> {
    change_one(slug, |i| i.tools.retain(|t| *t != tool))
}

/// The identity's folder of the tools' config.
pub fn dir(slug: &str) -> PathBuf {
    horadric_hooks::state_dir()
        .unwrap_or_default()
        .join("identities")
        .join(slug)
}

/// The human's pick for the project `project_key` on this machine: a
/// slug, "" for a pick of none, None for no pick.
pub fn pick(project_key: &str) -> Option<String> {
    horadric_hooks::tasks::identity(Path::new(project_key))
}

/// Sets the pick: Some(slug), Some("") for none whatever the config file
/// and claims say, None to let them decide.
pub fn set_pick(project_key: &str, slug: Option<&str>) -> Result<(), String> {
    horadric_hooks::tasks::set_identity(Path::new(project_key), slug).map_err(|e| e.to_string())
}

/// Which identity the project in `project_dir` gets, a worktree's being
/// its main tree's.
pub fn of_project(project_dir: &Path) -> Resolved {
    let key = crate::project::folder_key(&project_dir.to_string_lossy());
    if key.is_empty() {
        return Resolved::None;
    }
    // The config may be kept out of git, so a worktree reads its
    // project's from the main tree.
    let config = [Path::new(&key), project_dir]
        .into_iter()
        .map(|p| read_text(&horadric_hooks::tasks::config_file(p)))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default();
    core::resolve(
        pick(&key).as_deref(),
        core::config_identity(&config).as_deref(),
        &key,
        &all(),
    )
}

/// The project's identity itself, None for none or an unknown slug.
pub fn identity_of(project_dir: &Path) -> Option<Identity> {
    let slug = of_project(project_dir).slug()?.to_string();
    get(&slug)
}

/// What a session starts with besides the app's own: variables to set and
/// to remove from what it inherits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionEnv {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

/// The environment a session in `project_dir` gets from its identity,
/// empty with none. Fixed at the session's start.
pub fn session_env(project_dir: &Path) -> SessionEnv {
    let Some(id) = identity_of(project_dir) else {
        return SessionEnv::default();
    };
    let (set, remove) = core::env(&id, &dir(&id.slug));
    SessionEnv { set, remove }
}

/// The lines a Claude session in `project_dir` gets in its
/// `--append-system-prompt`, none without an identity.
pub fn prompt_lines(project_dir: &Path) -> Vec<String> {
    let Some(id) = identity_of(project_dir) else {
        return Vec::new();
    };
    let who: Vec<(Tool, Option<String>)> = Tool::ALL
        .into_iter()
        .filter(|t| id.tools.contains(t))
        .map(|t| (t, who(&id.slug, t)))
        .collect();
    core::prompt_lines(&id, &who)
}

/// Who identity `slug` is logged in to `tool` as, from the tool's files
/// and never by running it: `gh`'s active user, npm's registries with a
/// login, and for Vercel the name [`vercel_whoami`] last found, "" when
/// it never ran. None when not logged in.
pub fn who(slug: &str, tool: Tool) -> Option<String> {
    let d = dir(slug);
    match tool {
        Tool::Gh => core::gh_user(&read_text(&d.join("gh").join("hosts.yml"))),
        Tool::Npm => npm_who(&d.join("npm").join(".npmrc")),
        Tool::Vercel => vercel_auth(&d.join("xdg-data"))
            .then(|| read_text(&vercel_name_file(slug)).trim().to_string()),
    }
}

fn npm_who(npmrc: &Path) -> Option<String> {
    let registries = core::npm_registries(&read_text(npmrc));
    (!registries.is_empty()).then(|| registries.join(", "))
}

/// Whether a Vercel data folder holds a login. Where in it differs by
/// version on Windows, so both places are looked in.
fn vercel_auth(data: &Path) -> bool {
    let at = data.join("com.vercel.cli");
    [at.join("auth.json"), at.join("Data").join("auth.json")]
        .iter()
        .any(|f| core::vercel_logged_in(&read_text(f)))
}

fn vercel_name_file(slug: &str) -> PathBuf {
    dir(slug).join("vercel-user.txt")
}

/// Runs `vercel whoami` as identity `slug` and keeps the name for
/// [`who`]. Only on the human's click, and off the UI thread: it is a
/// network call.
pub fn vercel_whoami(slug: &str) -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let program = horadric_pty::find_program(
        "vercel",
        &path,
        horadric_pty::PROGRAM_EXTS,
        Path::is_file,
    )?;
    let mut command = Command::new(program);
    command.arg("whoami").creation_flags(CREATE_NO_WINDOW);
    tool_command(&mut command, slug, Tool::Vercel);
    let out = command.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())?
        .to_string();
    fs::write(vercel_name_file(slug), &name).ok();
    Some(name)
}

/// Whether identity `slug` has a login for the tool `--on-login` named.
/// An unknown tool name has none.
pub fn has_login(slug: &str, tool_name: &str) -> bool {
    Tool::from_name(tool_name).is_some_and(|t| who(slug, t).is_some())
}

/// Whether a session in `project_dir` would find `tool_name` logged in:
/// its identity's login, or with no identity (or one without that tool)
/// the machine's. What a quest's `--on-login` wait checks.
pub fn project_has_login(project_dir: &Path, tool_name: &str) -> bool {
    let Some(tool) = Tool::from_name(tool_name) else {
        return false;
    };
    match identity_of(project_dir) {
        Some(id) if id.tools.contains(&tool) => who(&id.slug, tool).is_some(),
        _ => global_login(tool),
    }
}

/// The machine's own login for `tool`, where each keeps it by default.
fn global_login(tool: Tool) -> bool {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match tool {
        Tool::Gh => var("GH_CONFIG_DIR")
            .or_else(|| var("APPDATA").map(|a| a.join("GitHub CLI")))
            .is_some_and(|d| core::gh_user(&read_text(&d.join("hosts.yml"))).is_some()),
        Tool::Npm => var("NPM_CONFIG_USERCONFIG")
            .or_else(|| var("USERPROFILE").map(|h| h.join(".npmrc")))
            .is_some_and(|f| npm_who(&f).is_some()),
        Tool::Vercel => var("XDG_DATA_HOME")
            .or_else(|| var("APPDATA"))
            .is_some_and(|d| vercel_auth(&d)),
    }
}

/// The GitHub accounts Git Credential Manager keeps, for the git row's
/// pick. Runs GCM, so only on the human's click.
pub fn github_accounts() -> Vec<String> {
    let out = Command::new("git")
        .args(["credential-manager", "github", "list"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Points `command` at identity `slug`'s folder for `tool`, without the
/// machine's tokens that would beat it or the tags of the session the app
/// may have been started from.
fn tool_command(command: &mut Command, slug: &str, tool: Tool) {
    for (k, v) in core::tool_env(tool, &dir(slug)) {
        command.env(k, v);
    }
    let tags = [horadric_hooks::SESSION_ENV, horadric_hooks::OWNER_ENV];
    for name in console::inherited_env(console::under_claude())
        .iter()
        .chain(&tags)
        .chain(&core::MACHINE_TOKENS)
    {
        command.env_remove(name);
    }
}

/// Opens `tool`'s own login for identity `slug` in a console of its own,
/// in the identity's folder, and from then on points the identity's
/// sessions at it. The app sees the login in the files.
pub fn log_in(slug: &str, tool: Tool) -> Result<(), String> {
    let id = get(slug).ok_or_else(|| format!("no identity {slug}"))?;
    let d = dir(slug);
    for sub in ["gh", "npm", "xdg-data"] {
        fs::create_dir_all(d.join(sub)).map_err(|e| e.to_string())?;
    }
    if !id.tools.contains(&tool) {
        change_one(slug, |i| i.tools.push(tool))?;
    }
    let title = format!("Log in to {} for {}", tool.name(), id.name).replace('"', "");
    // `start` gives it a console with working handles, which a program
    // without one cannot hand down, and `cmd /k` keeps it open to read
    // what the login said. `cmd` finds `npm.cmd` and `vercel.cmd` too.
    let mut command = Command::new("cmd.exe");
    command
        .raw_arg(format!(
            "/c start \"{title}\" cmd /k {} {}",
            tool.name(),
            tool.login_args().join(" ")
        ))
        .current_dir(&d)
        .creation_flags(CREATE_NO_WINDOW);
    tool_command(&mut command, slug, tool);
    command.spawn().map(drop).map_err(|e| e.to_string())
}
