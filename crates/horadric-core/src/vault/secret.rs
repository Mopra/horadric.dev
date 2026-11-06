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
        values.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
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

/// Programs that run code the agent can write, and so could do anything
/// with a value: they are only ever offered Once, never Always. The
/// deploy and cloud CLIs the Vault is mostly for are not here but in
/// [`pinned`]: what could make them run other code is in files of their
/// own, and an Always holds only while those files stay as they were.
/// Everything here runs the agent's code by design, so no pin helps.
const ONCE_ONLY: &[&str] = &[
    // Shells and interpreters.
    "sh", "bash", "zsh", "fish", "dash", "cmd", "powershell", "pwsh", "python", "python3", "py",
    "pythonw", "node", "deno", "bun", "bunx", "perl", "ruby", "php", "lua", "java", "dotnet",
    "go", "rustc", "r", "rscript", "julia", "busybox", "wsl", "env", "time", "start", "xargs",
    "find", "forfiles",
    // Package managers and build runners, which run the project's scripts.
    "npm", "npx", "pnpm", "pnpx", "yarn", "uv", "uvx", "pip", "pipx", "poetry", "conda",
    "composer", "bundle", "rake", "make", "just", "cargo", "gradle", "mvn", "ant", "msbuild",
    // Version control, and the infrastructure tools that run the
    // project's own code as their configuration.
    "git", "ssh", "scp", "sftp", "terraform", "tofu", "pulumi", "podman",
    // Windows' own ways to run a script or a library.
    "cscript", "wscript", "mshta", "rundll32", "regsvr32", "certutil", "bitsadmin",
    "installutil", "msiexec",
];

/// A file or folder whose content an Always is bound to, because changing
/// it could make the program run other code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// How the question names it: `gh's config.yml`. Never holds `,`, `=`
    /// or `|`, which [`fingerprint`] uses.
    pub what: String,
    pub path: String,
    /// A whole folder, every file in it, rather than one file.
    pub tree: bool,
}

/// The deploy and cloud CLIs that may have Always once what could make
/// them run other code is pinned: their aliases, extensions, plugins,
/// credential helpers and exec hooks. Some for the program at this path,
/// the files and folders to pin as `env` (the environment the child will
/// get, an identity's own folders included) places them; None for any
/// other program. A program that keeps no such file pins nothing extra,
/// but is still here, since the caller pins the npm package it runs from.
pub fn pinned(program_path: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<Vec<Pin>> {
    let stem = stem_of(program_path);
    let var = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let home = || var("USERPROFILE");
    let file = |what: &str, path: Option<String>| {
        path.map(|path| Pin {
            what: what.to_string(),
            path,
            tree: false,
        })
    };
    let tree = |what: &str, path: Option<String>| {
        path.map(|path| Pin {
            what: what.to_string(),
            path,
            tree: true,
        })
    };
    let kube = || -> Vec<Option<Pin>> {
        match var("KUBECONFIG") {
            Some(list) => list
                .split(';')
                .filter(|p| !p.trim().is_empty())
                .map(|p| file("the kubeconfig", Some(p.trim().to_string())))
                .collect(),
            None => vec![file("the kubeconfig", home().map(|h| join(&h, r".kube\config")))],
        }
    };
    let pins: Vec<Option<Pin>> = match stem.as_str() {
        "gh" => {
            let config = var("GH_CONFIG_DIR")
                .or_else(|| var("XDG_CONFIG_HOME").map(|x| join(&x, "gh")))
                .or_else(|| var("APPDATA").map(|a| join(&a, "GitHub CLI")));
            let data = var("GH_DATA_DIR")
                .or_else(|| var("XDG_DATA_HOME").map(|x| join(&x, "gh")))
                .or_else(|| var("LOCALAPPDATA").map(|a| join(&a, "GitHub CLI")));
            vec![
                file("gh's config.yml", config.map(|c| join(&c, "config.yml"))),
                tree("gh's extensions", data.map(|d| join(&d, "extensions"))),
            ]
        }
        "aws" => vec![
            file(
                "the aws config",
                var("AWS_CONFIG_FILE").or_else(|| home().map(|h| join(&h, r".aws\config"))),
            ),
            // `credential_process` runs a command from here too.
            file(
                "the aws credentials file",
                var("AWS_SHARED_CREDENTIALS_FILE").or_else(|| home().map(|h| join(&h, r".aws\credentials"))),
            ),
        ],
        "docker" => {
            let dir = var("DOCKER_CONFIG").or_else(|| home().map(|h| join(&h, ".docker")));
            vec![
                // `credsStore` and `credHelpers` name programs it runs.
                file("docker's config.json", dir.as_ref().map(|d| join(d, "config.json"))),
                tree("docker's cli-plugins", dir.map(|d| join(&d, "cli-plugins"))),
            ]
        }
        // A kubeconfig's `exec` runs a command for credentials.
        "kubectl" => kube(),
        "helm" => {
            let mut pins = kube();
            pins.push(tree(
                "helm's plugins",
                var("HELM_PLUGINS")
                    .or_else(|| var("HELM_DATA_HOME").map(|d| join(&d, "plugins")))
                    .or_else(|| var("APPDATA").map(|a| join(&a, r"helm\plugins"))),
            ));
            pins
        }
        "gcloud" => {
            let dir = var("CLOUDSDK_CONFIG").or_else(|| var("APPDATA").map(|a| join(&a, "gcloud")));
            vec![
                file("gcloud's properties", dir.as_ref().map(|d| join(d, "properties"))),
                file("gcloud's active_config", dir.as_ref().map(|d| join(d, "active_config"))),
                tree("gcloud's configurations", dir.map(|d| join(&d, "configurations"))),
            ]
        }
        "az" => {
            let dir = var("AZURE_CONFIG_DIR").or_else(|| home().map(|h| join(&h, ".azure")));
            vec![
                file("az's config", dir.as_ref().map(|d| join(d, "config"))),
                tree(
                    "az's extensions",
                    var("AZURE_EXTENSION_DIR").or_else(|| dir.map(|d| join(&d, "cliextensions"))),
                ),
            ]
        }
        // Their own config holds tokens that refresh, never a command, so
        // pinning it would only ask again for nothing. What they can be made
        // to run is the project's build; see [`runs_builds`].
        "vercel" | "wrangler" | "netlify" => Vec::new(),
        _ => return None,
    };
    Some(pins.into_iter().flatten().collect())
}

/// Whether the program deploys by running the project's own build
/// (`wrangler.toml`'s `[build]`, `package.json` scripts), which lives in
/// the project and changes with every commit, so no pin can hold it. The
/// question says so, and the human answers knowing it.
pub fn runs_builds(program_path: &str) -> bool {
    matches!(stem_of(program_path).as_str(), "vercel" | "wrangler" | "netlify")
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
    let depth = if parts.get(1).is_some_and(|p| p.starts_with('@')) { 3 } else { 2 };
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
pub fn changed_pins(allowed: &[String], project: &str, program: &str, fingerprint: &str) -> Vec<String> {
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
    let was: Vec<(&str, &str)> = was.split(',').skip(1).filter_map(|p| p.split_once('=')).collect();
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
    allowed.iter().any(|a| *a == want)
}

/// Whether the program at this path is a shell, an interpreter, a script
/// runner, or a tool that runs code of the agent's some other way. Give it
/// the canonical long path: a short name like `POWERS~1.EXE` is not on the
/// list, so the caller resolves it first.
pub fn once_only(program_path: &str) -> bool {
    let file = program_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program_path)
        .to_ascii_lowercase();
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, e),
        _ => (file.as_str(), ""),
    };
    if matches!(
        ext,
        "bat" | "cmd" | "ps1" | "psm1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "hta"
            | "py" | "pyw" | "sh" | "rb" | "pl" | "php" | "jar" | "msi"
    ) {
        return true;
    }
    // python3.12, pip3, node18 and the like.
    let bare = stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
    ONCE_ONLY.contains(&stem) || ONCE_ONLY.contains(&bare) || stem.starts_with("python")
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

    fn pick<'a>(items: &'a [Item], project: &str, id: Option<&str>, name: &str) -> Option<u8> {
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
        assert_eq!(
            mask(&[("A", "abcd"), ("B", "cdef")], &["abcdef"]),
            "[A]ef"
        );
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
    fn shells_interpreters_runners_and_git_are_once_only() {
        for p in [
            r"C:\Windows\System32\cmd.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\nodejs\npm.cmd",
            r"C:\Python312\python.exe",
            r"C:\Python312\python3.12.exe",
            r"C:\Program Files\Git\cmd\GIT.EXE",
            r"C:\tools\deploy.bat",
            r"C:\tools\deploy.ps1",
            "/usr/bin/env",
            "pwsh",
        ] {
            assert!(once_only(p), "{p}");
        }
        for p in [
            r"C:\Windows\System32\rundll32.exe",
            r"C:\Windows\System32\certutil.exe",
            r"C:\Program Files\Java\bin\java.exe",
            r"C:\Go\bin\go.exe",
            r"C:\R\bin\Rscript.exe",
            r"C:\R\bin\R.exe",
            r"C:\Python312\Scripts\pip3.exe",
            r"C:\Windows\System32\find.exe",
            r"C:\Windows\System32\msiexec.exe",
            r"C:\tools\tool.jar",
        ] {
            assert!(once_only(p), "{p}");
        }
        for p in [
            r"C:\Windows\System32\curl.exe",
            r"C:\Program Files\Stripe\stripe.exe",
            r"C:\tools\gofer.exe",
        ] {
            assert!(!once_only(p), "{p}");
        }
    }

    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    fn pin_paths(program: &str, vars: &[(&str, &str)]) -> Option<Vec<(String, String, bool)>> {
        let env = env_of(vars);
        pinned(program, &env).map(|p| p.into_iter().map(|p| (p.what, p.path, p.tree)).collect())
    }

    #[test]
    fn deploy_and_cloud_clis_are_pinned_not_once_only() {
        for p in [
            r"C:\Program Files\GitHub CLI\gh.exe",
            r"C:\tools\vercel.exe",
            r"C:\Users\a\AppData\Roaming\npm\wrangler.cmd",
            "netlify",
            r"C:\Program Files\Amazon\AWSCLIV2\aws.exe",
            "az",
            "gcloud",
            r"C:\Program Files\Docker\Docker\resources\bin\docker.exe",
            "kubectl",
            "helm",
        ] {
            assert!(pinned(p, &|_| None).is_some(), "{p}");
            assert!(!ONCE_ONLY.contains(&stem_of(p).as_str()), "{p}");
        }
        for p in ["terraform", "tofu", "pulumi", "git", "npm", "curl", "stripe"] {
            assert!(pinned(p, &|_| None).is_none(), "{p}");
        }
        assert!(runs_builds("vercel.cmd") && runs_builds("wrangler") && runs_builds("NETLIFY.exe"));
        assert!(!runs_builds("gh.exe"));
    }

    #[test]
    fn gh_pins_its_config_and_extensions_where_its_env_puts_them() {
        let home = [
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\a\AppData\Local"),
        ];
        assert_eq!(
            pin_paths("gh.exe", &home).unwrap(),
            [
                (
                    "gh's config.yml".to_string(),
                    r"C:\Users\a\AppData\Roaming\GitHub CLI\config.yml".to_string(),
                    false
                ),
                (
                    "gh's extensions".to_string(),
                    r"C:\Users\a\AppData\Local\GitHub CLI\extensions".to_string(),
                    true
                ),
            ]
        );
        let identity = [
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("GH_CONFIG_DIR", r"C:\ids\work\gh\"),
            ("XDG_DATA_HOME", r"C:\ids\work\data"),
        ];
        let p = pin_paths("gh", &identity).unwrap();
        assert_eq!(p[0].1, r"C:\ids\work\gh\config.yml");
        assert_eq!(p[1].1, r"C:\ids\work\data\gh\extensions");
    }

    #[test]
    fn cloud_clis_pin_their_exec_hooks() {
        let home = [("USERPROFILE", r"C:\Users\a"), ("APPDATA", r"C:\Users\a\AppData\Roaming")];
        let paths = |p: &str, vars: &[(&str, &str)]| -> Vec<String> {
            pin_paths(p, vars).unwrap().into_iter().map(|p| p.1).collect()
        };
        assert_eq!(paths("aws", &home), [r"C:\Users\a\.aws\config", r"C:\Users\a\.aws\credentials"]);
        assert_eq!(
            paths("aws", &[("USERPROFILE", r"C:\Users\a"), ("AWS_CONFIG_FILE", r"D:\aws.cfg")])[0],
            r"D:\aws.cfg"
        );
        assert_eq!(
            paths("docker", &home),
            [r"C:\Users\a\.docker\config.json", r"C:\Users\a\.docker\cli-plugins"]
        );
        assert_eq!(paths("docker", &[("DOCKER_CONFIG", r"D:\dk")])[0], r"D:\dk\config.json");
        assert_eq!(paths("kubectl", &home), [r"C:\Users\a\.kube\config"]);
        assert_eq!(paths("kubectl", &[("KUBECONFIG", r"D:\a;;D:\b")]), [r"D:\a", r"D:\b"]);
        assert_eq!(
            paths("helm", &home),
            [r"C:\Users\a\.kube\config", r"C:\Users\a\AppData\Roaming\helm\plugins"]
        );
        assert_eq!(
            paths("gcloud", &[("CLOUDSDK_CONFIG", r"D:\g")]),
            [r"D:\g\properties", r"D:\g\active_config", r"D:\g\configurations"]
        );
        assert_eq!(paths("az", &home), [r"C:\Users\a\.azure\config", r"C:\Users\a\.azure\cliextensions"]);
        assert!(paths("vercel", &home).is_empty());
        // No home to place them in pins nothing rather than a wrong path.
        assert!(paths("aws", &[]).is_empty());
        for pin in pinned("gcloud", &env_of(&home)).unwrap() {
            assert!(!pin.what.contains([',', '=', '|']), "{}", pin.what);
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
        let scoped = SHIM.replace(r"node_modules\vercel\dist", r"node_modules\@netlify\cli\bin");
        assert_eq!(npm_shim(&scoped).unwrap().package, r"node_modules\@netlify\cli");
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
            vec![("gh's config.yml".to_string(), c.to_string()), ("gh's extensions".to_string(), e.to_string())]
        };
        let was = fingerprint("ABC", &pins("c1", "e1"));
        let allowed = vec![approval("c:/a", gh, &was)];
        assert!(approved(&allowed, "c:/a", gh, &fingerprint("abc", &pins("c1", "e1"))));
        let now = fingerprint("abc", &pins("c2", "e1"));
        assert!(!approved(&allowed, "c:/a", gh, &now));
        assert_eq!(changed_pins(&allowed, "c:/a", gh, &now), ["gh's config.yml"]);
        // Another program content, project or none allowed names nothing.
        assert!(changed_pins(&allowed, "c:/a", gh, &fingerprint("abd", &pins("c2", "e1"))).is_empty());
        assert!(changed_pins(&allowed, "c:/b", gh, &now).is_empty());
        assert!(changed_pins(&[], "c:/a", gh, &now).is_empty());
        // A pin that is new since, or gone, is named too.
        let more = fingerprint("abc", &[("gh's config.yml".into(), "c1".into())]);
        assert_eq!(changed_pins(&allowed, "c:/a", gh, &more), ["gh's extensions"]);
    }

    #[test]
    fn an_always_is_for_one_project_one_program_and_its_content() {
        let gh = r"C:\Program Files\GitHub CLI\gh.exe";
        let allowed = vec![approval("c:/a", gh, "ABC")];
        assert!(approved(&allowed, "c:/a", gh, "abc"));
        assert!(approved(&allowed, "c:/a", &gh.to_uppercase(), "abc"));
        assert!(!approved(&allowed, "c:/b", gh, "abc"), "another project");
        assert!(!approved(&allowed, "c:/a", gh, "abd"), "a changed program");
        assert!(!approved(&allowed, "c:/a", r"C:\x\gh.exe", "abc"), "another program");
        // A list from before Always was bound, a bare path, matches nothing.
        assert!(!approved(&[gh.to_ascii_lowercase()], "c:/a", gh, "abc"));
        assert!(approval("c:/a", gh, "abc").ends_with(r"\gh.exe"));
    }

    #[test]
    fn a_log_line_never_holds_more_than_it_is_given() {
        let line = log_line(0, "c:/a", "s1", "TOKEN", r"C:\gh.exe", "allowed");
        assert_eq!(line, "1970-01-01 00:00:00Z\tc:/a\ts1\tTOKEN\tC:\\gh.exe\tallowed");
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
        let lines = prompt_lines(&[], &["OPTICLOUD".into()]);
        assert!(lines[0].contains("browser_fill_login"));
    }
}
