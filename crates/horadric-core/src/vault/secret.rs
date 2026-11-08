//! Secrets used, never read: whose a secret is, which one wins, the mask
//! over a program's output, which programs are only ever offered Once, and
//! the log line. See docs/plan/vault.md.

use serde::{Deserialize, Serialize};

/// Whose a secret or a web login is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owner {
    /// A project, by its folder key.
    Project(String),
    /// An identity, by its slug.
    Identity(String),
    Everywhere,
}

impl Owner {
    /// How a person or the CLI writes it: `project`, `identity:<slug>` or
    /// `everywhere`.
    pub fn label(&self) -> String {
        match self {
            Owner::Project(_) => "project".into(),
            Owner::Identity(slug) => format!("identity:{slug}"),
            Owner::Everywhere => "everywhere".into(),
        }
    }
}

/// An environment variable's name as a secret may have it: upper case,
/// digits and `_`, a letter first.
pub fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// The item kept under `name` that a session of `project` with `identity`
/// gets: the project's own, else its identity's, else an Everywhere one.
pub fn winner<'a, T>(
    items: &'a [T],
    owner_of: impl Fn(&T) -> &Owner,
    name_of: impl Fn(&T) -> &str,
    project: &str,
    identity: Option<&str>,
    name: &str,
) -> Option<&'a T> {
    let rank = |o: &Owner| match o {
        Owner::Project(p) if p == project => Some(0),
        Owner::Identity(s) if Some(s.as_str()) == identity => Some(1),
        Owner::Everywhere => Some(2),
        _ => None,
    };
    items
        .iter()
        .filter(|t| name_of(t) == name)
        .filter_map(|t| rank(owner_of(t)).map(|r| (r, t)))
        .min_by_key(|(r, _)| *r)
        .map(|(_, t)| t)
}

/// Writes `[NAME]` wherever a value would have shown in a stream of bytes.
/// A value split over two chunks is still caught: a tail that could be the
/// start of one is held back until the next chunk says whether it is.
pub struct Mask {
    /// Longest first, so a value inside another never leaves the rest of
    /// the longer one showing.
    values: Vec<(Vec<u8>, Vec<u8>)>,
    held: Vec<u8>,
}

impl Mask {
    pub fn new(values: Vec<(String, String)>) -> Mask {
        let mut values: Vec<(Vec<u8>, Vec<u8>)> = values
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(n, v)| (v.into_bytes(), format!("[{n}]").into_bytes()))
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.0.len()));
        Mask {
            values,
            held: Vec::new(),
        }
    }

    /// What of the stream so far can be passed on.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(chunk);
        self.run(false)
    }

    /// The held tail, masked, once the stream has ended.
    pub fn finish(&mut self) -> Vec<u8> {
        self.run(true)
    }

    fn run(&mut self, last: bool) -> Vec<u8> {
        let buf = std::mem::take(&mut self.held);
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        'scan: while i < buf.len() {
            let rest = &buf[i..];
            for (value, label) in &self.values {
                if rest.starts_with(value) {
                    out.extend_from_slice(label);
                    i += value.len();
                    continue 'scan;
                }
                if !last && rest.len() < value.len() && value.starts_with(rest) {
                    self.held = rest.to_vec();
                    return out;
                }
            }
            out.push(buf[i]);
            i += 1;
        }
        out
    }
}

/// The programs that may be allowed Always, each a single-purpose CLI
/// whose ways to run other code are known: what it reads is pinned
/// ([`pinned`]) and what its arguments can make it do is refused
/// ([`always_refused`]). Everything else is Once only. A list of what may
/// not would never be complete: an alias of a CLI, a plugin it loads from
/// a folder of its own, a hook in the project it deploys.
const ALWAYS: &[&str] = &["aws", "az", "gcloud", "stripe"];

/// Whether the program at this path may be allowed Always. It must be a
/// program file of its own: a batch file or script on the list's name is
/// not the program, and an npm shim is judged by its package, which is
/// never on the list.
pub fn always_allowed(program_path: &str) -> bool {
    let file = program_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program_path)
        .to_ascii_lowercase();
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    matches!(ext, "" | "exe") && ALWAYS.contains(&stem_of(program_path).as_str())
}

/// How a pin is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinKind {
    /// One file's content.
    File,
    /// A config file's content, which must read as UTF-8: one that does
    /// not may be read by the program otherwise than it reads here.
    Text,
    /// Every folder and file under it.
    Tree,
    /// A folder that must be missing or empty: anything in it is code the
    /// program would run, put there before any question could show it.
    Empty,
    /// Only the names of what is at its top, for a folder whose new
    /// entries matter but whose content another pin covers or churns:
    /// the global `node_modules` a package can load a planted one from.
    Names,
}

/// A file or folder whose content an Always is bound to, because changing
/// it could make the program run other code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// How the question names it: `the aws config`. Never holds `,`, `=`
    /// or `|`, which [`fingerprint`] uses.
    pub what: String,
    pub path: String,
    pub kind: PinKind,
}

/// The cloud CLIs that may have Always once what could make them run
/// other code is pinned: their aliases, plugins and exec hook files. Some
/// for the program at this path, the files and folders to pin as `env`
/// (the environment the child will get, an identity's own folders
/// included) places them; None for any other program. What their
/// arguments can make them do is [`always_refused`]'s.
pub fn pinned(program_path: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<Vec<Pin>> {
    let stem = stem_of(program_path);
    let var = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let home = || var("USERPROFILE");
    let pin = |what: &str, kind: PinKind, path: Option<String>| {
        path.map(|path| Pin {
            what: what.to_string(),
            path,
            kind,
        })
    };
    let pins: Vec<Option<Pin>> = match stem.as_str() {
        "aws" => vec![
            pin(
                "the aws config",
                PinKind::Text,
                var("AWS_CONFIG_FILE").or_else(|| home().map(|h| join(&h, r".aws\config"))),
            ),
            // `credential_process` runs a command from here too.
            pin(
                "the aws credentials file",
                PinKind::Text,
                var("AWS_SHARED_CREDENTIALS_FILE")
                    .or_else(|| home().map(|h| join(&h, r".aws\credentials"))),
            ),
            // Aliases run `!` shell commands. The folder's own content is
            // the credential cache, which churns, so only its names count.
            pin(
                "the aws aliases",
                PinKind::Text,
                home().map(|h| join(&h, r".aws\cli\alias")),
            ),
            pin(
                "the aws cli folder",
                PinKind::Names,
                home().map(|h| join(&h, r".aws\cli")),
            ),
            // botocore loads service models, endpoint rules included, from
            // here before its own.
            pin(
                "the aws models folder",
                PinKind::Empty,
                home().map(|h| join(&h, r".aws\models")),
            ),
        ],
        "gcloud" => {
            let dir = var("CLOUDSDK_CONFIG").or_else(|| var("APPDATA").map(|a| join(&a, "gcloud")));
            vec![
                pin(
                    "gcloud's properties",
                    PinKind::Text,
                    dir.as_ref().map(|d| join(d, "properties")),
                ),
                pin(
                    "gcloud's active_config",
                    PinKind::Text,
                    dir.as_ref().map(|d| join(d, "active_config")),
                ),
                pin(
                    "gcloud's configurations",
                    PinKind::Tree,
                    dir.map(|d| join(&d, "configurations")),
                ),
            ]
        }
        "az" => {
            let dir = var("AZURE_CONFIG_DIR").or_else(|| home().map(|h| join(&h, ".azure")));
            vec![
                pin(
                    "az's config",
                    PinKind::Text,
                    dir.as_ref().map(|d| join(d, "config")),
                ),
                pin(
                    "az's extensions",
                    PinKind::Tree,
                    var("AZURE_EXTENSION_DIR").or_else(|| dir.map(|d| join(&d, "cliextensions"))),
                ),
            ]
        }
        "stripe" => {
            let dir = var("XDG_CONFIG_HOME")
                .map(|x| join(&x, "stripe"))
                .or_else(|| home().map(|h| join(&h, r".config\stripe")));
            vec![
                pin(
                    "stripe's config.toml",
                    PinKind::Text,
                    dir.as_ref().map(|d| join(d, "config.toml")),
                ),
                // A plugin runs as `stripe <name>`, code of anyone's.
                pin(
                    "stripe's plugins",
                    PinKind::Empty,
                    dir.map(|d| join(&d, "plugins")),
                ),
            ]
        }
        _ => return None,
    };
    Some(pins.into_iter().flatten().collect())
}

/// What an Always is for beside the program: the leading words of the
/// command line that read as subcommands, `s3 ls` of `aws s3 ls
/// s3://bucket`, so allowing one does not allow another. Words stop at
/// the first flag or the first that is not a plain name (a number, a
/// path, a URL), and at three.
pub fn command_path(args: &[String]) -> String {
    args.iter()
        .take_while(|a| {
            let mut c = a.chars();
            c.next().is_some_and(|f| f.is_ascii_lowercase())
                && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        .take(3)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a pinned CLI's arguments can make it do that no pin covers, for
/// the question's "Only Once is offered: ...". Each has Always only for a
/// run that names a subcommand first, with no flag before it, so an
/// Always never covers a whole CLI.
struct Rules {
    stems: &'static [&'static str],
    /// Flags refused whatever their value.
    flags: &'static [&'static str],
    /// Flags refused when their value names a file to read.
    file_flags: &'static [&'static str],
    /// Command paths refused, and every path under them: those that
    /// install or fetch code, run git, or change the CLI's own config.
    commands: &'static [&'static str],
    /// Reads arguments from `@file`, which the agent can write.
    response_files: bool,
}

const RULES: &[Rules] = &[
    Rules {
        stems: &["aws"],
        // Another endpoint, or no TLS check, sends a bearer token such as
        // AWS_BEARER_TOKEN_BEDROCK wherever the flag says.
        flags: &["--endpoint-url", "--ca-bundle", "--no-verify-ssl"],
        file_flags: &["--cli-input-json", "--cli-input-yaml"],
        // `codeartifact login` runs npm, pip or dotnet to set them up.
        commands: &["configure", "codeartifact login"],
        response_files: false,
    },
    Rules {
        stems: &["gcloud"],
        flags: &["--flags-file"],
        file_flags: &[],
        // `source` runs git; `components` installs code.
        commands: &["source", "components"],
        response_files: false,
    },
    Rules {
        stems: &["az"],
        flags: &[],
        file_flags: &[],
        // Each installs or fetches code that runs as az, or starts git or
        // docker: `repos` checks out with git, `acr login` runs docker.
        commands: &[
            "extension",
            "bicep",
            "upgrade",
            "repos",
            "devops",
            "acr login",
        ],
        response_files: true,
    },
    Rules {
        stems: &["stripe"],
        // The `-base` flags send the key to another host; `--config`
        // reads another config; `--raw` and `--edit` give a fixture,
        // which names its own API host.
        flags: &[
            "--api-base",
            "--connect-base",
            "--dashboard-base",
            "--files-base",
            "--config",
            "--raw",
            "--edit",
        ],
        file_flags: &[],
        // `plugin` installs code, `samples` runs git, `login`, `logout`
        // and `config` change what the pins hold; `fixtures` and
        // `trigger` run fixtures, whose `api_base` is not checked.
        commands: &[
            "plugin", "samples", "login", "logout", "config", "fixtures", "trigger",
        ],
        response_files: false,
    },
];

/// Why the arguments leave only Once for the program `stem` names, None
/// when they do not.
pub fn always_refused(stem: &str, args: &[String]) -> Option<String> {
    let stem = stem_of(stem);
    let rules = RULES.iter().find(|r| r.stems.contains(&stem.as_str()))?;
    if let [only] = args {
        if matches!(only.as_str(), "--version" | "--help" | "-h") {
            return None;
        }
    }
    if args.first().is_some_and(|a| a.starts_with('-')) {
        return Some(format!(
            "a flag comes before {stem}'s subcommand, so what it runs is not plain"
        ));
    }
    let path = command_path(args);
    if path.is_empty() {
        return Some(format!(
            "it names no {stem} subcommand, and Always would cover all of them"
        ));
    }
    let under = |c: &str| path == c || path.starts_with(&format!("{c} "));
    if let Some(c) = rules.commands.iter().find(|c| under(c)) {
        return Some(format!(
            "`{stem} {c}` installs code or changes what {stem} runs"
        ));
    }
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = args.get(i + 1).map(String::as_str);
        i += 1;
        // Everything after `--` is an operand, never a flag.
        if a == "--" {
            break;
        }
        if rules.response_files && a.starts_with('@') {
            return Some(format!(
                "`{a}` reads more arguments from a file the agent can write"
            ));
        }
        let Some(long) = a.strip_prefix("--") else {
            continue;
        };
        let (name, inline) = match long.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (long, None),
        };
        let flag = format!("--{name}");
        if rules.flags.contains(&flag.as_str()) {
            return Some(format!(
                "`{flag}` can make {stem} read or run more than its command"
            ));
        }
        let value = inline.or(next);
        if rules.file_flags.contains(&flag.as_str())
            && value.is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.starts_with("file://") || v.starts_with("fileb://")
            })
        {
            return Some(format!(
                "`{flag}` reads its input from a file the agent can write"
            ));
        }
    }
    None
}

/// The CLIs that run git, which a pin cannot hold: its config, hooks,
/// filters, drivers and helpers are too many ways to run a program with
/// the secret in reach. A token for them is asked for Once each time;
/// unattended, they use the identity's own login instead.
const RUN_GIT: &[&str] = &["gh", "vercel", "netlify", "netlify-cli", "wrangler"];

/// Why the program is only ever offered Once, None when it may have
/// Always. `name` is the program's path, or for an npm shim its package's
/// name, never the shim's own file name, which can be any alias. For a
/// CLI that runs git, the reason says how to use it unattended.
pub fn once_reason(name: &str, npm: bool) -> Option<String> {
    let stem = stem_of(name);
    if RUN_GIT.contains(&stem.as_str()) {
        let then = match login_tool(name) {
            Some(tool) => format!(
                "; for unattended use, log {tool} in for this project's identity in Settings"
            ),
            None => String::new(),
        };
        return Some(format!(
            "{stem} runs git, which can run other programs with the secret{then}"
        ));
    }
    (npm || !always_allowed(name))
        .then(|| "only programs Horadric knows can be allowed always".to_string())
}

/// The identity login that stands in for a secret for this program, as
/// `--on-login` names it: gh and vercel have their own.
pub fn login_tool(name: &str) -> Option<&'static str> {
    match stem_of(name).as_str() {
        "gh" => Some("gh"),
        "vercel" => Some("vercel"),
        _ => None,
    }
}

/// The program an Always in a secret's `allowed` list is for, from the
/// part [`approval`] wrote: its path, without the subcommand and input
/// after it. The path ends at its program file's extension.
pub fn allowed_program(bound: &str) -> &str {
    let lower = bound.to_ascii_lowercase();
    for ext in [".exe", ".com", ".cmd", ".bat"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(ext) {
            let end = from + at + ext.len();
            if end == bound.len() || bound[end..].starts_with(' ') {
                return &bound[..end];
            }
            from = end;
        }
    }
    bound.split(' ').next().unwrap_or(bound)
}

/// Whether the program deploys by running the project's own build
/// (`wrangler.toml`'s `[build]`, `package.json` scripts), which lives in
/// the project and changes with every commit, so no pin can hold it. The
/// question says so, and the human answers knowing it.
pub fn runs_builds(program_path: &str) -> bool {
    matches!(
        stem_of(program_path).as_str(),
        "vercel" | "wrangler" | "netlify"
    )
}

fn stem_of(program_path: &str) -> String {
    let file = program_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program_path)
        .to_ascii_lowercase();
    match file.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s.to_string(),
        _ => file,
    }
}

fn join(base: &str, rel: &str) -> String {
    format!("{}\\{rel}", base.trim_end_matches(['\\', '/']))
}

/// What an npm `.cmd` shim runs: the package folder and the script in
/// it, both relative to the shim's folder. Horadric runs that script with
/// node itself rather than the shim through cmd, so the shim's text is
/// only read, never trusted to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shim {
    /// `node_modules\vercel` or `node_modules\@scope\name`.
    pub package: String,
    /// `node_modules\vercel\dist\index.js`.
    pub script: String,
}

/// Reads an npm cmd-shim, None for any other `.cmd`. Only the shape npm
/// writes for a node script with no extra arguments counts; a shim for
/// another interpreter, or with arguments of its own, stays Once only.
pub fn npm_shim(text: &str) -> Option<Shim> {
    if !text.contains(r#""%dp0%\node.exe""#) {
        return None;
    }
    let line = text.lines().rev().find(|l| l.contains(r#""%_prog%""#))?;
    let (_, after) = line.split_once(r#""%_prog%""#)?;
    let after = after.trim_start();
    let rest = after.strip_prefix(r#""%dp0%\"#)?;
    let (script, tail) = rest.split_once('"')?;
    if tail.trim() != "%*" {
        return None;
    }
    let parts: Vec<&str> = script.split('\\').collect();
    let clean = parts.iter().all(|p| {
        !p.is_empty()
            && *p != "."
            && *p != ".."
            && !p.contains(['%', '"', '/', ':', '^', '&', '|', '<', '>', '!'])
    });
    if !clean || parts.first() != Some(&"node_modules") {
        return None;
    }
    let depth = if parts.get(1).is_some_and(|p| p.starts_with('@')) {
        3
    } else {
        2
    };
    if parts.len() <= depth {
        return None;
    }
    Some(Shim {
        package: parts[..depth].join("\\"),
        script: script.to_string(),
    })
}

/// The content an Always holds to: the program's SHA-256, then each pin's
/// as `what=hash`, so the question can say which one changed.
pub fn fingerprint(program_sha256: &str, pins: &[(String, String)]) -> String {
    let mut out = program_sha256.to_ascii_lowercase();
    for (what, hash) in pins {
        out.push_str(&format!(",{what}={hash}"));
    }
    out
}

/// What changed since an Always for this project and program, when the
/// program is the same and only pinned things differ: their names, for
/// the question. Empty when nothing was allowed or the program itself
/// changed.
pub fn changed_pins(
    allowed: &[String],
    project: &str,
    program: &str,
    fingerprint: &str,
) -> Vec<String> {
    let tail = format!("|{}|{}", project, program.to_ascii_lowercase());
    let now = fingerprint.to_ascii_lowercase();
    let mut now = now.split(',');
    let program_hash = now.next().unwrap_or_default();
    let now: Vec<(&str, &str)> = now.filter_map(|p| p.split_once('=')).collect();
    let Some(was) = allowed
        .iter()
        .filter_map(|a| a.strip_suffix(&tail)?.strip_prefix("sha256:"))
        .find(|f| f.split(',').next() == Some(program_hash))
    else {
        return Vec::new();
    };
    let was: Vec<(&str, &str)> = was
        .split(',')
        .skip(1)
        .filter_map(|p| p.split_once('='))
        .collect();
    now.iter()
        .filter(|(what, hash)| !was.iter().any(|(w, h)| w == what && h == hash))
        .map(|(what, _)| what.to_string())
        .chain(
            was.iter()
                .filter(|(what, _)| !now.iter().any(|(w, _)| w == what))
                .map(|(what, _)| what.to_string()),
        )
        .collect()
}

/// An Always as kept in a secret's `allowed` list: for one project, one
/// program by its canonical path, and that program's content by SHA-256.
/// The path comes last, so a list that shows an entry by its last part
/// shows the program's file name.
pub fn approval(project: &str, program: &str, sha256: &str) -> String {
    format!(
        "sha256:{}|{}|{}",
        sha256.to_ascii_lowercase(),
        project,
        program.to_ascii_lowercase()
    )
}

/// Whether `allowed` holds an Always for this project, program and
/// content. Another project's Always, or one for the program before it
/// changed, is not one: the human is asked again.
pub fn approved(allowed: &[String], project: &str, program: &str, sha256: &str) -> bool {
    let want = approval(project, program, sha256);
    allowed.contains(&want)
}

/// One line of `secrets.log`: when, which project and session, which
/// secret, which program, and what came of it. Never the value and never
/// the command line, which may hold other things worth keeping out.
pub fn log_line(
    at: u64,
    project: &str,
    session: &str,
    name: &str,
    program: &str,
    outcome: &str,
) -> String {
    let clean = |s: &str| s.replace(['\t', '\r', '\n'], " ");
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        utc(at),
        clean(project),
        clean(session),
        clean(name),
        clean(program),
        clean(outcome)
    )
}

/// Unix seconds as `2026-11-06 12:34:56Z`.
fn utc(at: u64) -> String {
    let days = (at / 86_400) as i64;
    let secs = at % 86_400;
    // Howard Hinnant's days to civil date.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// The lines a session's system prompt gets about the secrets and web
/// logins it may use: their names, never a value, and how to use them.
pub fn prompt_lines(names: &[String], logins: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if !names.is_empty() {
        lines.push(format!(
            "Secrets Horadric keeps for this project, by name: {}. Use one without reading it: \
             `horadric secret run NAME[,NAME] -- program args...` starts the program with the values \
             in its environment and masks them in its output; `--stdin NAME` also writes that value \
             to the program's input.",
            names.join(", ")
        ));
    }
    if !logins.is_empty() {
        lines.push(format!(
            "Web logins Horadric keeps for this project, by name: {}. Sign in with the browser tool \
             `browser_fill_login`, giving the login's name and the refs of the fields from \
             `browser_snapshot`; you never see the password.",
            logins.join(", ")
        ));
    }
    if !lines.is_empty() {
        lines.push(
            "Never print, write, commit or pass a secret on in any other way, and never try to \
             read one. When one is missing or refused, ask the human to add or allow it in Project \
             settings, or block the quest with `horadric quest blocked --on-secret NAME`."
                .into(),
        );
    }
    lines
}

/// Text from the agent as the human's question shows it: every control,
/// format, direction and zero-width character written out as `\u{202e}`
/// (`\n`, `\r` and `\t` as themselves), so a line break, a right-to-left
/// override or an invisible character cannot make the command read as
/// another.
pub fn plainly(text: &str) -> String {
    let hidden = |c: char| {
        c.is_control()
            || matches!(
                c as u32,
                0x00AD
                    | 0x034F
                    | 0x061C
                    | 0x115F..=0x1160
                    | 0x17B4..=0x17B5
                    | 0x180B..=0x180F
                    | 0x200B..=0x200F
                    | 0x2028..=0x202E
                    | 0x2060..=0x206F
                    | 0x3164
                    | 0xFE00..=0xFE0F
                    | 0xFEFF
                    | 0xFFA0
                    | 0xFFF9..=0xFFFB
                    | 0x1D173..=0x1D17A
                    | 0xE0000..=0xE0FFF
            )
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if hidden(c) => out.push_str(&format!(r"\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The line a `secret run` fails with when `name` is not kept.
pub fn missing_line(name: &str) -> String {
    format!(
        "{name} is not kept for this project. Ask the human to add it in Project settings, \
         or block the quest with --on-secret {name}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_environment_variables_in_upper_case() {
        assert!(valid_name("GITHUB_TOKEN"));
        assert!(valid_name("A"));
        assert!(valid_name("K8S_CONFIG_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("github_token"));
        assert!(!valid_name("_TOKEN"));
        assert!(!valid_name("1TOKEN"));
        assert!(!valid_name("MY-TOKEN"));
        assert!(!valid_name("MY TOKEN"));
        assert!(!valid_name(&"A".repeat(65)));
        assert!(valid_name(&"A".repeat(64)));
    }

    struct Item(Owner, &'static str, u8);

    fn pick(items: &[Item], project: &str, id: Option<&str>, name: &str) -> Option<u8> {
        winner(items, |i| &i.0, |i| i.1, project, id, name).map(|i| i.2)
    }

    #[test]
    fn the_project_beats_the_identity_beats_everywhere() {
        let items = [
            Item(Owner::Everywhere, "T", 3),
            Item(Owner::Identity("opti".into()), "T", 2),
            Item(Owner::Project("c:/a".into()), "T", 1),
            Item(Owner::Project("c:/b".into()), "T", 9),
            Item(Owner::Identity("other".into()), "T", 8),
        ];
        assert_eq!(pick(&items, "c:/a", Some("opti"), "T"), Some(1));
        assert_eq!(pick(&items, "c:/x", Some("opti"), "T"), Some(2));
        assert_eq!(pick(&items, "c:/x", None, "T"), Some(3));
        assert_eq!(pick(&items, "c:/x", Some("nobody"), "T"), Some(3));
        assert_eq!(pick(&items, "c:/a", None, "U"), None);
        let only_others = [Item(Owner::Project("c:/b".into()), "T", 9)];
        assert_eq!(pick(&only_others, "c:/a", None, "T"), None);
    }

    fn mask(values: &[(&str, &str)], chunks: &[&str]) -> String {
        let mut m = Mask::new(
            values
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        );
        let mut out = Vec::new();
        for c in chunks {
            out.extend(m.push(c.as_bytes()));
        }
        out.extend(m.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_value_is_masked() {
        assert_eq!(
            mask(&[("TOKEN", "s3cret")], &["token=s3cret and s3cret\n"]),
            "token=[TOKEN] and [TOKEN]\n"
        );
    }

    #[test]
    fn a_value_split_over_chunks_is_caught() {
        assert_eq!(
            mask(&[("TOKEN", "s3cret")], &["a s3", "c", "ret b"]),
            "a [TOKEN] b"
        );
        assert_eq!(
            mask(&[("TOKEN", "s3cret")], &["s", "3", "c", "r", "e", "t"]),
            "[TOKEN]"
        );
    }

    #[test]
    fn a_held_tail_that_was_not_a_value_comes_out() {
        let mut m = Mask::new(vec![("TOKEN".into(), "s3cret".into())]);
        assert_eq!(m.push(b"abc s3c"), b"abc ");
        assert_eq!(m.push(b"x"), b"s3cx");
        assert_eq!(m.push(b" s3"), b" ");
        assert_eq!(m.finish(), b"s3");
        assert_eq!(m.finish(), b"");
    }

    #[test]
    fn the_longest_value_wins_where_they_overlap() {
        assert_eq!(
            mask(&[("SHORT", "abc"), ("LONG", "abcdef")], &["xabcdefx abcx"]),
            "x[LONG]x [SHORT]x"
        );
        assert_eq!(mask(&[("A", "abcd"), ("B", "cdef")], &["abcdef"]), "[A]ef");
        // A longer value's start held back must not hide a shorter one
        // that turns out to be all there was.
        assert_eq!(
            mask(&[("SHORT", "abc"), ("LONG", "abcdef")], &["abcd", "x"]),
            "[SHORT]dx"
        );
    }

    #[test]
    fn empty_values_mask_nothing() {
        assert_eq!(mask(&[("E", "")], &["plain"]), "plain");
    }

    #[test]
    fn the_question_shows_what_hides_itself() {
        assert_eq!(plainly("gh pr list"), "gh pr list");
        assert_eq!(plainly("a\nb\r\tc"), r"a\nb\r\tc");
        assert_eq!(plainly("gh \u{202e}tsil rp"), r"gh \u{202e}tsil rp");
        assert_eq!(
            plainly("x\u{200b}y\u{feff}z\u{2066}"),
            r"x\u{200b}y\u{feff}z\u{2066}"
        );
        assert_eq!(plainly("\u{1b}[2J\u{7f}"), r"\u{1b}[2J\u{7f}");
        assert_eq!(plainly("tag\u{e0041}"), r"tag\u{e0041}");
        assert_eq!(plainly("line\u{2028}two"), r"line\u{2028}two");
        // Letters of any script stay as they are.
        assert_eq!(plainly("café ﷺ 日本"), "café ﷺ 日本");
    }

    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn pin_paths(program: &str, vars: &[(&str, &str)]) -> Option<Vec<(String, String, PinKind)>> {
        let env = env_of(vars);
        pinned(program, &env).map(|p| p.into_iter().map(|p| (p.what, p.path, p.kind)).collect())
    }

    #[test]
    fn only_known_programs_may_have_always() {
        for p in [
            r"C:\Program Files\Amazon\AWSCLIV2\aws.exe",
            "az",
            "gcloud",
            r"C:\Program Files\Stripe\stripe.exe",
        ] {
            assert!(always_allowed(p), "{p}");
            assert!(pinned(p, &|_| None).is_some(), "{p}");
            assert_eq!(once_reason(p, false), None, "{p}");
        }
        for p in [
            "curl",
            r"C:\Program Files\GitHub CLI\gh.exe",
            r"C:\Windows\System32\cmd.exe",
            "bash",
            "docker",
            "kubectl",
            "helm",
            "git",
            "git-lfs",
            "terraform",
            "firebase",
            // A script or batch file on an allowed name is not the program.
            r"C:\tools\aws.cmd",
            r"C:\tools\stripe.bat",
            r"C:\tools\aws.ps1",
        ] {
            assert!(!always_allowed(p), "{p}");
            assert!(once_reason(p, false).is_some(), "{p}");
        }
        assert_eq!(
            once_reason("curl", false).as_deref(),
            Some("only programs Horadric knows can be allowed always")
        );
        // An npm package is never allowed, whatever its shim is called.
        assert!(once_reason("stripe", true).is_some());
        assert!(runs_builds("vercel.cmd") && runs_builds("wrangler") && runs_builds("NETLIFY.exe"));
        assert!(!runs_builds("gh.exe"));
    }

    #[test]
    fn a_git_running_cli_is_told_to_use_the_identity_login() {
        let gh = once_reason(r"C:\Program Files\GitHub CLI\gh.exe", false).unwrap();
        assert_eq!(
            gh,
            "gh runs git, which can run other programs with the secret; for unattended use, log gh in for this project's identity in Settings"
        );
        assert!(once_reason("vercel", true)
            .unwrap()
            .contains("log vercel in"));
        let netlify = once_reason("netlify-cli", true).unwrap();
        assert!(
            netlify.contains("runs git") && !netlify.contains("log"),
            "{netlify}"
        );
        assert_eq!(login_tool("gh.exe"), Some("gh"));
        assert_eq!(login_tool("wrangler"), None);
        for t in ["gh", "vercel"] {
            assert!(
                crate::vault::identity::Tool::from_name(t).is_some(),
                "{t} is a login --on-login takes"
            );
        }
    }

    #[test]
    fn an_always_entry_names_its_program() {
        assert_eq!(
            allowed_program(r"c:\program files\github cli\gh.exe pr create"),
            r"c:\program files\github cli\gh.exe"
        );
        assert_eq!(
            allowed_program(r"c:\a.exe.d\stripe.exe"),
            r"c:\a.exe.d\stripe.exe"
        );
        assert_eq!(
            allowed_program(r"c:\x\aws.exe s3 ls <stdin K>"),
            r"c:\x\aws.exe"
        );
        assert_eq!(allowed_program("aws"), "aws");
    }

    #[test]
    fn cloud_clis_pin_their_exec_hooks() {
        let home = [
            ("USERPROFILE", r"C:\Users\a"),
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
        ];
        let paths = |p: &str, vars: &[(&str, &str)]| -> Vec<String> {
            pin_paths(p, vars)
                .unwrap()
                .into_iter()
                .map(|p| p.1)
                .collect()
        };
        assert_eq!(
            paths("aws", &home),
            [
                r"C:\Users\a\.aws\config",
                r"C:\Users\a\.aws\credentials",
                r"C:\Users\a\.aws\cli\alias",
                r"C:\Users\a\.aws\cli",
                r"C:\Users\a\.aws\models"
            ]
        );
        assert_eq!(pin_paths("aws", &home).unwrap()[0].2, PinKind::Text);
        assert_eq!(pin_paths("aws", &home).unwrap()[3].2, PinKind::Names);
        assert_eq!(
            paths(
                "aws",
                &[
                    ("USERPROFILE", r"C:\Users\a"),
                    ("AWS_CONFIG_FILE", r"D:\aws.cfg")
                ]
            )[0],
            r"D:\aws.cfg"
        );
        assert_eq!(
            paths("gcloud", &[("CLOUDSDK_CONFIG", r"D:\g")]),
            [
                r"D:\g\properties",
                r"D:\g\active_config",
                r"D:\g\configurations"
            ]
        );
        assert_eq!(
            paths("az", &home),
            [
                r"C:\Users\a\.azure\config",
                r"C:\Users\a\.azure\cliextensions"
            ]
        );
        // No home to place them in pins nothing rather than a wrong path.
        assert!(paths("aws", &[]).is_empty());
        for pin in pinned("gcloud", &env_of(&home)).unwrap() {
            assert!(!pin.what.contains([',', '=', '|']), "{}", pin.what);
        }
    }

    fn args(line: &str) -> Vec<String> {
        line.split(' ')
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn an_always_is_for_the_subcommand_too() {
        assert_eq!(command_path(&args("s3 ls s3://b")), "s3 ls");
        assert_eq!(
            command_path(&args("ec2 describe-instances --region x")),
            "ec2 describe-instances"
        );
        assert_eq!(command_path(&args("--version")), "");
        assert_eq!(command_path(&args("a b c d")), "a b c");
        assert_eq!(command_path(&args("https://x.example")), "");
        assert_eq!(command_path(&[]), "");
    }

    #[test]
    fn arguments_that_reach_past_the_command_leave_only_once() {
        let refused = |stem: &str, line: &str| always_refused(stem, &args(line)).is_some();
        // aws reads its input from a file only with file://.
        assert!(refused(
            "aws",
            "ec2 run-instances --cli-input-json file://x.json"
        ));
        assert!(refused(
            "aws",
            "ec2 run-instances --cli-input-yaml=fileb://x"
        ));
        assert!(refused("aws", "configure set x y"));
        assert!(!refused("aws", "ec2 run-instances --cli-input-json {}"));
        assert!(!refused("aws", "s3 ls --profile work"));
        for line in [
            "bedrock-runtime converse --endpoint-url https://evil.example",
            "s3 ls --endpoint-url=https://evil.example",
            "s3 ls --ca-bundle x.pem",
            "s3 ls --no-verify-ssl",
        ] {
            assert!(refused("aws", line), "aws {line}");
        }
        // No subcommand, or a flag before it, is never an Always for all of it.
        for line in ["", "--profile work s3 ls", "-- s3 ls"] {
            assert!(refused("aws", line), "aws {line}");
        }
        assert!(!refused("aws", "--version"));
        assert!(refused("gcloud", "run deploy --flags-file=f.yaml"));
        assert!(refused("gcloud", "source repos clone r"));
        assert!(refused("gcloud", "components install x"));
        assert!(!refused("gcloud", "run deploy --configuration work"));
        assert!(refused("az", "vm create @args.txt"));
        assert!(refused("az", "extension add -n x"));
        assert!(refused("az", "bicep install"));
        assert!(refused("az", "repos pr checkout --id 1"));
        assert!(refused("az", "acr login -n r"));
        assert!(!refused("az", "acr list"));
        assert!(refused("aws", "codeartifact login --tool npm"));
        assert!(!refused("az", "login --debug"));
        // A flag after `--` is an operand.
        assert!(!refused("gcloud", "run deploy -- --flags-file=x"));
        // A program with no rules of its own is not refused here.
        assert!(!refused("curl", "-t x"));
        // stripe: another host, another config, plugins, git or a login.
        for line in [
            "",
            "--api-base https://evil.example customers list",
            "customers list --api-base=https://evil.example",
            "listen --files-base x",
            "customers list --config c.toml",
            "plugin install apps",
            "samples create x",
            "login",
            "config --list",
        ] {
            assert!(refused("stripe", line), "stripe {line}");
        }
        for line in [
            "trigger payment_intent.succeeded",
            "trigger ./f.json",
            "fixtures f.json",
            "customers create --raw {}",
            "listen --edit",
        ] {
            assert!(refused("stripe", line), "stripe {line}");
        }
        for line in ["customers list", "listen --forward-to localhost:3000"] {
            assert!(!refused("stripe", line), "stripe {line}");
        }
    }

    const SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\vercel\\dist\\index.js\" %*\r\n";

    #[test]
    fn an_npm_shim_names_its_package_and_script() {
        assert_eq!(
            npm_shim(SHIM),
            Some(Shim {
                package: r"node_modules\vercel".into(),
                script: r"node_modules\vercel\dist\index.js".into(),
            })
        );
        let scoped = SHIM.replace(
            r"node_modules\vercel\dist",
            r"node_modules\@netlify\cli\bin",
        );
        assert_eq!(
            npm_shim(&scoped).unwrap().package,
            r"node_modules\@netlify\cli"
        );
        for bad in [
            SHIM.replace(r"node_modules\vercel", r"node_modules\..\..\x"),
            SHIM.replace(r"node_modules\vercel", r"other\vercel"),
            SHIM.replace("\" %*", "\" --flag %*"),
            SHIM.replace("\"%_prog%\"  \"", "\"%_prog%\" --x \""),
            SHIM.replace("node.exe", "sh.exe"),
            SHIM.replace(r"vercel\dist\index.js", "vercel"),
            SHIM.replace(r"dist\index.js", "dist\\%X%.js"),
            "@echo off\r\nnode x.js %*\r\n".to_string(),
        ] {
            assert_eq!(npm_shim(&bad), None, "{bad}");
        }
    }

    #[test]
    fn a_changed_pin_is_named() {
        let gh = r"C:\Program Files\GitHub CLI\gh.exe";
        let pins = |c: &str, e: &str| {
            vec![
                ("gh's config.yml".to_string(), c.to_string()),
                ("gh's extensions".to_string(), e.to_string()),
            ]
        };
        let was = fingerprint("ABC", &pins("c1", "e1"));
        let allowed = vec![approval("c:/a", gh, &was)];
        assert!(approved(
            &allowed,
            "c:/a",
            gh,
            &fingerprint("abc", &pins("c1", "e1"))
        ));
        let now = fingerprint("abc", &pins("c2", "e1"));
        assert!(!approved(&allowed, "c:/a", gh, &now));
        assert_eq!(
            changed_pins(&allowed, "c:/a", gh, &now),
            ["gh's config.yml"]
        );
        // Another program content, project or none allowed names nothing.
        assert!(
            changed_pins(&allowed, "c:/a", gh, &fingerprint("abd", &pins("c2", "e1"))).is_empty()
        );
        assert!(changed_pins(&allowed, "c:/b", gh, &now).is_empty());
        assert!(changed_pins(&[], "c:/a", gh, &now).is_empty());
        // A pin that is new since, or gone, is named too.
        let more = fingerprint("abc", &[("gh's config.yml".into(), "c1".into())]);
        assert_eq!(
            changed_pins(&allowed, "c:/a", gh, &more),
            ["gh's extensions"]
        );
    }

    #[test]
    fn an_always_is_for_one_project_one_program_and_its_content() {
        let gh = r"C:\Program Files\GitHub CLI\gh.exe";
        let allowed = vec![approval("c:/a", gh, "ABC")];
        assert!(approved(&allowed, "c:/a", gh, "abc"));
        assert!(approved(&allowed, "c:/a", &gh.to_uppercase(), "abc"));
        assert!(!approved(&allowed, "c:/b", gh, "abc"), "another project");
        assert!(!approved(&allowed, "c:/a", gh, "abd"), "a changed program");
        assert!(
            !approved(&allowed, "c:/a", r"C:\x\gh.exe", "abc"),
            "another program"
        );
        // A list from before Always was bound, a bare path, matches nothing.
        assert!(!approved(&[gh.to_ascii_lowercase()], "c:/a", gh, "abc"));
        assert!(approval("c:/a", gh, "abc").ends_with(r"\gh.exe"));
    }

    #[test]
    fn a_log_line_never_holds_more_than_it_is_given() {
        let line = log_line(0, "c:/a", "s1", "TOKEN", r"C:\gh.exe", "allowed");
        assert_eq!(
            line,
            "1970-01-01 00:00:00Z\tc:/a\ts1\tTOKEN\tC:\\gh.exe\tallowed"
        );
        let line = log_line(1_762_432_496, "p\nq", "s", "N", "x", "asked");
        assert!(line.starts_with("2025-11-06 12:34:56Z\t"), "{line}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn prompt_lines_name_and_never_value() {
        assert!(prompt_lines(&[], &[]).is_empty());
        let lines = prompt_lines(&["GITHUB_TOKEN".into()], &[]);
        assert!(lines[0].contains("GITHUB_TOKEN"));
        assert!(lines.iter().any(|l| l.contains("--on-secret")));
        let lines = prompt_lines(&[], &["EXAMPLE".into()]);
        assert!(lines[0].contains("browser_fill_login"));
    }
}
