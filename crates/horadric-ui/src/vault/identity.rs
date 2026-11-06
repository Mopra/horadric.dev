//! Identities on this machine: where they are kept, which one a project
//! gets, the environment its sessions start with, and logging a tool in.
//! What it all means is `horadric_core::vault::identity`.

use std::ffi::{c_void, OsString};
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use horadric_core::vault::identity as core;
pub use horadric_core::vault::identity::{Git, Identity, Resolved, Tool};

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

/// Every identity this install has, none when the file is missing. One
/// that cannot be read or parsed is an error, so no change saves an empty
/// list over the identities in it.
pub fn load() -> Result<Vec<Identity>, String> {
    let path = file().ok_or("no state folder")?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{} cannot be read: {e}", path.display())),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(text).map_err(|e| {
        format!(
            "{} is not valid ({e}), so it is left as it is until it is mended",
            path.display()
        )
    })
}

/// [`load`] for a reader, none when the file is broken: a session then
/// starts with the machine's logins, as it would with no identities.
pub fn all() -> Vec<Identity> {
    load().unwrap_or_default()
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
    let mut list = load()?;
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

/// Forgets the identity, its folder of logins and every project's pick of
/// it. Tokens a tool keeps in Windows Credential Manager (`gh`'s) stay
/// there, as a logout would not have run. The folder goes first and the
/// entry last: a folder a locked file kept, or picks left behind, would
/// otherwise be inherited by the next identity made with the same slug.
pub fn delete(slug: &str) -> Result<(), String> {
    // A slug that is not one could name the folder of every identity.
    if slug.is_empty() || core::slug(slug) != slug {
        return Err(format!("no identity {slug}"));
    }
    load()?;
    let d = dir(slug);
    if d.is_dir() {
        fs::remove_dir_all(&d).map_err(|e| format!("{} could not be removed: {e}", d.display()))?;
    }
    let store = horadric_hooks::state_dir().map(|d| d.join("local.json"));
    for key in store.map_or_else(Vec::new, |f| core::picks_of(&read_text(&f), slug)) {
        set_pick(&key, None)?;
    }
    change(|list| list.retain(|i| i.slug != slug))
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

/// The environment a session in `project_dir` gets from its identity.
/// Every identity variable the app has goes, and the user's own value of
/// each comes back unless the identity sets its own, so one left over from
/// a session Horadric was started from never applies. Fixed at the
/// session's start.
pub fn session_env(project_dir: &Path) -> SessionEnv {
    let mut remove: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| core::identity_var(k))
        .collect();
    remove.push(horadric_hooks::SECRET_KEY_ENV.into());
    let id = identity_of(project_dir);
    let mut set = Vec::new();
    if let Some(id) = &id {
        let (s, r) = core::env(id, &dir(&id.slug));
        set = s;
        remove.extend(r);
    }
    let sets = |k: &str| set.iter().any(|(s, _)| s.eq_ignore_ascii_case(k));
    // The user's git entries past the identity's count would not be read,
    // and those under it would be overwritten: all or none.
    let own_git = !sets("GIT_CONFIG_COUNT");
    let own: Vec<(String, String)> = own_env()
        .iter()
        .filter(|(k, _)| !(is_token(k) && id.is_some()) && !sets(k))
        .filter(|(k, _)| own_git || !k.to_ascii_uppercase().starts_with("GIT_CONFIG_"))
        .cloned()
        .collect();
    set.extend(own);
    SessionEnv { set, remove }
}

fn is_token(name: &str) -> bool {
    core::MACHINE_TOKENS
        .iter()
        .any(|t| t.eq_ignore_ascii_case(name))
}

/// The user's own value of every identity variable and machine token,
/// read once from [`user_env`]. Without it, this process's own values,
/// unless it runs under a session, whose are not the user's.
fn own_env() -> &'static [(String, String)] {
    static OWN: OnceLock<Vec<(String, String)>> = OnceLock::new();
    OWN.get_or_init(|| {
        let base = user_env().unwrap_or_else(|| match console::under_claude() {
            true => Vec::new(),
            false => std::env::vars_os().collect(),
        });
        base.into_iter()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .filter(|(k, _)| core::identity_var(k) || is_token(k))
            .collect()
    })
}

fn own_var(name: &str) -> Option<PathBuf> {
    own_env()
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| PathBuf::from(v))
}

/// The user's own environment, as Windows makes it for a program they
/// start: the system's variables and theirs, with nothing a session
/// Horadric was started from added or took away. None when Windows will
/// not say.
pub fn user_env() -> Option<Vec<(OsString, OsString)>> {
    // SAFETY: the token is closed once the block is made, and the block is
    // read only up to its double nul, then destroyed.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            &mut token,
        )
        .ok()?;
        let mut block: *mut c_void = std::ptr::null_mut();
        let made = CreateEnvironmentBlock(&mut block, Some(token), false);
        let _ = CloseHandle(token);
        made.ok()?;
        let mut vars = Vec::new();
        let mut at = block as *const u16;
        loop {
            let mut len = 0;
            while *at.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            let entry = std::slice::from_raw_parts(at, len);
            // Past the first character: `=C:=C:\x` names a drive's folder.
            if let Some(eq) = entry[1..].iter().position(|&c| c == u16::from(b'=')) {
                vars.push((
                    OsString::from_wide(&entry[..eq + 1]),
                    OsString::from_wide(&entry[eq + 2..]),
                ));
            }
            at = at.add(len + 1);
        }
        let _ = DestroyEnvironmentBlock(block);
        Some(vars)
    }
}

/// For an app started from inside a session (a dev instance, an agent's
/// first start): puts the user's own identity variables and machine
/// tokens back in this process and takes the session's key out, so that
/// session's identity does not become every project's. Called before any
/// thread reads the environment.
pub fn reset_inherited() {
    let own = own_env();
    let stale: Vec<OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            k.to_str().is_some_and(|k| {
                core::identity_var(k)
                    || is_token(k)
                    || k.eq_ignore_ascii_case(horadric_hooks::SECRET_KEY_ENV)
            })
        })
        .collect();
    for k in stale {
        std::env::remove_var(k);
    }
    for (k, v) in own {
        std::env::set_var(k, v);
    }
}

/// Gives `command`, the app `reload` starts, the user's own environment
/// rather than this process's, which may have come from a session: its
/// identity, without the machine tokens it took away. Horadric's own
/// settings (`HORADRIC_DEV` and the like) go along; a session's tags and
/// key do not.
pub fn user_env_for(command: &mut Command) {
    let session = [
        horadric_hooks::SESSION_ENV,
        horadric_hooks::OWNER_ENV,
        horadric_hooks::TASKS_ENV,
        horadric_hooks::SECRET_KEY_ENV,
        horadric_core::worktree::SETUP_ENV,
        horadric_core::worktree::PORT_FIRST_ENV,
        horadric_core::worktree::PORT_LAST_ENV,
    ];
    let knob = |k: &str| {
        k.to_ascii_uppercase().starts_with("HORADRIC_")
            && !session.iter().any(|s| s.eq_ignore_ascii_case(k))
    };
    let carried: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| k.to_str().is_some_and(knob))
        .collect();
    match user_env() {
        Some(vars) => {
            command.env_clear().envs(vars);
        }
        None => {
            let inherited = console::inherited_env(console::under_claude());
            for (k, _) in std::env::vars_os() {
                let gone = k.to_str().is_some_and(|k| {
                    core::identity_var(k)
                        || session
                            .iter()
                            .chain(&inherited)
                            .any(|s| s.eq_ignore_ascii_case(k))
                });
                if gone {
                    command.env_remove(k);
                }
            }
        }
    }
    command.envs(carried);
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
        Tool::Vercel => vercel_auth(&vercel_dir(&d.join("xdg-data")))
            .then(|| read_text(&vercel_name_file(slug)).trim().to_string()),
    }
}

/// Where the Vercel CLI with `XDG_DATA_HOME` at `data` keeps its login:
/// its `xdg-app-paths` honours the variable on Windows too, and then adds
/// no `Data` folder. [`log_in`] makes the folder, as the CLI would
/// otherwise fall back to a `~/.now` it finds, the machine's.
fn vercel_dir(data: &Path) -> PathBuf {
    data.join("com.vercel.cli")
}

fn npm_who(npmrc: &Path) -> Option<String> {
    let registries = core::npm_registries(&read_text(npmrc));
    (!registries.is_empty()).then(|| registries.join(", "))
}

/// Whether a Vercel CLI config folder holds a login.
fn vercel_auth(config: &Path) -> bool {
    core::vercel_logged_in(&read_text(&config.join("auth.json")))
}

/// The machine's Vercel config folder, found as the CLI's
/// `getGlobalPathConfig` finds it: the first of its data folder, the old
/// `~/.now` and the older `now` data folder that is there, else the first.
/// Without `XDG_DATA_HOME` the data folder on Windows is
/// `%APPDATA%\com.vercel.cli\Data`.
fn vercel_global_dir() -> Option<PathBuf> {
    let (current, legacy) = match own_var("XDG_DATA_HOME") {
        Some(x) => (x.join("com.vercel.cli"), x.join("now")),
        None => {
            let app = std::env::var_os("APPDATA").map(PathBuf::from)?;
            (
                app.join("com.vercel.cli").join("Data"),
                app.join("now").join("Data"),
            )
        }
    };
    let home = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".now"));
    let found = [Some(current.clone()), home, Some(legacy)]
        .into_iter()
        .flatten()
        .find(|d| d.is_dir());
    Some(found.unwrap_or(current))
}

fn vercel_name_file(slug: &str) -> PathBuf {
    dir(slug).join("vercel-user.txt")
}

/// Runs `vercel whoami` as identity `slug` and keeps the name for
/// [`who`]. Only on the human's click, and off the UI thread: it is a
/// network call.
pub fn vercel_whoami(slug: &str) -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let program =
        horadric_pty::find_program("vercel", &path, horadric_pty::PROGRAM_EXTS, Path::is_file)?;
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

/// The machine's own login for `tool`, where each keeps it by default or
/// where the user's own variables say, never a session's leftovers.
fn global_login(tool: Tool) -> bool {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match tool {
        Tool::Gh => own_var("GH_CONFIG_DIR")
            .or_else(|| var("APPDATA").map(|a| a.join("GitHub CLI")))
            .is_some_and(|d| core::gh_user(&read_text(&d.join("hosts.yml"))).is_some()),
        Tool::Npm => own_var("NPM_CONFIG_USERCONFIG")
            .or_else(|| var("USERPROFILE").map(|h| h.join(".npmrc")))
            .is_some_and(|f| npm_who(&f).is_some()),
        Tool::Vercel => vercel_global_dir().is_some_and(|d| vercel_auth(&d)),
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
    for sub in [d.join("gh"), d.join("npm"), vercel_dir(&d.join("xdg-data"))] {
        fs::create_dir_all(sub).map_err(|e| e.to_string())?;
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
