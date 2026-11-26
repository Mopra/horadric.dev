//! The hand's manners at its start: the user's autostart programs all
//! come up on the desk too (a game launcher, a chat client, a kiosk app
//! that covers the screen), and the desk is a workplace, not a second
//! copy of the human's tray. So for its first minute the hand stops the
//! programs the Run keys and the Startup folders started, in its own
//! session and nowhere else. Which program a Run value names is parsed
//! in `horadric_core::desk_apps`.

use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use horadric_core::desk_apps::{self, Autostart};
use windows::core::{Interface, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_EXPAND_SZ, REG_SZ,
};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::System::Threading::{
    TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonStartup, FOLDERID_Startup, IShellLinkW, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DEFAULT,
};

use super::apps;

/// How long autostart programs are watched for: they trickle in over
/// the first minute of a sign-in.
const ROUNDS: u32 = 30;
const EVERY: Duration = Duration::from_secs(2);

const RUN_KEYS: [(HKEY, &str); 3] = [
    (
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
    ),
    (
        HKEY_LOCAL_MACHINE,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
    ),
    (
        HKEY_LOCAL_MACHINE,
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
    ),
];

/// The programs the hand started itself when asked to. One of them that
/// is an autostart program too is on the desk because a session wants
/// it there.
static KEPT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Notes that the hand started the program at `path` on purpose.
pub(super) fn keep(path: &str) {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.push(path.to_string());
}

/// Watches the desk's `session` for autostart programs and stops them,
/// on a thread of its own, for the first minute. Only to be called when
/// this process really is in the desk.
pub(super) fn start(session: u32, log_file: &'static str) {
    let spawned = thread::Builder::new()
        .name("desk-manners".into())
        .spawn(move || {
            let autostarts = autostarts();
            let windows = windows_folder();
            for _ in 0..ROUNDS {
                if !sweep(session, &autostarts, &windows, log_file) {
                    return;
                }
                thread::sleep(EVERY);
            }
        });
    if let Err(e) = spawned {
        crate::screen_share::log(log_file, &format!("no thread to stop autostarts on: {e}"));
    }
}

/// One look over the processes of `session`. False when this is no
/// longer a session to stop anything in.
fn sweep(session: u32, autostarts: &[Autostart], windows: &str, log_file: &str) -> bool {
    // Asked again each time: nothing is ever stopped in the session the
    // human sits at, whatever led here.
    // SAFETY: takes nothing and hands a number back.
    if session == unsafe { WTSGetActiveConsoleSessionId() } {
        return false;
    }
    let kept = KEPT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // An autostart entry the hand was asked to start is left alone from
    // then on, with all it names.
    let unwanted: Vec<&Autostart> = autostarts
        .iter()
        .filter(|a| !kept.iter().any(|k| a.is(k)))
        .collect();
    let me = std::process::id();
    for process in apps::processes() {
        if process.session != session || process.pid == me || process.pid == 0 {
            continue;
        }
        // The one handle is asked what the process is and then stops
        // it, so the pid can not have gone to another program between.
        let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
        let Some(handle) = apps::open(process.pid, rights) else {
            continue;
        };
        let Some(path) = apps::image(&handle) else {
            continue;
        };
        if desk_apps::spared(&path, windows) || !unwanted.iter().any(|a| a.is(&path)) {
            continue;
        }
        if stop(&handle, process.pid, session) {
            crate::screen_share::log(log_file, &format!("stopped the autostart {path}"));
        }
    }
    true
}

/// Stops the process behind `handle`, once Windows itself has said it
/// is in the desk's session.
fn stop(handle: &OwnedHandle, pid: u32, session: u32) -> bool {
    let mut theirs = 0u32;
    // SAFETY: `theirs` outlives the call. The pid is still this
    // process's, as the open handle keeps it from being given away.
    if unsafe { ProcessIdToSessionId(pid, &mut theirs) }.is_err() || theirs != session {
        return false;
    }
    // SAFETY: the handle is open with the right to terminate.
    unsafe { TerminateProcess(HANDLE(handle.as_raw_handle()), 1) }.is_ok()
}

fn windows_folder() -> String {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is a local array.
    let len = unsafe { GetWindowsDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len >= buf.len() {
        // With no answer the usual place is spared all the same.
        return r"C:\Windows".into();
    }
    String::from_utf16_lossy(&buf[..len])
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Every program the Run keys and the Startup folders start at sign-in.
fn autostarts() -> Vec<Autostart> {
    let mut all: Vec<Autostart> = RUN_KEYS
        .iter()
        .flat_map(|(hive, key)| run_values(*hive, key))
        .filter_map(|value| desk_apps::autostart(&value, &env))
        .collect();
    // SAFETY: no pointers; paired with the CoUninitialize below when it
    // succeeded, on this same thread.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    for folder in [&FOLDERID_Startup, &FOLDERID_CommonStartup] {
        let entries = known_folder(folder).and_then(|f| std::fs::read_dir(f).ok());
        for entry in entries.into_iter().flatten().flatten() {
            let path = entry.path();
            let is = |ext: &str| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            };
            let command = if is("lnk") {
                shortcut(&path)
            } else if is("exe") {
                Some(format!("\"{}\"", path.display()))
            } else {
                None
            };
            all.extend(command.and_then(|c| desk_apps::autostart(&c, &env)));
        }
    }
    if com {
        // SAFETY: pairs the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    all
}

/// The data of every text value under a Run key.
fn run_values(hive: HKEY, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut open = HKEY::default();
    // SAFETY: the key's name outlives the call, and `open` is closed
    // below when it was opened.
    if unsafe { RegOpenKeyExW(hive, &HSTRING::from(key), None, KEY_READ, &mut open) }
        != ERROR_SUCCESS
    {
        return values;
    }
    for index in 0.. {
        let mut name = [0u16; 512];
        let mut name_len = name.len() as u32;
        let mut data = [0u16; 4096];
        let mut size = std::mem::size_of_val(&data) as u32;
        let mut kind = 0u32;
        // SAFETY: every buffer is a local array handed with its own
        // length, in characters for the name and in bytes for the data.
        let read = unsafe {
            RegEnumValueW(
                open,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if read != ERROR_SUCCESS {
            // A value too long to be a command line is passed over, and
            // anything else is the end of the list.
            if read == windows::Win32::Foundation::ERROR_MORE_DATA {
                continue;
            }
            break;
        }
        if kind == REG_SZ.0 || kind == REG_EXPAND_SZ.0 {
            let units = &data[..(size as usize / 2).min(data.len())];
            let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
            values.push(String::from_utf16_lossy(&units[..end]));
        }
    }
    // SAFETY: opened above and closed nowhere else.
    let _ = unsafe { RegCloseKey(open) };
    values
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the string Windows hands over is read and then freed as
    // it asks, once.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

/// What a shortcut starts, as a command line. COM must be joined on
/// this thread.
fn shortcut(link: &Path) -> Option<String> {
    // SAFETY: the link's name outlives the calls, and each buffer is a
    // local array handed with its own length.
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = shell.cast().ok()?;
        file.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;
        let mut target = [0u16; 1024];
        shell.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        let mut arguments = [0u16; 2048];
        let _ = shell.GetArguments(&mut arguments);
        let text = |buf: &[u16]| {
            let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        };
        let target = text(&target);
        (!target.is_empty()).then(|| format!("\"{target}\" {}", text(&arguments)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_the_run_keys_read_without_stopping_anything() {
        // Reading is all this does: what is stopped is decided against
        // a session, which a test has no desk to give.
        let _ = autostarts();
        assert!(!windows_folder().is_empty());
    }

    #[test]
    fn hand_nothing_is_stopped_in_the_console_session() {
        // SAFETY: takes nothing and hands a number back.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        let everything = [Autostart {
            exe: String::new(),
            child: Some((String::new(), String::new())),
        }];
        assert!(!sweep(console, &everything, "", "desk-hand-test.log"));
    }
}
