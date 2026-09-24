//! Child processes in Windows pseudo consoles.
//!
//! A Horadric terminal is a real `claude` attached to a ConPTY: Windows runs
//! the console host, we get a byte stream of VT output and send VT input
//! back. This crate owns that plumbing and nothing else. Parsing the stream
//! into a grid is the UI's job.
//!
//! Written on the `windows` crate directly rather than `portable-pty`. The
//! whole API we need is five calls, and the wrapper would bring a trait
//! object design and a dozen crates for them.

#![cfg(windows)]

mod conpty;

pub use conpty::{Command, Pty};

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Finds `name` on a `PATH` style list, trying each extension in order.
///
/// `exists` is injected so the search can be tested without a disk.
pub fn find_program(
    name: &str,
    path_var: &OsStr,
    exts: &[&str],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Joins arguments into one Windows command line, quoted the way the MSVC
/// runtime splits them again.
pub fn command_line(program: &Path, args: &[String]) -> String {
    let mut out = String::new();
    push_quoted(&mut out, &program.to_string_lossy());
    for a in args {
        out.push(' ');
        push_quoted(&mut out, a);
    }
    out
}

fn push_quoted(out: &mut String, arg: &str) {
    let plain = !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']);
    if plain {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are escapes, so double them, then
                // escape the quote itself.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes sit before the closing quote.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// Builds a Unicode environment block: `KEY=value` pairs, each NUL
/// terminated, sorted the way Windows expects, with a final NUL.
///
/// Starts from `base`, drops every key in `remove`, then applies `set`.
/// Keys compare without case, as Windows does.
pub fn environment_block(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    set: &[(String, String)],
    remove: &[&str],
) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(k, _)| {
            let k = k.to_string_lossy();
            !remove.iter().any(|r| r.eq_ignore_ascii_case(&k))
                && !set.iter().any(|(s, _)| s.eq_ignore_ascii_case(&k))
        })
        .collect();
    vars.extend(set.iter().map(|(k, v)| (k.into(), v.into())));
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());

    let mut block = Vec::new();
    for (k, v) in &vars {
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    if vars.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// NUL terminated UTF-16, for the W functions.
pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_left_alone() {
        let line = command_line(Path::new("claude"), &["-p".into(), "hello".into()]);
        assert_eq!(line, "claude -p hello");
    }

    #[test]
    fn spaces_quotes_and_backslashes_are_quoted() {
        let args = vec![
            "Reply with pong".into(),
            r#"say "hi""#.into(),
            r"C:\dir with space\".into(),
            String::new(),
        ];
        let line = command_line(Path::new(r"C:\Program Files\claude.exe"), &args);
        assert_eq!(
            line,
            r#""C:\Program Files\claude.exe" "Reply with pong" "say \"hi\"" "C:\dir with space\\" """#
        );
    }

    #[test]
    fn finds_first_match_in_path_order() {
        let path = OsString::from(r"C:\a;C:\b");
        let found = find_program("claude", &path, &[".exe", ".cmd"], |p| {
            p == Path::new(r"C:\b\claude.exe") || p == Path::new(r"C:\a\claude.cmd")
        });
        // Directory order wins over extension order, as in cmd.exe.
        assert_eq!(found, Some(PathBuf::from(r"C:\a\claude.cmd")));
        assert_eq!(find_program("nope", &path, &[".exe"], |_| false), None);
    }

    #[test]
    fn environment_sets_removes_and_sorts() {
        let base = vec![
            ("Path".into(), r"C:\bin".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("horadric_session".into(), "stale".into()),
            ("ALPHA".into(), "a".into()),
        ];
        let block = environment_block(
            base,
            &[("HORADRIC_SESSION".into(), "fresh".into())],
            &["claudecode"],
        );
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "ALPHA=a\0HORADRIC_SESSION=fresh\0Path=C:\\bin\0\0");
    }

    #[test]
    fn empty_environment_is_two_nuls() {
        assert_eq!(environment_block(Vec::new(), &[], &[]), vec![0, 0]);
    }
}
