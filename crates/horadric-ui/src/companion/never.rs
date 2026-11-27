//! What the companion never watches: Horadric itself, password
//! managers, private browser windows, and whatever the human adds to
//! `companion-never.txt` in the state folder.
//!
//! A window that is never watched leaves nothing in the trail, not even
//! its title.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// The human's own list, in the state folder.
pub const LIST_FILE: &str = "companion-never.txt";

/// Password managers, by how their program's name starts: each ships
/// helpers beside the main program (`1Password-BrowserSupport.exe`,
/// `KeePassXC-proxy.exe`) that are no more to be read than it is.
const PASSWORD_MANAGERS: [&str; 11] = [
    "1password",
    "bitwarden",
    "keepass",
    "lastpass",
    "dashlane",
    "keeper",
    "enpass",
    "nordpass",
    "proton pass",
    "protonpass",
    "proton.pass",
];

/// Where Windows itself asks for a password or shows the lock screen.
const WINDOWS_OWN: [&str; 4] = ["credentialuibroker", "consent", "logonui", "lockapp"];

/// What a private browser window says in its title. Edge and Firefox
/// do; Chrome's incognito window says nothing there, so it is watched
/// unless the human lists it.
const PRIVATE_TITLES: [&str; 3] = ["inprivate", "incognito", "private browsing"];

const HEADER: &str = "\
# What Horadric's companion never watches, beside what it never does
# anyway: Horadric itself, password managers and private browser windows.
#
# One per line: a program's name (signal.exe) or a piece of a window's
# title (Online banking). Upper and lower case are the same. A window
# that fits a line leaves nothing in the trail, not even its title.
# Lines that start with # are notes. Saving this file is enough.
";

/// Whether a window of `program` (its file name, as `msedge.exe`) with
/// `title` is left alone.
pub fn never(program: &str, title: &str) -> bool {
    builtin(program, title) || listed(program, title)
}

/// The rules that need no file.
pub fn builtin(program: &str, title: &str) -> bool {
    own(program) || keeps_passwords(program) || private_window(title)
}

/// Horadric's own windows: what its sessions do is already known.
pub fn own(program: &str) -> bool {
    stem(program).starts_with("horadric")
}

/// A password manager, or Windows asking for a password.
pub fn keeps_passwords(program: &str) -> bool {
    let stem = stem(program);
    PASSWORD_MANAGERS.iter().any(|p| stem.starts_with(p)) || WINDOWS_OWN.contains(&stem.as_str())
}

/// A browser window that says in its title that it is private.
pub fn private_window(title: &str) -> bool {
    let title = title.to_lowercase();
    PRIVATE_TITLES.iter().any(|p| title.contains(p))
}

/// The program's name in lower case without `.exe`.
fn stem(program: &str) -> String {
    let lower = program.trim().to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// The human's list as read: program names and pieces of titles, in
/// lower case.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct List(Vec<String>);

impl List {
    pub fn parse(text: &str) -> List {
        List(
            text.lines()
                .map(|l| l.trim().to_lowercase())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect(),
        )
    }

    /// Whether a line names the program, with or without `.exe`, or is
    /// a piece of the title.
    pub fn hit(&self, program: &str, title: &str) -> bool {
        let stem = stem(program);
        let title = title.to_lowercase();
        self.0.iter().any(|line| {
            line.strip_suffix(".exe").unwrap_or(line) == stem || title.contains(line.as_str())
        })
    }
}

/// Where the human's list is: `state_dir()/companion-never.txt`.
pub fn path() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join(LIST_FILE))
}

/// Whether the human's list names this window. The file is read again
/// when it was saved since, and written with its explanation when it is
/// not there.
pub fn listed(program: &str, title: &str) -> bool {
    static KEPT: Mutex<Option<(Option<SystemTime>, List)>> = Mutex::new(None);
    let Some(path) = path() else { return false };
    let modified = fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if !kept.as_ref().is_some_and(|(at, _)| *at == modified) {
        let list = match fs::read_to_string(&path) {
            Ok(text) => List::parse(&text),
            Err(e) => {
                if e.kind() == ErrorKind::NotFound {
                    start(&path);
                } else {
                    eprintln!("horadric: {LIST_FILE} could not be read: {e}");
                }
                List::default()
            }
        };
        // The time from before the read: a save in between is then
        // seen at the next question, not missed.
        *kept = Some((modified, list));
    }
    kept.as_ref().is_some_and(|(_, list)| list.hit(program, title))
}

fn start(path: &std::path::Path) {
    let made = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, HEADER));
    if let Err(e) = made {
        eprintln!("horadric: {LIST_FILE} could not be made: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horadric_is_never_watched() {
        assert!(own("horadric.exe"));
        assert!(own("Horadric-Host-3.exe"));
        assert!(own("horadricw.exe"));
        assert!(!own("msedge.exe"));
        assert!(builtin("HORADRIC.EXE", "anything"));
    }

    #[test]
    fn password_managers_are_never_watched() {
        for program in [
            "1Password.exe",
            "Bitwarden.exe",
            "KeePass.exe",
            "KeePassXC.exe",
            "keepassxc-proxy.exe",
            "LastPass.exe",
            "Dashlane.exe",
            "keeperpasswordmanager.exe",
            "Enpass.exe",
            "NordPass.exe",
            "Proton Pass.exe",
            "ProtonPass.exe",
            "CredentialUIBroker.exe",
            "LogonUI.exe",
            "LockApp.exe",
            "consent.exe",
        ] {
            assert!(keeps_passwords(program), "{program}");
        }
        for program in ["msedge.exe", "OUTLOOK.EXE", "consentmanager.exe", ""] {
            assert!(!keeps_passwords(program), "{program}");
        }
    }

    #[test]
    fn a_private_window_says_so_in_its_title() {
        assert!(private_window("New tab - [InPrivate] - Microsoft Edge"));
        assert!(private_window("Bank - Mozilla Firefox Private Browsing"));
        assert!(private_window("New Incognito Tab - Google Chrome"));
        assert!(!private_window("Inbox - Outlook"));
        assert!(builtin("msedge.exe", "x - [InPrivate] - Microsoft Edge"));
        assert!(!builtin("msedge.exe", "x - Microsoft Edge"));
    }

    #[test]
    fn the_list_names_programs_and_pieces_of_titles() {
        let list = List::parse(
            "# my own\n\n  Signal.exe  \nteams\nOnline Banking\n#notepad.exe\n",
        );
        assert_eq!(
            list,
            List(vec![
                "signal.exe".into(),
                "teams".into(),
                "online banking".into()
            ])
        );
        assert!(list.hit("signal.exe", ""));
        assert!(list.hit("SIGNAL.EXE", ""));
        assert!(list.hit("Teams.exe", "Chat"));
        assert!(list.hit("msedge.exe", "Acme online banking - Edge"));
        assert!(!list.hit("notepad.exe", "notes.txt"));
        assert!(!list.hit("signalizer.exe", "x"));
        assert!(!List::default().hit("signal.exe", "anything"));
    }

    #[test]
    fn the_header_is_all_notes() {
        assert_eq!(List::parse(HEADER), List::default());
    }
}
