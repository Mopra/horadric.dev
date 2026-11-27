//! `takeover_open`: starts a program on the human's own desktop, so an
//! agent asked to look through Slack is not stuck when Slack is not open.
//!
//! Starting a program is operating it: the app is asked first, through
//! the same approval question as a first click, naming the program, and
//! the lists of programs allowed ahead of time count as they do there.
//! What is already running is never started a second time: the answer
//! lists its windows instead, and nothing is brought forward, which would
//! be operating it unasked. A program is never started as administrator.
//!
//! The twin rule of the agent's desk does not apply: this is the human's
//! own session, where a second copy is the thing to avoid, not the first.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::desk_apps::{self, Kind, Way};
use horadric_core::takeover_programs::is_horadric;
use serde_json::{json, Value};
use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_ELEVATION_REQUIRED, ERROR_SUCCESS};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, IPersistFile,
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_NOEXPAND,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY,
};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    ApplicationActivationManager, BHID_EnumItems, FOLDERID_AppsFolder,
    IApplicationActivationManager, IEnumShellItems, IShellItem, IShellLinkW, SHGetKnownFolderItem,
    ShellLink, AO_NONE, KF_FLAG_DEFAULT, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

use super::{
    allow_body, allowed, list_windows, reach_of, report, short, window_json, windows_text,
    Takeover, Window,
};
use crate::mcp::text;
use crate::takeover_watch as watch;

/// How long a started program has to show its window. Electron apps and
/// Store apps take seconds; past this the agent is told to wait for it.
const OPEN_WAIT: Duration = Duration::from_secs(20);
const OPEN_POLL: Duration = Duration::from_millis(250);

const OWN: &str = "Horadric's own programs are not started by takeover: a session operating \
                   Horadric could answer its own questions";

/// What the call asked to start, read and checked before anything is
/// looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// A name like slack, outlook or notepad, lower case, without `.exe`.
    Name(String),
    /// A program's file (.exe), a shortcut to one (.lnk), or a packaged
    /// app's `shell:AppsFolder\<id>`.
    Path(String),
}

/// The id in `shell:AppsFolder\<id>`, in any case.
pub fn folder_id(path: &str) -> Option<&str> {
    let p = path.trim();
    let head = "shell:appsfolder";
    let rest = p
        .get(..head.len())
        .filter(|h| h.eq_ignore_ascii_case(head))
        .map(|_| &p[head.len()..])?;
    let id = rest.strip_prefix(['\\', '/'])?.trim();
    (!id.is_empty()).then_some(id)
}

/// Whether an AppUserModelID is a packaged app's (family!app), which is
/// started by activation. Any other id in the Apps folder is a shortcut
/// whose program is better named by its path.
pub fn packaged(id: &str) -> bool {
    id.split_once('!')
        .is_some_and(|(family, app)| !family.trim().is_empty() && !app.trim().is_empty())
}

fn own(what: &str) -> bool {
    desk_apps::own_refusal(what).is_some() || is_horadric(&desk_apps::clean(what))
}

/// Reads `app` and `path` of a call.
pub fn asked(app: Option<&str>, path: Option<&str>) -> Result<Asked, String> {
    let app = app.map(str::trim).filter(|a| !a.is_empty());
    let path = path.map(str::trim).filter(|p| !p.is_empty());
    let asked = match (app, path) {
        (Some(_), Some(_)) => return Err("give app or path, not both".into()),
        (None, None) => {
            return Err("say what to start: app (a name like slack) or path".into());
        }
        (Some(a), None) => {
            // Not `process_name`, which would read "cmd /c x" as "c x".
            let name = a.to_lowercase();
            let name = name
                .strip_suffix(".exe")
                .unwrap_or(&name)
                .trim()
                .to_string();
            let simple = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ' ');
            if name.is_empty() || !name.chars().all(simple) {
                return Err(format!(
                    "{a:?} is not an app's name: give a name like slack, or a path"
                ));
            }
            Asked::Name(name)
        }
        (None, Some(p)) => {
            let p = desk_apps::clean(p);
            match desk_apps::kind(&p) {
                Kind::Program | Kind::Shortcut => {}
                Kind::Shell if folder_id(&p).is_some() => {}
                _ => {
                    return Err(format!(
                        "{p} is not a program: give a program's path (.exe), a shortcut to one \
                         (.lnk) or shell:AppsFolder\\<id>. To open a document, start its program \
                         with the document in args"
                    ))
                }
            }
            Asked::Path(p)
        }
    };
    let (Asked::Name(n) | Asked::Path(n)) = &asked;
    if own(n) {
        return Err(OWN.into());
    }
    Ok(asked)
}

/// The process names, lower case and without `.exe`, that mean what was
/// asked for runs already.
pub fn running_names(asked: &Asked) -> Vec<String> {
    let mut names = Vec::new();
    let app = match asked {
        Asked::Name(n) => {
            names.push(n.clone());
            desk_apps::find(n)
        }
        Asked::Path(p) => {
            if desk_apps::kind(p) == Kind::Program {
                names.push(desk_apps::process_name(p));
            }
            desk_apps::app_of(p)
        }
    };
    if let Some(app) = app {
        names.extend(app.processes.iter().map(|p| p.to_string()));
    }
    names.sort();
    names.dedup();
    names
}

fn stem(exe: &str) -> String {
    desk_apps::process_name(exe)
}

/// The windows of `names` among `list`, frontmost first.
pub fn running(list: &[Window], names: &[String]) -> Vec<Window> {
    list.iter()
        .filter(|w| names.contains(&stem(&w.exe)))
        .cloned()
        .collect()
}

/// The window a start brought: one not open `before`, of the process
/// started (`pid`), else of a program of `names`, since a program that
/// runs once per user hands its start to the copy running and exits.
/// A main window before a dialog, the frontmost first.
pub fn opened(
    before: &[isize],
    now: &[Window],
    pid: Option<u32>,
    names: &[String],
) -> Option<Window> {
    let new: Vec<&Window> = now.iter().filter(|w| !before.contains(&w.hwnd)).collect();
    let started = |w: &&&Window| Some(w.pid) == pid;
    let named = |w: &&&Window| names.contains(&stem(&w.exe));
    let main = |w: &&&Window| w.owner.is_none();
    new.iter()
        .filter(main)
        .find(started)
        .or_else(|| new.iter().filter(main).find(named))
        .or_else(|| new.iter().find(|w| started(w) || named(w)))
        .map(|w| (*w).clone())
}

/// Programs that run whatever their arguments say: one started with
/// arguments is asked about as a risky action, since its arguments are
/// a command.
const RUNNERS: &[&str] = &[
    "cmd",
    "powershell",
    "pwsh",
    "wt",
    "conhost",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "msiexec",
    "reg",
    "regedit",
    "schtasks",
    "sc",
    "wsl",
    "bash",
    "python",
    "pythonw",
    "py",
    "node",
    "certutil",
    "bitsadmin",
    "msbuild",
    "installutil",
    "explorer",
    "forfiles",
];

/// Whether starting `exe` with `args` is asked about as risky.
pub fn start_risk(exe: &str, args: &[String]) -> bool {
    !args.is_empty() && RUNNERS.contains(&stem(exe).as_str())
}

/// The arguments of a call: a list of strings, or one command line tail.
pub fn args_of(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(desk_apps::split_arguments(s)),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "args is a list of strings".to_string()),
        Some(_) => Err("args is a list of strings".into()),
    }
}

/// How a program is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Its file, by CreateProcessW, which never elevates.
    Exe(String),
    /// A packaged app by its AppUserModelID, through activation, which
    /// never elevates either.
    Packaged(String),
}

/// What will be started, once found.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Its name as the human reads it.
    pub label: String,
    pub start: Start,
    /// The file name the approval and the lists name it by: the exe, or
    /// for a packaged app its process when known, else its id.
    pub exe: String,
    /// The exe's full path, empty for a packaged app.
    pub path: String,
    /// A packaged app's id, empty otherwise.
    pub app_id: String,
    pub args: Vec<String>,
    /// The process names that mean it runs.
    pub names: Vec<String>,
}

/// What the agent and the human read for a plan.
pub fn what_of(plan: &Plan) -> String {
    let mut what = format!("start {}", plan.label);
    match &plan.start {
        Start::Exe(path) => what.push_str(&format!(" ({path})")),
        Start::Packaged(id) => what.push_str(&format!(" (the app {id})")),
    }
    if !plan.args.is_empty() {
        what.push_str(&format!(
            " with {}",
            short(&desk_apps::arguments(&plan.args), 200)
        ));
    }
    what
}

/// The answer when it runs already: its windows, nothing done.
pub fn already_text(label: &str, list: &str) -> String {
    format!(
        "{label} is already open, so it was not started a second time and nothing was asked. \
         Act on one of its windows by id; the first look or action in it asks the user as usual.\n\
         {list}"
    )
}

/// The answer when a program will only run as administrator.
pub fn elevated_text(label: &str) -> String {
    format!(
        "{label} only runs as administrator, and takeover never starts a program elevated. \
         Ask the user to start it; once it is open, takeover reaches it through Horadric's \
         elevated helper."
    )
}

/// Runs `f` on a thread of its own in a single threaded apartment, which
/// the shell's objects want, apart from this thread's UI Automation.
fn in_sta<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    thread::spawn(move || {
        // SAFETY: paired with the CoUninitialize below on this thread.
        let joined =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let out = f();
        if joined.is_ok() {
            // SAFETY: pairs the CoInitializeEx that succeeded.
            unsafe { CoUninitialize() };
        }
        out
    })
    .join()
    .map_err(|_| "looking the program up failed".to_string())
}

/// Where the App Paths key says a program is, the user's own entry before
/// the machine's: where the Run box looks a bare name up.
fn app_path(file: &str) -> Option<String> {
    let key = HSTRING::from(format!(
        r"Software\Microsoft\Windows\CurrentVersion\App Paths\{file}"
    ));
    let plain = REG_ROUTINE_FLAGS(0);
    [
        (HKEY_CURRENT_USER, plain),
        (HKEY_LOCAL_MACHINE, plain),
        (HKEY_LOCAL_MACHINE, RRF_SUBKEY_WOW6432KEY),
    ]
    .into_iter()
    .find_map(|(hive, view)| {
        let mut buf = [0u16; 1024];
        let mut size = std::mem::size_of_val(&buf) as u32;
        // SAFETY: the key outlives the call, and the buffer goes with its
        // size in bytes.
        let read = unsafe {
            RegGetValueW(
                hive,
                &key,
                PCWSTR::null(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND | view,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if read != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        let written = String::from_utf16_lossy(&buf[..end]);
        let path = desk_apps::expand(written.trim().trim_matches('"'), &|name| {
            std::env::var(name).ok()
        });
        // Not `is_file`: a Store app's entry is an alias that starts but
        // can not be opened to look at.
        std::fs::symlink_metadata(&path).is_ok().then_some(path)
    })
}

/// A program on the PATH. Not SearchPathW, which looks in the current
/// folder first, the project's, where a planted exe of the name would run.
fn on_the_path(file: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(file))
        .find(|f| f.is_file())
        .map(|f| f.to_string_lossy().into_owned())
}

fn exe_named(file: &str) -> Option<String> {
    app_path(file).or_else(|| on_the_path(file))
}

/// The target and arguments of a shortcut.
fn shortcut(link: &Path) -> Option<(String, String)> {
    // SAFETY: the link's name outlives the calls, and each buffer is a
    // local array handed with its own length.
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = shell.cast().ok()?;
        file.Load(&HSTRING::from(link.as_os_str()), STGM_READ)
            .ok()?;
        let mut target = [0u16; 1024];
        shell.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        let mut arguments = [0u16; 2048];
        let _ = shell.GetArguments(&mut arguments);
        let text = |buf: &[u16]| {
            let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        };
        let target = text(&target);
        (!target.is_empty()).then(|| (target, text(&arguments)))
    }
}

/// The Start menu's folders, the user's before everyone's.
fn start_menus() -> Vec<PathBuf> {
    ["APPDATA", "ProgramData"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs"))
        .collect()
}

/// A Start menu shortcut whose name is `name`, in any case, a few folders
/// deep at most.
fn start_menu_shortcut(name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
        let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        let hit = entries.iter().map(|e| e.path()).find(|p| {
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk"))
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().trim().eq_ignore_ascii_case(name))
        });
        if hit.is_some() || depth == 0 {
            return hit;
        }
        entries
            .iter()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .find_map(|d| walk(&d, name, depth - 1))
    }
    start_menus().iter().find_map(|d| walk(d, name, 3))
}

fn display(item: &IShellItem, how: SIGDN) -> Option<String> {
    // SAFETY: the name comes back allocated for the caller, who frees it.
    unsafe {
        let p = item.GetDisplayName(how).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// The apps of the Start menu's Apps folder, each with its name and id.
fn apps_folder() -> Vec<(String, String)> {
    let mut out = Vec::new();
    // SAFETY: plain COM calls on this apartment's thread, each item freed
    // as it goes out of scope.
    unsafe {
        let Ok(folder) =
            SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
        else {
            return out;
        };
        let Ok(items) =
            folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems)
        else {
            return out;
        };
        // Bounded, in case an enumerator never says it is done.
        for _ in 0..5000 {
            let mut one = [None];
            let mut got = 0u32;
            if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(item) = one[0].take() else { break };
            if let (Some(name), Some(id)) = (
                display(&item, SIGDN_NORMALDISPLAY),
                display(&item, SIGDN_PARENTRELATIVEPARSING),
            ) {
                out.push((name, id));
            }
        }
    }
    out
}

fn plan_exe(label: String, path: String, args: Vec<String>, mut names: Vec<String>) -> Plan {
    names.push(stem(&path));
    names.sort();
    names.dedup();
    Plan {
        label,
        exe: path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string(),
        start: Start::Exe(path.clone()),
        path,
        app_id: String::new(),
        args,
        names,
    }
}

fn plan_packaged(label: String, id: &str, args: Vec<String>, names: Vec<String>) -> Plan {
    // A known app's process names it for the lists; any other is named by
    // its id, which no list entry matches, so it is asked.
    let exe = names
        .iter()
        .find(|n| desk_apps::find(n).is_some())
        .map_or_else(|| id.to_string(), |n| format!("{n}.exe"));
    Plan {
        label,
        start: Start::Packaged(id.to_string()),
        exe,
        path: String::new(),
        app_id: id.to_string(),
        args,
        names,
    }
}

/// Finds what to start for `asked`. Runs in [`in_sta`].
fn resolve(asked: &Asked, args: Vec<String>) -> Result<Plan, String> {
    let names = running_names(asked);
    match asked {
        Asked::Name(n) => by_name(n, args, names),
        Asked::Path(p) => {
            if let Some(id) = folder_id(p) {
                if packaged(id) {
                    let label = apps_folder()
                        .into_iter()
                        .find(|(_, i)| i.eq_ignore_ascii_case(id))
                        .map_or_else(|| id.to_string(), |(name, _)| name);
                    return Ok(plan_packaged(label, id, args, names));
                }
                return match desk_apps::app_of(p) {
                    Some(app) => by_name(app.names[0], args, names),
                    None => Err(format!(
                        "{id} is not a packaged app, so it is not started by its id: give the \
                         program's path"
                    )),
                };
            }
            if desk_apps::kind(p) == Kind::Shortcut {
                let (target, tail) = shortcut(Path::new(p))
                    .ok_or_else(|| format!("{p} could not be read for the program it starts"))?;
                let target = desk_apps::clean(&target);
                if desk_apps::kind(&target) != Kind::Program {
                    return Err(format!("{p} starts {target}, which is not a program"));
                }
                let mut all = desk_apps::split_arguments(&tail);
                all.extend(args);
                return from_file(&target, all, names);
            }
            from_file(p, args, names)
        }
    }
}

fn from_file(path: &str, args: Vec<String>, names: Vec<String>) -> Result<Plan, String> {
    if own(path) {
        return Err(OWN.into());
    }
    if !Path::new(path).is_file() {
        return Err(format!("{path} was not found"));
    }
    let label = desk_apps::app_of(path).map_or_else(|| stem(path), |a| a.label.to_string());
    Ok(plan_exe(label, path.to_string(), args, names))
}

/// A name, found the ways Windows itself finds one: the table of known
/// apps, the App Paths key, the PATH, the Start menu's shortcuts, then
/// its Apps folder for a Store app.
fn by_name(name: &str, args: Vec<String>, names: Vec<String>) -> Result<Plan, String> {
    let known = desk_apps::find(name);
    let label = known.map_or_else(|| name.to_string(), |a| a.label.to_string());
    let mut folder: Option<Vec<(String, String)>> = None;
    for way in known.map_or(&[][..], |a| a.ways) {
        match way {
            Way::Exe(file) => {
                if let Some(path) = exe_named(file) {
                    return from_file(&path, args, names).map(|p| Plan { label, ..p });
                }
            }
            Way::Packaged(id) => {
                let apps = folder.get_or_insert_with(apps_folder);
                if apps.iter().any(|(_, i)| i.eq_ignore_ascii_case(id)) {
                    return Ok(plan_packaged(label, id, args, names));
                }
            }
        }
    }
    if let Some(path) = exe_named(&format!("{name}.exe")) {
        return from_file(&path, args, names).map(|p| Plan { label, ..p });
    }
    if let Some(link) = start_menu_shortcut(name) {
        if let Some((target, tail)) = shortcut(&link) {
            let target = desk_apps::clean(&target);
            if desk_apps::kind(&target) == Kind::Program {
                let mut all = desk_apps::split_arguments(&tail);
                all.extend(args);
                return from_file(&target, all, names).map(|p| Plan { label, ..p });
            }
        }
    }
    let apps = folder.get_or_insert_with(apps_folder);
    let found = apps
        .iter()
        .find(|(n, id)| n.trim().eq_ignore_ascii_case(name) && packaged(id))
        .cloned();
    if let Some((display, id)) = found {
        return Ok(plan_packaged(display, &id, args, names));
    }
    Err(format!(
        "no program named {name:?} was found (not in the App Paths key, on the PATH, in the \
         Start menu or among the Store apps): give its path"
    ))
}

/// Starts a plan, never elevated. The process id, when Windows says it.
fn launch(plan: &Plan) -> Result<u32, String> {
    match &plan.start {
        Start::Exe(exe) => create(exe, &plan.args, &plan.label),
        Start::Packaged(id) => {
            // SAFETY: plain COM calls; the strings outlive them.
            unsafe {
                let manager: IApplicationActivationManager =
                    CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_ALL)
                        .map_err(|e| format!("{} did not start: {}", plan.label, e.message()))?;
                manager
                    .ActivateApplication(
                        &HSTRING::from(id.as_str()),
                        &HSTRING::from(desk_apps::arguments(&plan.args)),
                        AO_NONE,
                    )
                    .map_err(|e| format!("{} did not start: {}", plan.label, e.message()))
            }
        }
    }
}

fn create(exe: &str, args: &[String], label: &str) -> Result<u32, String> {
    let mut line: Vec<u16> = desk_apps::command_line(exe, args)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    if line.len() > 32_767 {
        return Err("the arguments are too long for one command line".into());
    }
    let program = HSTRING::from(exe);
    let folder = Path::new(exe)
        .parent()
        .filter(|f| !f.as_os_str().is_empty())
        .map(|f| HSTRING::from(f.as_os_str()));
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut made = PROCESS_INFORMATION::default();
    // SAFETY: the program, the folder and the line outlive the call, the
    // line is NUL ended and writable, and no handle is inherited.
    // CreateProcessW never raises the elevation prompt: a program whose
    // manifest asks for administrator fails with ERROR_ELEVATION_REQUIRED.
    unsafe {
        let started = CreateProcessW(
            &program,
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            folder
                .as_ref()
                .map_or(PCWSTR::null(), |f| PCWSTR(f.as_ptr())),
            &startup,
            &mut made,
        );
        if let Err(e) = started {
            if e.code() == ERROR_ELEVATION_REQUIRED.to_hresult() {
                return Err(elevated_text(label));
            }
            return Err(format!("{label} did not start: {}", e.message()));
        }
        let _ = CloseHandle(made.hThread);
        let _ = CloseHandle(made.hProcess);
    }
    Ok(made.dwProcessId)
}

impl Takeover {
    /// `takeover_open`.
    pub(super) fn open<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        args: &Value,
        reason: &str,
        app: &mut A,
    ) -> Result<Vec<Value>, String> {
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        let asked = asked(s("app"), s("path"))?;
        let extra = args_of(args.get("args"))?;
        // Looked at before anything is looked up or asked: a program the
        // human has open is theirs to show, not to start again.
        let shown = |me: &Takeover, label: &str, list: &[Window]| {
            let known = |w: &Window| me.granted.contains_key(&w.pid);
            already_text(label, &windows_text(list, reach_of, known))
        };
        let (Asked::Name(label) | Asked::Path(label)) = &asked;
        let now = running(&list_windows(), &running_names(&asked));
        if !now.is_empty() {
            return Ok(vec![text(shown(self, label, &now))]);
        }
        let plan = {
            let asked = asked.clone();
            in_sta(move || resolve(&asked, extra))??
        };
        let now = running(&list_windows(), &plan.names);
        if !now.is_empty() {
            return Ok(vec![text(shown(self, &plan.label, &now))]);
        }
        let asking = Window {
            hwnd: 0,
            title: String::new(),
            exe: plan.exe.clone(),
            path: plan.path.clone(),
            pid: 0,
            owner: None,
            class: String::new(),
        };
        let what = what_of(&plan);
        let risky = args.get("risky") == Some(&json!(true)) || start_risk(&plan.exe, &plan.args);
        let mut body = allow_body(&asking, "operate", &what, risky, None, reason);
        body["open"] = json!(true);
        body["app_id"] = json!(plan.app_id);
        allowed(app(body)?, &what)?;
        let before: Vec<isize> = list_windows().iter().map(|w| w.hwnd).collect();
        let started = {
            let plan = plan.clone();
            in_sta(move || launch(&plan)).and_then(|r| r)
        };
        let pid = match started {
            Ok(pid) => pid,
            Err(why) => return report(app, &asking, &what, Err(why)),
        };
        if pid != 0 {
            self.granted.insert(pid, true);
        }
        let until = Instant::now() + OPEN_WAIT;
        let found = loop {
            let now = list_windows();
            if let Some(w) = opened(&before, &now, (pid != 0).then_some(pid), &plan.names) {
                break Some(w);
            }
            if Instant::now() >= until {
                break None;
            }
            thread::sleep(OPEN_POLL);
        };
        let Some(w) = found else {
            let said = format!(
                "Started {} (process {pid}), but no window of it showed within {} s. It may \
                 still be starting or sit in the tray: takeover_wait with appears, or \
                 takeover_windows, finds it once it shows.",
                plan.label,
                OPEN_WAIT.as_secs()
            );
            return report(app, &asking, &what, Ok((vec![text(said.clone())], said)));
        };
        self.let_in(&w, true);
        watch::follow(&w);
        // The app keeps its grant by the program's path, and what shows
        // the window is often another file than the one started (a
        // launcher, a Store app's own exe), so it is told which window
        // came of the start it allowed.
        let carried = app(json!({ "op": "desktop.opened", "window": window_json(&w) }))
            .is_ok_and(|a| a["granted"] == true);
        let mut said = format!(
            "Started {}: window {} {:?} ({}, process {}).",
            plan.label, w.hwnd, w.title, w.exe, w.pid
        );
        if carried {
            said.push_str(
                " This session may operate it now without asking again; risky actions are \
                 still asked.",
            );
        } else {
            said.push_str(&format!(
                " It runs as {}, not as what was allowed, so the first action in it asks the \
                 user once more.",
                w.exe
            ));
        }
        report(app, &asking, &what, Ok((vec![text(said.clone())], said)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(hwnd: isize, exe: &str, pid: u32, owner: Option<isize>) -> Window {
        Window {
            hwnd,
            title: format!("{exe} {hwnd}"),
            exe: exe.into(),
            path: format!(r"C:\x\{exe}"),
            pid,
            owner,
            class: String::new(),
        }
    }

    #[test]
    fn a_call_asks_for_a_name_or_a_program_s_path() {
        assert_eq!(asked(Some("Slack"), None), Ok(Asked::Name("slack".into())));
        assert_eq!(
            asked(Some("notepad.exe"), None),
            Ok(Asked::Name("notepad".into()))
        );
        assert_eq!(
            asked(None, Some(r#""C:\Apps\Tool.exe""#)),
            Ok(Asked::Path(r"C:\Apps\Tool.exe".into()))
        );
        assert!(asked(None, Some(r"shell:AppsFolder\A.B_x!App")).is_ok());
        assert!(asked(None, Some(r"C:\x\notes.txt")).is_err());
        assert!(asked(None, Some("https://example.com")).is_err());
        assert!(asked(None, Some("shell:startup")).is_err());
        assert!(asked(Some("a"), Some("b.exe")).is_err());
        assert!(asked(None, None).is_err());
        assert!(asked(Some("cmd /c del"), None).is_err());
    }

    #[test]
    fn horadric_is_never_started() {
        for a in ["horadric", "Horadric.exe", "horadric-host"] {
            assert_eq!(asked(Some(a), None), Err(OWN.to_string()), "{a}");
        }
        assert_eq!(
            asked(
                None,
                Some(r"C:\Users\x\AppData\Local\Horadric\horadric.exe.")
            ),
            Err(OWN.to_string())
        );
        assert!(own(r"C:\h\horadricw.exe"));
        assert!(!own(r"C:\x\slack.exe"));
    }

    #[test]
    fn a_folder_id_is_read_in_any_case() {
        assert_eq!(folder_id(r"shell:AppsFolder\A.B_x!App"), Some("A.B_x!App"));
        assert_eq!(folder_id("SHELL:appsfolder/chrome"), Some("chrome"));
        assert_eq!(folder_id(r"shell:AppsFolder\"), None);
        assert_eq!(folder_id("shell:startup"), None);
        assert!(packaged("MSTeams_8wekyb3d8bbwe!MSTeams"));
        assert!(!packaged("Microsoft.Office.OUTLOOK.EXE.15"));
        assert!(!packaged("!App"));
    }

    #[test]
    fn what_runs_is_known_by_its_process_names() {
        assert_eq!(running_names(&Asked::Name("slack".into())), ["slack"]);
        assert_eq!(
            running_names(&Asked::Name("edge".into())),
            ["edge", "msedge"]
        );
        assert_eq!(
            running_names(&Asked::Path(r"C:\P\Microsoft VS Code\Code.exe".into())),
            ["code"]
        );
        assert_eq!(
            running_names(&Asked::Path(
                r"shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams".into()
            )),
            ["ms-teams"]
        );
        let list = [
            win(1, "Slack.exe", 10, None),
            win(2, "notepad.exe", 11, None),
            win(3, "slack.exe", 10, Some(1)),
        ];
        let open = running(&list, &["slack".into()]);
        assert_eq!(open.iter().map(|w| w.hwnd).collect::<Vec<_>>(), [1, 3]);
        assert!(running(&list, &["outlook".into()]).is_empty());
    }

    #[test]
    fn the_window_a_start_brought_is_the_new_one_of_its_process_or_name() {
        let names = ["slack".to_string()];
        let before = [1];
        // Its own process's main window first, a dialog of it last.
        let now = [
            win(1, "slack.exe", 5, None),
            win(4, "other.exe", 9, None),
            win(3, "tool.exe", 7, Some(2)),
            win(2, "tool.exe", 7, None),
        ];
        assert_eq!(
            opened(&before, &now, Some(7), &names).map(|w| w.hwnd),
            Some(2)
        );
        // A copy that handed its start to the one running exits: the new
        // window is of that one, by name.
        let now = [win(1, "slack.exe", 5, None), win(6, "Slack.exe", 5, None)];
        assert_eq!(
            opened(&before, &now, Some(8), &names).map(|w| w.hwnd),
            Some(6)
        );
        // A window open before is never taken for the new one.
        assert_eq!(opened(&before, &now[..1], Some(5), &names), None);
        let dialog = [win(7, "tool.exe", 7, Some(1))];
        assert_eq!(
            opened(&before, &dialog, Some(7), &names).map(|w| w.hwnd),
            Some(7)
        );
    }

    #[test]
    fn a_command_runner_with_arguments_is_risky() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(start_risk("cmd.exe", &args(&["/c", "del", "x"])));
        assert!(start_risk(r"C:\W\PowerShell.EXE", &args(&["-c", "x"])));
        assert!(!start_risk("cmd.exe", &[]));
        assert!(!start_risk("excel.exe", &args(&[r"C:\x\book.xlsx"])));
    }

    #[test]
    fn args_are_a_list_or_one_tail() {
        assert_eq!(args_of(None), Ok(vec![]));
        assert_eq!(
            args_of(Some(&json!(["a b", "c"]))),
            Ok(vec!["a b".into(), "c".into()])
        );
        assert_eq!(
            args_of(Some(&json!(r#""a b" c"#))),
            Ok(vec!["a b".into(), "c".into()])
        );
        assert!(args_of(Some(&json!([1]))).is_err());
        assert!(args_of(Some(&json!(3))).is_err());
    }

    /// Looks programs up the way a call does, and starts nothing.
    #[test]
    fn a_name_is_found_where_windows_finds_it() {
        let found = |name: &str| {
            let asked = Asked::Name(name.into());
            in_sta(move || resolve(&asked, vec![])).unwrap()
        };
        let notepad = found("notepad").unwrap();
        assert!(
            matches!(&notepad.start, Start::Exe(p) if p.to_lowercase().ends_with("notepad.exe"))
        );
        assert_eq!(notepad.exe.to_lowercase(), "notepad.exe");
        assert!(notepad.names.contains(&"notepad".to_string()));
        let none = found("no-such-program-anywhere").unwrap_err();
        assert!(none.contains("give its path"), "{none}");
    }

    #[test]
    fn the_question_names_the_program_and_its_arguments() {
        let plan = plan_exe(
            "Slack".into(),
            r"C:\U\slack\slack.exe".into(),
            vec!["--x".into()],
            vec!["slack".into()],
        );
        assert_eq!(plan.exe, "slack.exe");
        assert_eq!(
            what_of(&plan),
            r"start Slack (C:\U\slack\slack.exe) with --x"
        );
        let store = plan_packaged(
            "Teams".into(),
            "MSTeams_8wekyb3d8bbwe!MSTeams",
            vec![],
            vec!["ms-teams".into()],
        );
        assert_eq!(store.exe, "ms-teams.exe");
        assert!(store.path.is_empty());
        let unknown = plan_packaged("Thing".into(), "A.B_x!App", vec![], vec!["thing".into()]);
        assert_eq!(unknown.exe, "A.B_x!App");
    }
}
