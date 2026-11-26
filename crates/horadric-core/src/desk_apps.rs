//! Which apps the agent's desk may start, and what it starts instead of
//! one the human has open (the twin rule). Also the other pure parts of
//! the desk's hand: the bodies of `/apps` and `/launch`, a command line
//! as Windows reads it, and which programs the Run keys start at sign-in.
//! Pure, see `docs/plan/computer-use.md`.

use serde::{Deserialize, Serialize};

/// One way to start an app, tried in the order an app lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// A program by its file name, found through the App Paths key.
    Exe(&'static str),
    /// A packaged app by its AppUserModelId, started as
    /// `shell:AppsFolder\<id>`.
    Packaged(&'static str),
}

/// What happens when an app runs on the desk and at the human's at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Twice {
    /// It runs in both sessions.
    Fine,
    /// It goes wrong while the human has it open.
    Clash {
        /// What would go wrong, ending the sentence "<App> is open in
        /// your own session and".
        open: &'static str,
        /// What starting it on the desk means for the human's own copy
        /// afterwards, said when it is started.
        alone: &'static str,
    },
    /// It does not work on the desk at all, and this is why.
    Never(&'static str),
}

/// An app started in place of another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Twin {
    /// The stand in's first name in [`APPS`].
    pub app: &'static str,
    /// What it is started with, a web address for an app that has a web
    /// twin only.
    pub args: &'static [&'static str],
    /// Whether what the first app was asked to open goes to it too: a
    /// browser takes another browser's address, a mail program does not
    /// take another's switches.
    pub passes: bool,
}

/// An app the desk knows how to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct App {
    /// What it may be asked for by, lower case. The first is its name in
    /// answers.
    pub names: &'static [&'static str],
    /// Its name in a sentence.
    pub label: &'static str,
    /// The process names that mean it runs, lower case, without `.exe`.
    pub processes: &'static [&'static str],
    pub ways: &'static [Way],
    pub twice: Twice,
    /// What is started instead when [`App::twice`] says it can not be,
    /// the first that is installed. None means it is refused.
    pub twins: &'static [Twin],
}

const EDGE: Twin = Twin {
    app: "msedge",
    args: &[],
    passes: true,
};

/// The apps known. To add one: its names, its process, how it starts,
/// and what was seen when it ran in both sessions (test that first).
/// All of this was tested on 2026-11-26, the lore has the detail.
pub const APPS: &[App] = &[
    App {
        names: &["msedge", "edge"],
        label: "Edge",
        processes: &["msedge"],
        ways: &[Way::Exe("msedge.exe")],
        twice: Twice::Fine,
        twins: &[],
    },
    App {
        names: &["chrome"],
        label: "Chrome",
        processes: &["chrome"],
        ways: &[Way::Exe("chrome.exe")],
        // A second Chrome hands its start to the one running, wherever
        // that one is.
        twice: Twice::Clash {
            open: "would open its window there",
            alone: "Chrome now runs on the desk: a Chrome you start yourself while it does opens its window on the desk",
        },
        twins: &[EDGE],
    },
    App {
        names: &["outlook"],
        label: "Outlook",
        processes: &["outlook"],
        ways: &[Way::Exe("outlook.exe")],
        twice: Twice::Clash {
            open: "has its data file locked, so a second one hangs",
            alone: "Outlook now runs on the desk and holds its data file: your own Outlook hangs if you start it while it does",
        },
        twins: &[
            Twin {
                app: "olk",
                args: &[],
                passes: false,
            },
            Twin {
                app: "msedge",
                args: &["https://outlook.office.com"],
                passes: false,
            },
        ],
    },
    App {
        names: &["olk", "new-outlook"],
        label: "the new Outlook",
        processes: &["olk"],
        ways: &[
            Way::Packaged("Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows"),
            Way::Exe("olk.exe"),
        ],
        twice: Twice::Fine,
        twins: &[],
    },
    App {
        names: &["whatsapp"],
        label: "WhatsApp",
        processes: &["whatsapp", "whatsapp.root"],
        ways: &[],
        twice: Twice::Never("WhatsApp starts without a window on the desk"),
        twins: &[Twin {
            app: "msedge",
            args: &["https://web.whatsapp.com"],
            passes: false,
        }],
    },
    App {
        names: &["ms-teams", "teams"],
        label: "Teams",
        processes: &["ms-teams"],
        ways: &[
            Way::Exe("ms-teams.exe"),
            Way::Packaged("MSTeams_8wekyb3d8bbwe!MSTeams"),
        ],
        twice: Twice::Fine,
        twins: &[],
    },
    App {
        names: &["spotify"],
        label: "Spotify",
        processes: &["spotify"],
        ways: &[
            Way::Exe("spotify.exe"),
            Way::Packaged("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
        ],
        twice: Twice::Fine,
        twins: &[],
    },
    App {
        names: &["code", "vscode"],
        label: "VS Code",
        processes: &["code"],
        ways: &[Way::Exe("code.exe")],
        twice: Twice::Fine,
        twins: &[],
    },
];

/// A name as the table spells it: lower case, without `.exe`.
fn plain(name: &str) -> String {
    let name = name.trim().to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// The app a name or a process name means, if the table knows it.
pub fn find(name: &str) -> Option<&'static App> {
    let name = plain(name);
    APPS.iter()
        .find(|a| a.names.contains(&name.as_str()) || a.processes.contains(&name.as_str()))
}

/// Whether a process is one of a known app, which is all `/apps` says of
/// the human's session.
pub fn known_process(process: &str) -> bool {
    let name = plain(process);
    APPS.iter().any(|a| a.processes.contains(&name.as_str()))
}

/// One thing to try starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The name to answer with.
    pub app: String,
    /// How to start it, the first that works. Empty for an app the
    /// table does not know: that one is looked up as `<app>.exe`.
    pub ways: &'static [Way],
    pub args: Vec<String>,
    /// The app asked for, when this one stands in for it.
    pub instead_of: Option<&'static str>,
    /// Why another app stands in, or what starting this one means for
    /// the human's own copy.
    pub why: Option<String>,
}

/// What to start for the app `asked` for, with `args`, while the human
/// has the processes `human` running: the choices in order, of which the
/// first installed one is started. An error is a refusal in plain words.
pub fn plan(asked: &str, args: &[String], human: &[String]) -> Result<Vec<Choice>, String> {
    let name = plain(asked);
    let simple = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ' ');
    if name.is_empty() || !name.chars().all(simple) {
        return Err(format!(
            "{asked:?} is not an app's name: give a name like msedge, or a path"
        ));
    }
    let Some(app) = find(&name) else {
        return Ok(vec![Choice {
            app: name,
            ways: &[],
            args: args.to_vec(),
            instead_of: None,
            why: None,
        }]);
    };
    let itself = |why: Option<&str>| {
        Ok(vec![Choice {
            app: app.names[0].to_string(),
            ways: app.ways,
            args: args.to_vec(),
            instead_of: None,
            why: why.map(str::to_string),
        }])
    };
    let open = human
        .iter()
        .any(|p| app.processes.contains(&plain(p).as_str()));
    let why = match app.twice {
        Twice::Fine => return itself(None),
        Twice::Clash { alone, .. } if !open => return itself(Some(alone)),
        Twice::Clash { open, .. } => {
            format!("{} is open in your own session and {open}", app.label)
        }
        Twice::Never(why) => why.to_string(),
    };
    if app.twins.is_empty() {
        return Err(format!("{why}, and nothing stands in for it"));
    }
    Ok(app
        .twins
        .iter()
        .map(|twin| {
            let mut all: Vec<String> = twin.args.iter().map(|a| a.to_string()).collect();
            if twin.passes {
                all.extend_from_slice(args);
            }
            Choice {
                app: twin.app.to_string(),
                ways: find(twin.app).map_or(&[], |a| a.ways),
                args: all,
                instead_of: Some(app.names[0]),
                why: Some(why.clone()),
            }
        })
        .collect())
}

/// Whether a `/launch` path is a web address. Those go to Edge by name:
/// handed to Windows they open in the default browser, which may be the
/// Chrome running at the human's.
pub fn is_web(path: &str) -> bool {
    let path = path.trim_start().to_ascii_lowercase();
    path.starts_with("http://") || path.starts_with("https://")
}

/// What `POST /launch` is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// An app by name.
    App { app: String, args: Vec<String> },
    /// A program, a document or a `shell:` target by its path.
    Path { path: String, args: Vec<String> },
}

/// The body of `POST /launch`, or why it is not one.
pub fn parse_launch(body: &str) -> Result<Launch, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Body {
        app: Option<String>,
        path: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    }
    let body: Body = serde_json::from_str(body).map_err(|e| e.to_string())?;
    // A NUL would end the command line early, with the rest unseen.
    if (body.args.iter().chain(&body.app).chain(&body.path)).any(|s| s.contains('\0')) {
        return Err("a NUL in a name, a path or an argument".into());
    }
    let args = body.args;
    match (body.app, body.path) {
        (Some(app), None) => Ok(Launch::App { app, args }),
        (None, Some(path)) if !path.trim().is_empty() => Ok(Launch::Path { path, args }),
        _ => Err(r#"give either "app" or "path""#.into()),
    }
}

/// What `POST /launch` answers once something started.
pub fn started_json(started: &str, instead_of: Option<&str>, why: Option<&str>) -> String {
    #[derive(Serialize)]
    struct Started<'a> {
        started: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        instead_of: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        why: Option<&'a str>,
    }
    let started = Started {
        started,
        instead_of,
        why,
    };
    serde_json::to_string(&started).unwrap_or_else(|_| "{}".into())
}

/// A window open on the desk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Open {
    /// Its process name, lower case, without `.exe`.
    pub app: String,
    pub title: String,
}

/// What `GET /apps` answers.
pub fn apps_json(desk: &[Open], human: &[String]) -> String {
    #[derive(Serialize)]
    struct Apps<'a> {
        desk: &'a [Open],
        human: &'a [String],
    }
    serde_json::to_string(&Apps { desk, human }).unwrap_or_else(|_| "{}".into())
}

/// A process name as `/apps` gives it, from a path or a file name.
pub fn process_name(path: &str) -> String {
    plain(path.rsplit(['\\', '/']).next().unwrap_or(path))
}

/// A program and its arguments as the one command line CreateProcessW
/// takes, quoted so the program's C runtime splits it back into the same
/// arguments.
pub fn command_line(exe: &str, args: &[String]) -> String {
    let mut line = format!("\"{exe}\"");
    for arg in args {
        line.push(' ');
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
            line.push_str(arg);
            continue;
        }
        line.push('"');
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                // Backslashes before a quote count double, and the quote
                // takes one more.
                '"' => {
                    line.extend(std::iter::repeat_n('\\', slashes + 1));
                    slashes = 0;
                }
                _ => slashes = 0,
            }
            line.push(c);
        }
        // Those before the closing quote count double too.
        line.extend(std::iter::repeat_n('\\', slashes));
        line.push('"');
    }
    line
}

/// A path as two of them are compared: lower case, backslashes only.
pub fn same_path(path: &str) -> String {
    path.trim().replace('/', "\\").to_lowercase()
}

/// `%NAME%` replaced by what `env` says it is, and left as written when
/// it says nothing.
pub fn expand(text: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => match env(&after[..end]) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                // Not a variable: the second sign may open one.
                None => {
                    out.push('%');
                    out.push_str(&after[..end]);
                    rest = &after[end..];
                }
            },
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A program a Run value starts at sign-in, by the paths its processes
/// have. Paths are as [`same_path`] makes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autostart {
    /// The program the value names.
    pub exe: String,
    /// The program that one starts and leaves running, as the folder it
    /// is somewhere under (ending in a backslash) and its file name: an
    /// app with a Squirrel updater is registered as `Update.exe
    /// --processStart App.exe` and lives in a folder per version.
    pub child: Option<(String, String)>,
}

impl Autostart {
    /// Whether the process with this executable is the one started.
    pub fn is(&self, process: &str) -> bool {
        let process = same_path(process);
        process == self.exe
            || self.child.as_ref().is_some_and(|(folder, name)| {
                (process.strip_prefix(folder.as_str()))
                    .is_some_and(|rest| rest.rsplit('\\').next() == Some(name.as_str()))
            })
    }
}

/// The program a Run value starts, from its command line: quoted or not,
/// with arguments, with `%VARIABLES%`. None when it names no `.exe` by
/// its full path, which no process can be matched to.
pub fn autostart(value: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<Autostart> {
    let value = expand(value.trim(), env);
    let (exe, rest) = match value.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?,
        None => {
            // Unquoted paths with spaces in them are common here, so the
            // program ends at the first `.exe` that ends a word.
            let lower = value.to_ascii_lowercase();
            let end = lower
                .match_indices(".exe")
                .map(|(i, _)| i + 4)
                .find(|end| {
                    (lower[*end..].chars().next()).is_none_or(|c| c.is_whitespace())
                })?;
            value.split_at(end)
        }
    };
    let exe = same_path(exe);
    let full = exe.ends_with(".exe") && (exe.contains(":\\") || exe.starts_with("\\\\"));
    if !full {
        return None;
    }
    let mut words = rest.split_whitespace();
    let child = words
        .find(|w| w.eq_ignore_ascii_case("--processStart"))
        .and_then(|_| words.next())
        .map(|name| same_path(name.trim_matches('"')))
        .filter(|name| !name.is_empty() && !name.contains('\\'))
        .and_then(|name| {
            let folder = &exe[..=exe.rfind('\\')?];
            Some((folder.to_string(), name))
        });
    Some(Autostart { exe, child })
}

/// Whether a process is never stopped whatever the Run keys name: the
/// shell, and all that lives in the Windows folder (the security tray,
/// drivers' helpers, `cmd` and `rundll32` that a value starts something
/// through).
pub fn spared(process: &str, windows: &str) -> bool {
    let process = same_path(process);
    let windows = same_path(windows);
    let windows = windows.trim_end_matches('\\');
    let inside = !windows.is_empty()
        && (process.strip_prefix(windows)).is_some_and(|rest| rest.starts_with('\\'));
    inside || process.rsplit('\\').next() == Some("explorer.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn first(asked: &str, open: &[&str]) -> Choice {
        plan(asked, &[], &human(open)).unwrap().remove(0)
    }

    #[test]
    fn every_twin_is_known_and_runs_twice() {
        for app in APPS {
            assert_eq!(find(app.names[0]), Some(app), "{}", app.label);
            for twin in app.twins {
                let stand_in = find(twin.app).unwrap_or_else(|| panic!("{}", twin.app));
                assert_eq!(stand_in.names[0], twin.app);
                assert_eq!(stand_in.twice, Twice::Fine, "{}", twin.app);
                assert!(!stand_in.ways.is_empty(), "{}", twin.app);
            }
            if app.twice == Twice::Fine {
                assert!(!app.ways.is_empty(), "{}", app.label);
            }
        }
    }

    #[test]
    fn chrome_open_at_the_humans_becomes_edge() {
        let choice = first("chrome", &["outlook", "chrome"]);
        assert_eq!(choice.app, "msedge");
        assert_eq!(choice.ways, [Way::Exe("msedge.exe")]);
        assert_eq!(choice.instead_of, Some("chrome"));
        assert_eq!(
            choice.why.as_deref(),
            Some("Chrome is open in your own session and would open its window there")
        );
        // However it is spelled, on either side.
        assert_eq!(first("Chrome.exe", &["CHROME.EXE"]).app, "msedge");
    }

    #[test]
    fn chrome_alone_starts_and_says_what_follows() {
        let choice = first("chrome", &["msedge"]);
        assert_eq!(choice.app, "chrome");
        assert_eq!(choice.instead_of, None);
        assert!(choice.why.unwrap().contains("opens its window on the desk"));
    }

    #[test]
    fn what_was_asked_to_open_goes_to_a_browser_twin_only() {
        let url = vec!["https://example.com".to_string()];
        let edge = plan("chrome", &url, &human(&["chrome"])).unwrap();
        assert_eq!(edge[0].args, url);
        let mail = plan("outlook", &url, &human(&["outlook"])).unwrap();
        assert!(mail[0].args.is_empty());
        assert_eq!(mail[1].args, ["https://outlook.office.com"]);
        assert_eq!(plan("chrome", &url, &[]).unwrap()[0].args, url);
    }

    #[test]
    fn classic_outlook_becomes_the_new_one_then_the_web() {
        let choices = plan("outlook", &[], &human(&["outlook"])).unwrap();
        let names: Vec<&str> = choices.iter().map(|c| c.app.as_str()).collect();
        assert_eq!(names, ["olk", "msedge"]);
        assert!(matches!(
            choices[0].ways[0],
            Way::Packaged(id) if id.starts_with("Microsoft.OutlookForWindows_")
        ));
        for choice in &choices {
            assert_eq!(choice.instead_of, Some("outlook"));
            assert!(choice.why.as_ref().unwrap().contains("data file"));
        }
        // The new Outlook open at the human's is not the classic one.
        assert_eq!(first("outlook", &["olk"]).app, "outlook");
    }

    #[test]
    fn whatsapp_is_always_the_web_one() {
        for open in [&[][..], &["whatsapp"][..], &["whatsapp.root"][..]] {
            let choice = first("whatsapp", open);
            assert_eq!(choice.app, "msedge");
            assert_eq!(choice.args, ["https://web.whatsapp.com"]);
            assert_eq!(choice.instead_of, Some("whatsapp"));
            assert_eq!(
                choice.why.as_deref(),
                Some("WhatsApp starts without a window on the desk")
            );
        }
    }

    #[test]
    fn what_runs_twice_starts_as_itself() {
        let all = ["msedge", "olk", "ms-teams", "spotify", "code", "chrome"];
        for (asked, name) in [
            ("msedge", "msedge"),
            ("edge", "msedge"),
            ("olk", "olk"),
            ("teams", "ms-teams"),
            ("spotify", "spotify"),
            ("vscode", "code"),
        ] {
            let choice = first(asked, &all);
            assert_eq!(choice.app, name);
            assert_eq!((choice.instead_of, choice.why), (None, None), "{asked}");
        }
    }

    #[test]
    fn an_unknown_name_is_looked_up_as_itself() {
        let choice = first("Notepad.exe", &["chrome"]);
        assert_eq!(choice.app, "notepad");
        assert!(choice.ways.is_empty());
        assert_eq!((choice.instead_of, choice.why), (None, None));
        for bad in ["", "  ", "..\\x", "C:\\x.exe", "a/b", "a\"b", "a%b%"] {
            assert!(plan(bad, &[], &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_known_processes_are_told_of_the_human() {
        assert!(known_process("chrome"));
        assert!(known_process("OUTLOOK.EXE"));
        assert!(known_process("WhatsApp.Root.exe"));
        assert!(!known_process("edge"));
        assert!(!known_process("keepass"));
        assert_eq!(process_name("C:\\Program Files\\X\\MsEdge.EXE"), "msedge");
        assert_eq!(process_name("ms-teams.exe"), "ms-teams");
    }

    #[test]
    fn a_launch_is_an_app_or_a_path() {
        assert_eq!(
            parse_launch(r#"{"app":"chrome"}"#),
            Ok(Launch::App {
                app: "chrome".into(),
                args: vec![]
            })
        );
        assert_eq!(
            parse_launch(r#"{"path":"C:\\a b\\x.exe","args":["-n","1"]}"#),
            Ok(Launch::Path {
                path: "C:\\a b\\x.exe".into(),
                args: vec!["-n".into(), "1".into()]
            })
        );
        for bad in [
            "",
            "{}",
            r#"{"app":"a","path":"b"}"#,
            r#"{"path":" "}"#,
            r#"{"app":"a","more":1}"#,
            r#"{"path":"x","args":"y"}"#,
            r#"{"path":"x\u0000y"}"#,
            r#"{"path":"x","args":["a\u0000"]}"#,
        ] {
            assert!(parse_launch(bad).is_err(), "{bad}");
        }
        assert!(is_web("https://web.whatsapp.com"));
        assert!(is_web(" HTTP://x"));
        assert!(!is_web("shell:AppsFolder\\x"));
        assert!(!is_web("C:\\http://x"));
    }

    #[test]
    fn the_answers_read_as_the_plan_says() {
        assert_eq!(
            started_json("msedge", Some("chrome"), Some("because")),
            r#"{"started":"msedge","instead_of":"chrome","why":"because"}"#
        );
        assert_eq!(started_json("code", None, None), r#"{"started":"code"}"#);
        let desk = [Open {
            app: "msedge".into(),
            title: "A \"page\"".into(),
        }];
        assert_eq!(
            apps_json(&desk, &human(&["chrome", "outlook"])),
            r#"{"desk":[{"app":"msedge","title":"A \"page\""}],"human":["chrome","outlook"]}"#
        );
        assert_eq!(apps_json(&[], &[]), r#"{"desk":[],"human":[]}"#);
    }

    #[test]
    fn a_command_line_splits_back_into_its_arguments() {
        let line = |args: &[&str]| command_line("C:\\a b\\x.exe", &human(args));
        assert_eq!(line(&[]), r#""C:\a b\x.exe""#);
        assert_eq!(
            line(&["-n", "https://x/?a=1&b=2"]),
            r#""C:\a b\x.exe" -n https://x/?a=1&b=2"#
        );
        assert_eq!(line(&["a b", ""]), r#""C:\a b\x.exe" "a b" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""C:\a b\x.exe" "say \"hi\"""#);
        assert_eq!(
            line(&[r"C:\dir with space\"]),
            r#""C:\a b\x.exe" "C:\dir with space\\""#
        );
        assert_eq!(line(&[r#"a\\"b"#]), r#""C:\a b\x.exe" "a\\\\\"b""#);
        assert_eq!(line(&[r"C:\plain\"]), r#""C:\a b\x.exe" C:\plain\"#);
    }

    fn env(name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "localappdata" => Some("C:\\Users\\me\\AppData\\Local".into()),
            "windir" => Some("C:\\WINDOWS".into()),
            _ => None,
        }
    }

    fn exe(value: &str) -> Option<String> {
        autostart(value, &env).map(|a| a.exe)
    }

    #[test]
    fn variables_expand_and_the_rest_stays() {
        assert_eq!(expand("%windir%\\x", &env), "C:\\WINDOWS\\x");
        assert_eq!(expand("a %NOPE% b", &env), "a %NOPE% b");
        assert_eq!(
            expand("100% %LocalAppData%", &env),
            "100% C:\\Users\\me\\AppData\\Local"
        );
        assert_eq!(expand("%", &env), "%");
        assert_eq!(expand("", &env), "");
    }

    #[test]
    fn a_run_value_names_its_program() {
        assert_eq!(
            exe(r#""C:\Program Files (x86)\Steam\steam.exe" -silent"#).as_deref(),
            Some(r"c:\program files (x86)\steam\steam.exe")
        );
        // Unquoted, with spaces in the path and arguments after it.
        assert_eq!(
            exe(r"C:\Riot Games\Riot Client\RiotClientServices.exe --launch-background-mode")
                .as_deref(),
            Some(r"c:\riot games\riot client\riotclientservices.exe")
        );
        assert_eq!(
            exe(r"C:\Program Files\Kiosk\Kiosk.exe").as_deref(),
            Some(r"c:\program files\kiosk\kiosk.exe")
        );
        assert_eq!(
            exe(r"%LOCALAPPDATA%\Programs\Notes\Notes.EXE --open-at-login").as_deref(),
            Some(r"c:\users\me\appdata\local\programs\notes\notes.exe")
        );
        assert_eq!(
            exe(r"%windir%\system32\SecurityHealthSystray.exe").as_deref(),
            Some(r"c:\windows\system32\securityhealthsystray.exe")
        );
        // A folder named like a program does not end the path early.
        assert_eq!(
            exe(r"C:\tools.exe.d\run me.exe /x").as_deref(),
            Some(r"c:\tools.exe.d\run me.exe")
        );
        for none in [
            "",
            "rundll32.exe shell32.dll,Control_RunDLL",
            r"C:\scripts\start.cmd",
            r#""C:\scripts\start.bat" x.exe"#,
            r#""C:\unclosed\x.exe"#,
            r"%NOPE%\x.exe",
        ] {
            assert_eq!(exe(none), None, "{none}");
        }
    }

    #[test]
    fn an_updater_names_the_app_it_starts() {
        let discord = autostart(
            r#""%LocalAppData%\Discord\Update.exe" --processStart Discord.exe"#,
            &env,
        )
        .unwrap();
        assert_eq!(discord.exe, r"c:\users\me\appdata\local\discord\update.exe");
        assert!(discord.is(r"C:\Users\me\AppData\Local\Discord\Update.exe"));
        assert!(discord.is(r"C:\Users\me\AppData\Local\Discord\app-1.0.9/Discord.exe"));
        assert!(!discord.is(r"C:\Users\me\AppData\Local\Discord\app-1.0.9\Other.exe"));
        assert!(!discord.is(r"C:\Users\me\AppData\Local\Other\Discord.exe"));
        assert!(!discord.is(r"C:\Users\me\AppData\Local\DiscordPTB\app\Discord.exe"));

        let steam = autostart(r#""C:\Steam\steam.exe" -silent"#, &env).unwrap();
        assert_eq!(steam.child, None);
        assert!(steam.is(r"c:\steam\STEAM.exe"));
        assert!(!steam.is(r"c:\steam\bin\steamwebhelper.exe"));
        // A name with a path in it is not taken for a file name.
        let odd = autostart(r#""C:\a\Update.exe" --processStart ..\b.exe"#, &env).unwrap();
        assert_eq!(odd.child, None);
    }

    #[test]
    fn the_shell_and_the_windows_folder_are_spared() {
        let windows = "C:\\WINDOWS\\";
        assert!(spared(r"C:\Windows\explorer.exe", windows));
        assert!(spared(
            r"c:\windows\System32\SecurityHealthSystray.exe",
            windows
        ));
        assert!(spared(r"D:\odd\Explorer.EXE", windows));
        assert!(!spared(r"C:\WindowsApps\x.exe", windows));
        assert!(!spared(r"C:\Program Files\Steam\steam.exe", windows));
        assert!(!spared(r"C:\Program Files\Steam\steam.exe", ""));
    }
}
