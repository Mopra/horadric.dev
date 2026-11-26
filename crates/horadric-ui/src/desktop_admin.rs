//! The administrators' list of desktop programs on disk: where it is, who
//! may have written it, and how it is written. See
//! [`horadric_core::desktop_programs`] for what it holds.
//!
//! It lives in `%ProgramData%\Horadric`, a folder only administrators can
//! write. Every user may make folders in `%ProgramData%`, so the folder
//! proves nothing by being there: before a word of the list is believed,
//! the folder and the file are each opened and their owner and access list
//! read off the open handle, and anything a program that is not elevated
//! could have written is refused with the reason.
//!
//! This one file is compiled into two crates, by path: the app reads the
//! list here, and so do the elevated helper and the `desktop` command in
//! the binary. The check must not exist twice.

// Each of the two crates uses only its part of this.
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use horadric_core::desktop_programs::{self as listed, List};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Security::{
    GetKernelObjectSecurity, GetTokenInformation, SetFileSecurityW, TokenElevation,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Lets a folder be opened as a file is, for its access list.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Opens a link itself, not what it points at.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// The administrators' folder. Asked of Windows, never of the environment:
/// the session that starts the elevated helper sets the helper's
/// environment, and must not get to say where its list is.
pub fn folder() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        Some(PathBuf::from(text?).join(listed::ADMIN_FOLDER))
    }
}

/// The administrators' list.
pub fn file() -> Option<PathBuf> {
    folder().map(|f| f.join(listed::FILE))
}

/// The elevated helper's log.
pub fn log_file() -> Option<PathBuf> {
    folder().map(|f| f.join(listed::ADMIN_LOG_FILE))
}

/// What reading the administrators' list came to.
#[derive(Debug, PartialEq)]
pub enum Read {
    /// There is none, which is an empty list.
    Missing,
    /// It is there but not used, and why: a user could have written it,
    /// or it does not read or parse.
    Refused(String),
    List(List),
}

/// Whether this process runs as administrator.
pub fn elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        read.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// The owner and access list of an open file or folder, read off the
/// handle so they are those of what is then read, not of whatever has
/// that name a moment later.
fn descriptor_of(open: &File) -> Result<listed::Descriptor, String> {
    let handle = HANDLE(open.as_raw_handle());
    let what = OWNER_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0;
    unsafe {
        let mut len = 0u32;
        let _ = GetKernelObjectSecurity(handle, what, None, 0, &mut len);
        if len == 0 {
            return Err("Windows would not say who may write it".into());
        }
        // In u64s, so the descriptor is aligned as Windows wants it.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let sd = PSECURITY_DESCRIPTOR(buf.as_mut_ptr().cast());
        GetKernelObjectSecurity(handle, what, Some(sd), len, &mut len)
            .map_err(|e| format!("Windows would not say who may write it: {e}"))?;
        let bytes = std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len as usize);
        listed::parse_descriptor(bytes)
    }
}

/// Opens `path`, a folder or a file, only if nobody but administrators
/// could have written it. None when it is not there.
fn guarded(path: &Path, is_folder: bool) -> Result<Option<File>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(if is_folder {
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    });
    let open = match options.open(path) {
        Ok(open) => open,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("it can not be opened: {e}")),
    };
    let meta = open
        .metadata()
        .map_err(|e| format!("it can not be read: {e}"))?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("it is a link to somewhere else".into());
    }
    if meta.is_dir() != is_folder {
        return Err(if is_folder {
            "it is a file where a folder belongs".into()
        } else {
            "it is a folder where a file belongs".into()
        });
    }
    listed::admins_only(&descriptor_of(&open)?)?;
    Ok(Some(open))
}

/// Reads the administrators' list, refusing it unless the folder and the
/// file are both the administrators' alone.
pub fn read() -> Read {
    let (Some(folder), Some(file)) = (folder(), file()) else {
        return Read::Missing;
    };
    let refused = |what: &Path, why: String| {
        Read::Refused(listed::unsafe_text(&what.display().to_string(), &why))
    };
    match guarded(&folder, true) {
        Ok(Some(_)) => {}
        Ok(None) => return Read::Missing,
        Err(why) => return refused(&folder, why),
    }
    let mut open = match guarded(&file, false) {
        Ok(Some(open)) => open,
        Ok(None) => return Read::Missing,
        Err(why) => return refused(&file, why),
    };
    let mut text = String::new();
    if let Err(e) = open.read_to_string(&mut text) {
        return Read::Refused(format!("{} can not be read: {e}", file.display()));
    }
    match listed::parse(&text) {
        Ok(list) => Read::List(list),
        Err(why) => Read::Refused(format!(
            "{} does not parse, so nothing on it is allowed: {why}",
            file.display()
        )),
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Makes the administrators' folder with its access list from the start,
/// so there is no moment it stands open. One that is there already is
/// left as it is: [`guarded`] says whether it will do.
fn make_folder(folder: &Path) -> Result<(), String> {
    let descriptor = listed::admin_descriptor(true);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr() as *mut _,
        bInheritHandle: false.into(),
    };
    match unsafe { CreateDirectoryW(PCWSTR(wide(folder).as_ptr()), Some(&attributes)) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => Ok(()),
        Err(e) => Err(format!("{} can not be made: {e}", folder.display())),
    }
}

/// Makes a file in the administrators' folder the administrators' own.
/// The owner Windows gives by default may be the administrator's own
/// account, which on most machines is the session's account too, and an
/// owner can always rewrite the access list: so the group is named.
fn secure(file: &Path) -> Result<(), String> {
    let descriptor = listed::admin_descriptor(false);
    let what = OWNER_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION;
    let secured = unsafe {
        SetFileSecurityW(
            PCWSTR(wide(file).as_ptr()),
            what,
            PSECURITY_DESCRIPTOR(descriptor.as_ptr() as *mut _),
        )
    };
    match secured.as_bool() {
        true => Ok(()),
        false => Err(windows::core::Error::from_thread().to_string()),
    }
}

/// Writes the administrators' list. Only an elevated process gets through
/// this; it never tries to become one. A folder a user made first is
/// refused rather than mended, since what is in it is not known.
pub fn write(list: &List) -> Result<PathBuf, String> {
    if !elevated() {
        return Err(NOT_ELEVATED.into());
    }
    let (folder, file) = folder()
        .zip(file())
        .ok_or("Windows would not say where %ProgramData% is")?;
    make_folder(&folder)?;
    let unsafe_folder = |why: String| {
        format!(
            "{} is not the administrators' alone: {why}. Delete the folder from this elevated \
             shell (it may have been made by a program that is not elevated) and run this again",
            folder.display()
        )
    };
    guarded(&folder, true)
        .map_err(unsafe_folder)?
        .ok_or_else(|| format!("{} is gone again", folder.display()))?;
    // Inside a folder only administrators write, so nothing can have put
    // a link in the way of these two names.
    let part = folder.join(format!("{}.part", listed::FILE));
    let said = |e: std::io::Error| format!("{}: {e}", file.display());
    std::fs::write(&part, listed::render(list)).map_err(said)?;
    if let Err(why) = secure(&part) {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "{} could not be made the administrators' alone: {why}",
            file.display()
        ));
    }
    std::fs::rename(&part, &file).map_err(said)?;
    match read() {
        Read::List(_) => Ok(file),
        Read::Missing => Err(format!("{} is gone again", file.display())),
        Read::Refused(why) => Err(format!("written, but {why}")),
    }
}

/// Said by a command that changes the administrators' list from a shell
/// that is not elevated.
pub const NOT_ELEVATED: &str = "this changes the administrators' list of desktop programs, which \
only an elevated shell can write. Open a terminal with \"Run as administrator\" and run it there. \
Horadric never asks Windows for that itself";

/// Adds a line to the elevated helper's log, when the folder is the
/// administrators' alone. An elevated process must not write where a
/// user's program could have put a link to some other file.
pub fn log(line: &str) {
    let (Some(folder), Some(file)) = (folder(), log_file()) else {
        return;
    };
    if !elevated() || make_folder(&folder).is_err() {
        return;
    }
    if !matches!(guarded(&folder, true), Ok(Some(_))) {
        return;
    }
    let new = !file.exists();
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if new {
        let _ = secure(&file);
    }
}
