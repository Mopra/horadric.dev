//! What is open on the desk and at the human's, and starting an app on
//! the desk. What to start instead of an app that can not run twice is
//! decided in `horadric_core::desk_apps`; here it is looked up and run.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use horadric_core::desk_apps::{self, Choice, Launch, Open, Way};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_NOEXPAND,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY,
};
use windows::Win32::System::RemoteDesktop::{
    WTSEnumerateProcessesW, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTS_PROCESS_INFOW,
};
use windows::Win32::System::Threading::{
    CreateProcessW, OpenProcess, QueryFullProcessImageNameW, PROCESS_ACCESS_RIGHTS,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, GWL_EXSTYLE, SW_SHOWNORMAL, WS_EX_TOOLWINDOW,
};

/// A process of this machine, in whichever session.
pub(super) struct Process {
    pub pid: u32,
    pub session: u32,
    /// Its file name, as Windows spells it.
    pub name: String,
}

/// Every process of this machine with its session. From the terminal
/// services list, which says the session without opening the process.
pub(super) fn processes() -> Vec<Process> {
    let mut list: *mut WTS_PROCESS_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: both out pointers outlive the call; no server handle means
    // this machine.
    let listed = unsafe { WTSEnumerateProcessesW(None, 0, 1, &mut list, &mut count) };
    if listed.is_err() || list.is_null() {
        return Vec::new();
    }
    // SAFETY: Windows handed `count` entries at `list`, each name a NUL
    // ended string (or null), all alive until the list is freed below.
    unsafe {
        let all = std::slice::from_raw_parts(list, count as usize)
            .iter()
            .map(|p| Process {
                pid: p.ProcessId,
                session: p.SessionId,
                name: if p.pProcessName.is_null() {
                    String::new()
                } else {
                    p.pProcessName.to_string().unwrap_or_default()
                },
            })
            .collect();
        WTSFreeMemory(list.cast());
        all
    }
}

/// A process open with `rights`, closed when dropped.
pub(super) fn open(pid: u32, rights: PROCESS_ACCESS_RIGHTS) -> Option<OwnedHandle> {
    // SAFETY: no pointers go in, and the handle that comes out is owned
    // from here on and closed nowhere else.
    unsafe {
        let handle = OpenProcess(rights, false, pid).ok()?;
        Some(OwnedHandle::from_raw_handle(handle.0))
    }
}

/// The full path of an open process's program.
pub(super) fn image(process: &OwnedHandle) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the handle is open, and the buffer and its length describe
    // the same array.
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(process.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn text(buf: &[u16], len: i32) -> String {
    String::from_utf16_lossy(&buf[..(len.max(0) as usize).min(buf.len())])
}

/// The windows open on the desk: the visible top level ones with a
/// title, each with its program's name.
pub(super) fn on_the_desk() -> Vec<Open> {
    unsafe extern "system" fn each(window: HWND, found: LPARAM) -> BOOL {
        // SAFETY: `found` is the list below, alive for the whole of
        // EnumWindows, which calls this on the thread that owns it.
        let found = unsafe { &mut *(found.0 as *mut Vec<Open>) };
        found.extend(shown(window));
        BOOL(1)
    }
    let mut found: Vec<Open> = Vec::new();
    // SAFETY: `each` matches what EnumWindows calls, and `found`
    // outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&raw mut found as isize)) };
    found
}

/// A window as `/apps` lists it, None for one a person would not call
/// an open window.
fn shown(window: HWND) -> Option<Open> {
    // SAFETY: a window gone meanwhile makes each call answer nothing;
    // every buffer is a local array handed with its own length.
    unsafe {
        if !IsWindowVisible(window).as_bool()
            || GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return None;
        }
        // A store app kept suspended has a visible window that is drawn
        // nowhere: cloaked.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(window, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4);
        if cloaked != 0 {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(window, &mut buf);
        let title = text(&buf, len);
        if title.trim().is_empty() {
            return None;
        }
        // The desktop itself is a titled window of the shell's.
        let mut class = [0u16; 64];
        let len = GetClassNameW(window, &mut class);
        if text(&class, len) == "Progman" {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let app = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
            .and_then(|process| image(&process))
            .map_or_else(|| "unknown".to_string(), |path| desk_apps::process_name(&path));
        Some(Open { app, title })
    }
}

/// The known apps running in the human's own session, by process name.
///
/// Windows of another session can not be listed from here: a session
/// sees only its own window station. So it is the processes of the
/// console's session, and of those only the apps the twin table knows,
/// which is all the list is for and tells a session nothing else of
/// what the human has running. A process without a window counts, and
/// rightly: a Chrome left running in the background takes a start over
/// just the same.
pub(super) fn at_the_humans() -> Vec<String> {
    // SAFETY: takes nothing and hands a number back. Inside the desk it
    // is the session the desk is a child of.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let mut names: Vec<String> = processes()
        .iter()
        .filter(|p| p.session == console)
        .map(|p| desk_apps::process_name(&p.name))
        .filter(|name| desk_apps::known_process(name))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What was started, for the answer.
pub(super) struct Started {
    pub app: String,
    pub instead_of: Option<&'static str>,
    pub why: Option<String>,
}

/// Starts what was asked for on the desk, or what the twin rule puts in
/// its place. An error is the refusal in plain words.
pub(super) fn launch(what: Launch) -> Result<Started, String> {
    let human = at_the_humans();
    let (path, args) = match what {
        Launch::App { app, args } => return start(desk_apps::plan(&app, &args, &human)?),
        Launch::Path { path, args } => (path, args),
    };
    if desk_apps::is_web(&path) {
        let mut all = vec![path];
        all.extend(args);
        return start(desk_apps::plan("msedge", &all, &human)?);
    }
    let lower = path.trim().to_ascii_lowercase();
    if lower.starts_with("shell:") || !lower.ends_with(".exe") {
        shell(&path, &args)?;
        let app = if lower.starts_with("shell:") {
            path.trim().to_string()
        } else {
            desk_apps::process_name(&path)
        };
        return Ok(Started {
            app,
            instead_of: None,
            why: None,
        });
    }
    // A known app is under the twin rule by its path as by its name.
    let name = desk_apps::process_name(&path);
    let mut why = None;
    if desk_apps::find(&name).is_some() {
        let choices = desk_apps::plan(&name, &args, &human)?;
        if choices.iter().any(|c| c.instead_of.is_some()) {
            return start(choices);
        }
        why = choices.into_iter().next().and_then(|c| c.why);
    }
    create(path.trim(), &args)?;
    Ok(Started {
        app: name,
        instead_of: None,
        why,
    })
}

/// Starts the first of `choices` that is installed and starts.
fn start(choices: Vec<Choice>) -> Result<Started, String> {
    let mut tried = Vec::new();
    let mut stood_in = None;
    for choice in choices {
        let own = format!("{}.exe", choice.app);
        let started = if choice.ways.is_empty() {
            by_name(&own, &choice.args)
        } else {
            choice.ways.iter().any(|way| match way {
                Way::Exe(file) => by_name(file, &choice.args),
                Way::Packaged(id) => {
                    shell(&format!("shell:AppsFolder\\{id}"), &choice.args).is_ok()
                }
            })
        };
        if started {
            return Ok(Started {
                app: choice.app,
                instead_of: choice.instead_of,
                why: choice.why,
            });
        }
        if choice.instead_of.is_some() {
            stood_in = choice.why;
        }
        tried.push(choice.app);
    }
    let tried = tried.join(" and ");
    Err(match stood_in {
        Some(why) => format!("{why}, and {tried} could not be started in its place"),
        None => format!("{tried} was not found or did not start: give its path"),
    })
}

/// Starts a program by its file name, true when it started.
fn by_name(file: &str, args: &[String]) -> bool {
    app_path(file)
        .or_else(|| on_the_path(file))
        .is_some_and(|exe| create(&exe, args).is_ok())
}

/// Where the App Paths key says a program is, the user's own entry
/// before the machine's. It is where Windows itself looks a bare name
/// up for the Run box.
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
        // SAFETY: the key outlives the call, and the buffer is handed
        // with its size in bytes, which `size` comes back within.
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
        // Not `is_file`: a store app's entry is an alias that can be
        // started but not opened to look at.
        std::fs::symlink_metadata(&path).is_ok().then_some(path)
    })
}

/// A program Windows finds by its name alone: in its own folders and on
/// the PATH, as for Notepad, which has no App Paths entry.
fn on_the_path(file: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    // SAFETY: the name outlives the call and the buffer is a local array.
    let len = unsafe { SearchPathW(PCWSTR::null(), &HSTRING::from(file), None, Some(&mut buf), None) };
    (len > 0 && (len as usize) < buf.len()).then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// Starts a program in this session, with its own folder to work in.
fn create(exe: &str, args: &[String]) -> Result<(), String> {
    let mut line: Vec<u16> = desk_apps::command_line(exe, args)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // What CreateProcessW takes at most.
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
    // SAFETY: the program, the folder and the command line outlive the
    // call, the line is NUL ended and writable as asked, and no handle
    // is inherited.
    unsafe {
        CreateProcessW(
            &program,
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            folder.as_ref().map_or(PCWSTR::null(), |f| PCWSTR(f.as_ptr())),
            &startup,
            &mut made,
        )
        .map_err(|e| format!("{exe} did not start: {}", e.message()))?;
        let _ = CloseHandle(made.hThread);
        let _ = CloseHandle(made.hProcess);
    }
    super::manners::keep(exe);
    Ok(())
}

/// Has Windows open a document, a `shell:` target or anything else that
/// is not a program, as a double click would.
fn shell(target: &str, args: &[String]) -> Result<(), String> {
    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: pairs the CoInitializeEx below that succeeded,
                // on the same thread.
                unsafe { CoUninitialize() };
            }
        }
    }
    // The shell wants an apartment of the thread that asks.
    // SAFETY: no pointers; the pair is kept by `Com`.
    let _com = Com(
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok(),
    );
    let file = HSTRING::from(target.trim());
    let parameters = HSTRING::from(desk_apps::arguments(args));
    let mut run = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // No dialog on a failure, as nobody sits at the desk to close
        // it, and no work left for a message loop this thread lacks.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if args.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(parameters.as_ptr())
        },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the struct has its size set and its strings outlive the
    // call.
    unsafe { ShellExecuteExW(&mut run) }
        .map_err(|e| format!("{} did not open: {}", target.trim(), e.message()))?;
    if !run.hProcess.is_invalid() {
        // SAFETY: the handle came from ShellExecuteExW for this caller
        // to close, which the owner does.
        let process = unsafe { OwnedHandle::from_raw_handle(run.hProcess.0) };
        if let Some(path) = image(&process) {
            super::manners::keep(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_a_packaged_app_that_is_not_installed_fails_to_open() {
        // What the next way or the next twin is tried on.
        assert!(shell("shell:AppsFolder\\Horadric.NoSuchApp_0000000000000!None", &[]).is_err());
    }

    #[test]
    fn hand_this_test_is_among_the_processes() {
        let me = std::process::id();
        let mine = processes().into_iter().find(|p| p.pid == me).unwrap();
        assert!(mine.name.to_lowercase().ends_with(".exe"));
        let process = open(me, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let path = image(&process).unwrap();
        assert_eq!(desk_apps::process_name(&path), desk_apps::process_name(&mine.name));
    }
}
