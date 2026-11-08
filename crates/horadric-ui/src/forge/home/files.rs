//! What the home's page may do to the names it lists: rename, duplicate,
//! move, delete to the Recycle Bin, make a folder, and keep files dropped
//! on it.
//!
//! Every name comes from the page, and a page can say anything, so each
//! is checked to be one plain name of something in a folder of the served
//! root, never a link. Nothing is ever written over: a copy, a move or a
//! file dropped in takes the first name free, and a rename to a name in
//! use is refused. All of it reads and writes the disk, so it runs on a
//! worker thread.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
use windows::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_NO_CONNECTED_ELEMENTS,
    FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW,
};

use super::within;

/// The longest name Windows takes for one path segment.
const LONGEST: usize = 255;

/// How many free names are tried before giving up: a thousand copies of
/// one file is a script gone wrong, not work.
const TRIES: usize = 1000;

/// How many folders the move picker lists, and how deep it looks.
const FOLDERS_MOST: usize = 3000;
const FOLDERS_DEPTH: usize = 8;

fn refuse(text: impl Into<String>) -> io::Error {
    io::Error::other(text.into())
}

/// Whether `name` is one plain name Windows can hold: no separators, no
/// `.` or `..`, none of the characters a path refuses, no device name,
/// and not ending in a dot or space, which Windows would silently drop.
pub fn name_ok(name: &str) -> bool {
    if name.is_empty() || name.len() > LONGEST || name == "." || name == ".." {
        return false;
    }
    if name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c)) {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    !device
}

/// The repository's own folder: losing it loses every commit, and no page
/// should be one key away from that.
fn guarded(name: &str) -> bool {
    name.eq_ignore_ascii_case(".git")
}

/// `dir` checked to be a folder in `root`, no link on the way out of it.
fn folder(root: &Path, dir: &Path) -> io::Result<PathBuf> {
    if !within(root, dir) || !dir.is_dir() {
        return Err(refuse("This folder is not the project's."));
    }
    Ok(dir.to_path_buf())
}

/// The thing called `name` in the folder `dir` of `root`: there, and
/// neither a link nor a junction, which could lead anywhere on the disk.
pub fn item(root: &Path, dir: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = folder(root, dir)?;
    if !name_ok(name) {
        return Err(refuse(format!("\"{name}\" is not a name a file can have.")));
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(refuse(format!("{name} is a link, left alone.")))
        }
        Ok(_) => Ok(path),
        Err(_) => Err(refuse(format!("{name} is not here any more."))),
    }
}

/// The folder at `rel` (names joined by `/`, empty for the top) in
/// `root`, for a move to land in.
pub fn folder_at(root: &Path, rel: &str) -> io::Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        if !name_ok(seg) {
            return Err(refuse("That folder is not the project's."));
        }
        dir.push(seg);
    }
    let plain =
        std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
    if !plain || !within(root, &dir) {
        return Err(refuse("That folder is not here any more."));
    }
    Ok(dir)
}

/// `name` split for numbering: `notes.md` to `notes` and `.md`, a folder
/// or a dotfile whole.
fn split(name: &str, dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(at) if at > 0 && !dir => (&name[..at], &name[at..]),
        _ => (name, ""),
    }
}

/// The `n`th name to try for a thing called `stem` + `ext`: the name
/// itself, then `stem 2.ext` on.
fn nth(stem: &str, ext: &str, n: usize) -> String {
    if n == 1 {
        format!("{stem}{ext}")
    } else {
        format!("{stem} {n}{ext}")
    }
}

/// Tries `make` on the names `stem 1..` in `dir` until one is free.
/// `make` must fail with `AlreadyExists` on a name in use, never write
/// over it, so two at once still never take one name.
fn first_free(
    dir: &Path,
    stem: &str,
    ext: &str,
    mut make: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    for n in 1..TRIES {
        let name = nth(stem, ext, n);
        if name.len() > LONGEST {
            break;
        }
        let path = dir.join(name);
        match make(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(refuse("There is no free name left for it here."))
}

/// Moves `from` to `to`, never over something already there.
fn move_new(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() && !same_file(from, to) {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    // Without MOVEFILE_REPLACE_EXISTING Windows itself refuses a name in
    // use, closing the gap between the look above and the move.
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            MOVE_FILE_FLAGS(0),
        )
    }
    .map_err(|e| {
        // ERROR_FILE_EXISTS and ERROR_ALREADY_EXISTS.
        if matches!(e.code().0 as u32 & 0xFFFF, 80 | 183) {
            io::ErrorKind::AlreadyExists.into()
        } else {
            io::Error::from_raw_os_error((e.code().0 as u32 & 0xFFFF) as i32)
        }
    })
}

/// Whether two paths name one thing, as a rename that only changes case
/// does on Windows.
fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Gives `name` in `dir` the name `to`, refused when another has it.
pub fn rename(root: &Path, dir: &Path, name: &str, to: &str) -> io::Result<PathBuf> {
    let from = item(root, dir, name)?;
    if guarded(name) {
        return Err(refuse("The repository's own folder is left alone."));
    }
    if !name_ok(to) {
        return Err(refuse(format!("\"{to}\" is not a name a file can have.")));
    }
    if name == to {
        return Ok(from);
    }
    let dest = from.with_file_name(to);
    move_new(&from, &dest).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => refuse(format!("There is a {to} here already.")),
        _ => e,
    })?;
    Ok(dest)
}

/// A copy of `name` beside it: `notes copy.md`, then `notes copy 2.md`.
/// A folder is copied whole, its links left out.
pub fn duplicate(root: &Path, dir: &Path, name: &str) -> io::Result<PathBuf> {
    let from = item(root, dir, name)?;
    if guarded(name) {
        return Err(refuse("The repository's own folder is left alone."));
    }
    let is_dir = from.is_dir();
    let (stem, ext) = split(name, is_dir);
    let stem = format!("{stem} copy");
    let parent = from.parent().unwrap_or(dir).to_path_buf();
    if is_dir {
        let made = first_free(&parent, &stem, ext, |p| std::fs::create_dir(p))?;
        copy_tree(&from, &made)?;
        Ok(made)
    } else {
        first_free(&parent, &stem, ext, |p| copy_new(&from, p))
    }
}

/// Copies the file `from` to `to`, made new, so nothing is written over.
fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = OpenOptions::new().write(true).create_new(true).open(to)?;
    let copied = io::copy(&mut src, &mut dst).and_then(|_| dst.flush());
    if copied.is_err() {
        drop(dst);
        let _ = std::fs::remove_file(to);
    }
    copied
}

/// Copies what is in the folder `from` into the new folder `to`.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let dest = to.join(e.file_name());
        if ft.is_dir() {
            std::fs::create_dir(&dest)?;
            copy_tree(&e.path(), &dest)?;
        } else {
            copy_new(&e.path(), &dest)?;
        }
    }
    Ok(())
}

/// Moves `name` from `dir` into the folder `target`, under the first name
/// free there. A folder never goes into itself.
pub fn move_into(root: &Path, dir: &Path, name: &str, target: &Path) -> io::Result<PathBuf> {
    let from = item(root, dir, name)?;
    if guarded(name) {
        return Err(refuse("The repository's own folder is left alone."));
    }
    let target = folder(root, target)?;
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (f, t) = (canon(&from), canon(&target));
    if t.starts_with(&f) {
        return Err(refuse("A folder cannot go into itself."));
    }
    if from.parent().is_some_and(|p| canon(p) == t) {
        return Err(refuse(format!("{name} is in that folder already.")));
    }
    let (stem, ext) = split(name, from.is_dir());
    first_free(&target, stem, ext, |p| move_new(&from, p))
}

/// A new folder in `dir`: `New folder`, then `New folder 2`.
pub fn new_folder(root: &Path, dir: &Path) -> io::Result<PathBuf> {
    let dir = folder(root, dir)?;
    first_free(&dir, "New folder", "", |p| std::fs::create_dir(p))
}

/// Keeps `bytes`, a file dropped on the page, in `dir` under `name` or
/// the first name free after it.
pub fn keep(root: &Path, dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let dir = folder(root, dir)?;
    if !name_ok(name) {
        return Err(refuse(format!("\"{name}\" is not a name a file can have.")));
    }
    let (stem, ext) = split(name, false);
    first_free(&dir, stem, ext, |p| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
        let wrote = f.write_all(bytes).and_then(|_| f.flush());
        if wrote.is_err() {
            drop(f);
            let _ = std::fs::remove_file(p);
        }
        wrote
    })
}

/// Sends `names` in `dir` to the Recycle Bin, where they can be had back.
/// Where a drive keeps none (a share, a removable disk), Windows asks
/// before deleting for good rather than doing it unasked.
pub fn recycle(root: &Path, dir: &Path, names: &[String]) -> io::Result<()> {
    let mut paths = Vec::new();
    for n in names {
        if guarded(n) {
            return Err(refuse("The repository's own folder is left alone."));
        }
        paths.push(item(root, dir, n)?);
    }
    if paths.is_empty() {
        return Ok(());
    }
    // A list of paths, each ended by a NUL and the list by another.
    let mut from: Vec<u16> = Vec::new();
    for p in &paths {
        from.extend(p.as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);
    let flags = FOF_ALLOWUNDO
        | FOF_NOCONFIRMATION
        | FOF_WANTNUKEWARNING
        | FOF_SILENT
        | FOF_NOERRORUI
        | FOF_NO_CONNECTED_ELEMENTS;
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: flags.0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted.as_bool() {
        return Err(refuse("Left as it was."));
    }
    if r != 0 {
        return Err(refuse(format!(
            "Windows would not move it to the Recycle Bin (code {r:#x})."
        )));
    }
    Ok(())
}

/// The folders of `root` a thing can be moved into, as paths under it
/// joined by `/`, the top first as the empty path. Bounded, and skipping
/// what the listing tucks away and every link.
pub fn folders(root: &Path) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(names) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here: Vec<(PathBuf, String)> = Vec::new();
        for e in names.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if super::tucked(&name) {
                continue;
            }
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            here.push((e.path(), r));
        }
        for (path, r) in here {
            if out.len() >= FOLDERS_MOST {
                return sorted(out);
            }
            out.push(r.clone());
            if depth + 1 < FOLDERS_DEPTH {
                stack.push((path, r, depth + 1));
            }
        }
    }
    sorted(out)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort_by(|a, b| super::natural(a, b));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project folder `root` inside a scratch folder holding a secret
    /// beside it, which no operation may reach.
    fn project(name: &str) -> (PathBuf, PathBuf) {
        let outer = scratch(name);
        let root = outer.join("project");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(outer.join("secret.md"), "secret").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        (outer, root)
    }

    #[test]
    fn only_plain_names_pass() {
        for ok in ["a.md", "my notes.md", ".env", "x", "caf\u{e9}.md", "a..b"] {
            assert!(name_ok(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "..\\x",
            "../x",
            "c:x",
            "a?",
            "a*",
            "a|b",
            "a\"b",
            "<a>",
            "trailing.",
            "trailing ",
            " leading",
            "CON",
            "con.txt",
            "nul",
            "COM1",
            "lpt9.md",
            "a\u{1}",
        ] {
            assert!(!name_ok(bad), "{bad:?}");
        }
        assert!(name_ok("COM10"), "only COM1 to COM9 are devices");
        assert!(!name_ok(&"x".repeat(256)));
    }

    #[test]
    fn nothing_reaches_out_of_the_root() {
        let (outer, root) = project("out");
        let docs = root.join("docs");
        // Names that climb, through every operation.
        for bad in ["..", "../secret.md", "..\\secret.md", "C:\\x"] {
            assert!(item(&root, &root, bad).is_err(), "{bad}");
            assert!(rename(&root, &root, bad, "b.md").is_err(), "{bad}");
            assert!(duplicate(&root, &root, bad).is_err(), "{bad}");
            assert!(move_into(&root, &root, bad, &docs).is_err(), "{bad}");
            assert!(recycle(&root, &root, &[bad.to_string()]).is_err(), "{bad}");
            assert!(keep(&root, &root, bad, b"x").is_err(), "{bad}");
        }
        assert!(rename(&root, &root, "a.md", "../escaped.md").is_err());
        // A folder outside the root is refused as the place to work in.
        assert!(item(&root, &outer, "secret.md").is_err());
        assert!(rename(&root, &outer, "secret.md", "x.md").is_err());
        assert!(duplicate(&root, &outer, "secret.md").is_err());
        assert!(recycle(&root, &outer, &["secret.md".into()]).is_err());
        assert!(keep(&root, &outer, "x.md", b"x").is_err());
        assert!(new_folder(&root, &outer).is_err());
        assert!(move_into(&root, &root, "a.md", &outer).is_err());
        assert!(move_into(&root, &root.join("docs").join(".."), "a.md", &outer).is_err());
        // And as the place to move to, named from the page.
        assert!(folder_at(&root, "..").is_err());
        assert!(folder_at(&root, "docs/..").is_err());
        assert!(folder_at(&root, "docs/../..").is_err());
        assert!(folder_at(&root, "nope").is_err());
        assert_eq!(folder_at(&root, "docs").unwrap(), docs);
        assert_eq!(folder_at(&root, "").unwrap(), root);
        assert_eq!(
            std::fs::read_to_string(outer.join("secret.md")).unwrap(),
            "secret"
        );
        assert!(!outer.join("escaped.md").exists() && !outer.join("x.md").exists());
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn a_link_out_is_never_followed() {
        let (outer, root) = project("link");
        let elsewhere = outer.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("b.md"), "b").unwrap();
        // A junction needs no rights to make, unlike a symbolic link.
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("jump"))
            .arg(&elsewhere)
            .output()
            .is_ok_and(|o| o.status.success());
        if made {
            assert!(item(&root, &root, "jump").is_err());
            assert!(item(&root, &root.join("jump"), "b.md").is_err());
            assert!(folder_at(&root, "jump").is_err());
            assert!(move_into(&root, &root, "a.md", &root.join("jump")).is_err());
            assert!(keep(&root, &root.join("jump"), "c.md", b"c").is_err());
            assert!(!folders(&root).iter().any(|f| f == "jump"));
            assert!(root.join("a.md").exists() && !elsewhere.join("c.md").exists());
        }
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn nothing_is_written_over() {
        let (outer, root) = project("over");
        std::fs::write(root.join("b.md"), "b").unwrap();
        let e = rename(&root, &root, "a.md", "b.md").unwrap_err();
        assert!(e.to_string().contains("b.md here already"));
        assert_eq!(std::fs::read_to_string(root.join("b.md")).unwrap(), "b");
        // Only the case changing is the same file, not another.
        let up = rename(&root, &root, "a.md", "A.md").unwrap();
        assert_eq!(up.file_name().unwrap(), "A.md");
        let names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"A.md".to_string()));
        rename(&root, &root, "A.md", "a.md").unwrap();

        let c1 = duplicate(&root, &root, "a.md").unwrap();
        let c2 = duplicate(&root, &root, "a.md").unwrap();
        assert_eq!(c1.file_name().unwrap(), "a copy.md");
        assert_eq!(c2.file_name().unwrap(), "a copy 2.md");
        assert_eq!(std::fs::read_to_string(&c2).unwrap(), "a");

        std::fs::write(root.join("docs").join("a.md"), "theirs").unwrap();
        let moved = move_into(&root, &root, "a.md", &root.join("docs")).unwrap();
        assert_eq!(moved.file_name().unwrap(), "a 2.md");
        assert_eq!(
            std::fs::read_to_string(root.join("docs").join("a.md")).unwrap(),
            "theirs"
        );
        assert!(!root.join("a.md").exists());

        let k1 = keep(&root, &root, "b.md", b"dropped").unwrap();
        assert_eq!(k1.file_name().unwrap(), "b 2.md");
        assert_eq!(std::fs::read_to_string(root.join("b.md")).unwrap(), "b");

        let f1 = new_folder(&root, &root).unwrap();
        let f2 = new_folder(&root, &root).unwrap();
        assert_eq!(f1.file_name().unwrap(), "New folder");
        assert_eq!(f2.file_name().unwrap(), "New folder 2");
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn a_folder_is_copied_whole_and_never_moved_into_itself() {
        let (outer, root) = project("tree");
        std::fs::create_dir_all(root.join("docs").join("deep")).unwrap();
        std::fs::write(root.join("docs").join("deep").join("x.md"), "x").unwrap();
        let copy = duplicate(&root, &root, "docs").unwrap();
        assert_eq!(copy.file_name().unwrap(), "docs copy");
        assert_eq!(
            std::fs::read_to_string(copy.join("deep").join("x.md")).unwrap(),
            "x"
        );
        assert!(move_into(&root, &root, "docs", &root.join("docs").join("deep")).is_err());
        assert!(move_into(&root, &root, "docs", &root.join("docs")).is_err());
        assert!(
            move_into(&root, &root, "a.md", &root).is_err(),
            "already there"
        );
        assert!(rename(&root, &root, ".git", "x").is_err());
        let listed = folders(&root);
        assert_eq!(listed[0], "");
        assert!(listed.contains(&"docs/deep".to_string()));
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn numbering_keeps_the_extension() {
        assert_eq!(split("notes.md", false), ("notes", ".md"));
        assert_eq!(split(".env", false), (".env", ""));
        assert_eq!(split("v1.2", true), ("v1.2", ""));
        assert_eq!(nth("notes copy", ".md", 3), "notes copy 3.md");
    }
}
