//! Identities: whose a project is, and the environment that points each
//! tool at that identity's own logins. See docs/plan/vault.md, "Whose is
//! what" and "Logins".

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One of the human's worlds, with the tools it has logins for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    /// Fixed when the identity is made: its folder is named by it and
    /// config files name it, so a rename keeps it.
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<Git>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Folders whose projects are this identity's unless they pick.
    #[serde(default)]
    pub claims: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Git {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_user: Option<String>,
}

/// A tool the identity has its own login for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Gh,
    Npm,
    Vercel,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Gh, Tool::Npm, Tool::Vercel];

    /// The command's name, as `--on-login` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Gh => "gh",
            Tool::Npm => "npm",
            Tool::Vercel => "vercel",
        }
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        Tool::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The arguments of the tool's own login.
    pub fn login_args(self) -> &'static [&'static str] {
        match self {
            Tool::Gh => &["auth", "login"],
            Tool::Npm => &["login"],
            Tool::Vercel => &["login"],
        }
    }
}

/// Which identity a project gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Identity(String),
    None,
    /// A slug the project's config file names that this install has no
    /// identity for, a colleague's clone say. Behaves as None; Settings
    /// offers to make one by that slug.
    Unknown(String),
}

impl Resolved {
    /// The slug of a known identity, None for none or unknown.
    pub fn slug(&self) -> Option<&str> {
        match self {
            Resolved::Identity(s) => Some(s),
            _ => None,
        }
    }
}

/// Tokens set for the whole machine that would beat an identity's login
/// without a word.
pub const MACHINE_TOKENS: [&str; 5] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "NPM_TOKEN",
    "VERCEL_TOKEN",
];

/// The variables [`tool_env`] points a tool at an identity's folder with.
pub const TOOL_VARS: [&str; 3] = ["GH_CONFIG_DIR", "NPM_CONFIG_USERCONFIG", "XDG_DATA_HOME"];

/// Whether `name` is a variable an identity's session is given. One left
/// over in a process started from such a session (a `reload`, a dev
/// instance, a `horadric run`) would make everything it starts that
/// identity, so they are never inherited, only set.
pub fn identity_var(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    TOOL_VARS.contains(&n.as_str())
        || n == "GIT_CONFIG_COUNT"
        || n.starts_with("GIT_CONFIG_KEY_")
        || n.starts_with("GIT_CONFIG_VALUE_")
}

/// A folder name made of `name`: lower case ASCII letters and digits, the
/// rest one hyphen. Never empty.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = match c {
            'æ' | 'Æ' => Some("ae"),
            'ø' | 'Ø' => Some("o"),
            'å' | 'Å' => Some("a"),
            _ => None,
        }
        .map(String::from)
        .unwrap_or_else(|| c.to_ascii_lowercase().to_string());
        for c in c.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    let out = out.trim_end_matches('-');
    let out: String = out.chars().take(40).collect();
    match out.trim_end_matches('-') {
        "" => "identity".into(),
        s => s.into(),
    }
}

/// A slug for a new identity called `name`, unlike every one in `all`.
pub fn new_slug(name: &str, all: &[Identity]) -> String {
    let base = slug(name);
    let taken = |s: &str| all.iter().any(|i| i.slug == s);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .unwrap_or(base)
}

/// Which identity the project in `folder` (its key) gets: the human's
/// `pick` on this machine, else the slug its config `file` names, else the
/// longest folder claim, else none. A pick of "" is a pick of none, which
/// beats the file and the claims. A pick of an identity since deleted is
/// as no pick.
///
/// Not a security boundary: it picks whose logins a session starts with,
/// for the human's convenience. An agent can point `GH_CONFIG_DIR` at any
/// folder itself, and the config file it can edit names the identity.
pub fn resolve(pick: Option<&str>, file: Option<&str>, folder: &str, all: &[Identity]) -> Resolved {
    let known = |s: &str| {
        let s = s.trim();
        all.iter()
            .find(|i| i.slug.eq_ignore_ascii_case(s))
            .map(|i| i.slug.clone())
    };
    if let Some(pick) = pick {
        if pick.trim().is_empty() {
            return Resolved::None;
        }
        if let Some(s) = known(pick) {
            return Resolved::Identity(s);
        }
    }
    if let Some(file) = file.filter(|f| !f.trim().is_empty()) {
        return match known(file) {
            Some(s) => Resolved::Identity(s),
            None => Resolved::Unknown(file.trim().to_lowercase()),
        };
    }
    let folder = path_key(folder);
    all.iter()
        .flat_map(|i| i.claims.iter().map(move |c| (i, path_key(c))))
        .filter(|(_, c)| !c.is_empty() && under(&folder, c))
        .max_by_key(|(_, c)| c.len())
        .map(|(i, _)| Resolved::Identity(i.slug.clone()))
        .unwrap_or(Resolved::None)
}

/// One spelling of a folder: forward slashes, no long path prefix or
/// trailing slash, lower case, since Windows and a Mac's default disk
/// both ignore case.
pub fn path_key(p: &str) -> String {
    let p = p.trim().replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    p.trim_end_matches('/').to_lowercase()
}

/// Whether `folder` is `claim` or inside it, whole names only.
fn under(folder: &str, claim: &str) -> bool {
    folder == claim
        || folder
            .strip_prefix(claim)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The variables that point `tool` at the identity's folder `dir`.
pub fn tool_env(tool: Tool, dir: &Path) -> Vec<(String, String)> {
    let at = |p: &Path| p.to_string_lossy().into_owned();
    match tool {
        Tool::Gh => vec![("GH_CONFIG_DIR".into(), at(&dir.join("gh")))],
        Tool::Npm => vec![(
            "NPM_CONFIG_USERCONFIG".into(),
            at(&dir.join("npm").join(".npmrc")),
        )],
        // It moves every XDG minded tool's data too, so only for an
        // identity that logs in to Vercel.
        Tool::Vercel => vec![("XDG_DATA_HOME".into(), at(&dir.join("xdg-data")))],
    }
}

/// The `GIT_CONFIG_*` entries for `git`: config given to every git the
/// session runs, with nothing written to any `.gitconfig`.
pub fn git_env(git: &Git) -> Vec<(String, String)> {
    let pairs: Vec<(&str, &str)> = [
        ("user.name", git.name.trim()),
        ("user.email", git.email.trim()),
        (
            "credential.https://github.com.username",
            git.github_user.as_deref().unwrap_or("").trim(),
        ),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), pairs.len().to_string())];
    for (n, (k, v)) in pairs.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{n}"), k.into()));
        env.push((format!("GIT_CONFIG_VALUE_{n}"), v.into()));
    }
    env
}

/// What a session of identity `id`, its folder `dir`, sets and removes.
pub fn env(id: &Identity, dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let mut set = Vec::new();
    for tool in Tool::ALL.into_iter().filter(|t| id.tools.contains(t)) {
        set.extend(tool_env(tool, dir));
    }
    if let Some(git) = &id.git {
        set.extend(git_env(git));
    }
    let remove = MACHINE_TOKENS.iter().map(|s| s.to_string()).collect();
    (set, remove)
}

/// The slug `.horadric/config.json` names under `"identity"`.
pub fn config_identity(config_json: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(strip_bom(config_json)).ok()?;
    let s = v.get("identity")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_lowercase())
}

/// The secret names `.horadric/config.json` says the project needs, each
/// once, those not a valid name left out.
pub fn config_secrets(config_json: &str) -> Vec<String> {
    let v = serde_json::from_str::<Value>(strip_bom(config_json)).unwrap_or(Value::Null);
    let mut out: Vec<String> = Vec::new();
    for s in v
        .get("secrets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
    {
        if secret_name(s) && !out.iter().any(|o| o == s) {
            out.push(s.into());
        }
    }
    out
}

/// An environment variable's name as the Vault keeps them, the same rule
/// as `secret::valid_name`.
fn secret_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// The active `github.com` user in `gh`'s `hosts.yml`: the host's own
/// `user:`, not a name under its `users:`.
pub fn gh_user(hosts_yml: &str) -> Option<String> {
    let mut in_host = false;
    let mut child: Option<usize> = None;
    for line in strip_bom(hosts_yml).lines() {
        let text = line.trim_end();
        let body = text.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indent = text.len() - body.len();
        if indent == 0 {
            in_host = unquote(body.trim_end_matches(':')) == "github.com" && body.ends_with(':');
            child = None;
            continue;
        }
        if !in_host {
            continue;
        }
        let level = *child.get_or_insert(indent);
        if indent != level {
            continue;
        }
        if let Some(user) = body.strip_prefix("user:") {
            let user = unquote(user.trim());
            return (!user.is_empty()).then(|| user.to_string());
        }
    }
    None
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner;
        }
    }
    s
}

/// The registries an `.npmrc` holds a login for, `registry.npmjs.org`,
/// each once.
pub fn npm_registries(npmrc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in strip_bom(npmrc).lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("//") else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let Some((host, field)) = key.trim().rsplit_once(':') else {
            continue;
        };
        if !matches!(field, "_authToken" | "_auth" | "_password") || value.trim().is_empty() {
            continue;
        }
        let host = host.trim_end_matches('/').to_string();
        if !host.is_empty() && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// Whether Vercel's `auth.json` holds a login. It holds no name.
pub fn vercel_logged_in(auth_json: &str) -> bool {
    serde_json::from_str::<Value>(strip_bom(auth_json))
        .ok()
        .and_then(|v| v.get("token")?.as_str().map(|t| !t.trim().is_empty()))
        .unwrap_or(false)
}

/// What a session of `id` is told: whose project it is, who it is logged
/// in as per tool (`who`, a tool's user; Some("") is logged in with no
/// name known, None not logged in), and to leave the logins alone.
pub fn prompt_lines(id: &Identity, who: &[(Tool, Option<String>)]) -> Vec<String> {
    let mut lines = vec![format!(
        "This project belongs to the identity \"{}\". Horadric started this session with that identity's own logins, not the machine's global ones.",
        id.name
    )];
    if let Some(git) = &id.git {
        let mut parts = Vec::new();
        match (git.name.trim(), git.email.trim()) {
            ("", "") => {}
            (n, "") => parts.push(format!("git commits as {n}")),
            ("", e) => parts.push(format!("git commits as <{e}>")),
            (n, e) => parts.push(format!("git commits as {n} <{e}>")),
        }
        if let Some(u) = git.github_user.as_deref().filter(|u| !u.trim().is_empty()) {
            parts.push(format!("pushes to GitHub over HTTPS as {}", u.trim()));
        }
        if !parts.is_empty() {
            lines.push(format!("{}.", capital(&parts.join(" and "))));
        }
    }
    for (tool, user) in who {
        let what = tool_line(*tool);
        lines.push(match user.as_deref().map(str::trim) {
            Some("") => format!("{}: logged in{what}.", tool.name()),
            Some(u) => format!("{}: logged in as {u}{what}.", tool.name()),
            None => format!("{}: not logged in for {}.", tool.name(), id.name),
        });
    }
    lines.push(
        "Never run a tool's login, logout or account switch (`gh auth login`, `gh auth logout`, `gh auth switch`, `npm login`, `npm logout`, `vercel login`, `vercel logout`, `vercel switch`), and never set git's user.name or user.email: the logins are the human's, set per identity.".into(),
    );
    lines.push(
        "If a tool says it is not logged in or not authorized, block the quest with `horadric quest blocked --on-login <tool>` (gh, npm or vercel), and it goes on once the human has logged that tool in for this identity.".into(),
    );
    lines
}

fn tool_line(tool: Tool) -> &'static str {
    match tool {
        Tool::Npm => " (registries with a login)",
        _ => "",
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The pick in a project's local entry: Some(slug), Some("") for a pick
/// of none, None for no pick.
pub fn local_pick(entry: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(entry).ok()?;
    match v.get("identity")? {
        Value::String(s) => Some(s.trim().to_lowercase()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

/// `entry` with the pick set: Some(slug), Some("") for none (written as
/// null), None to take the pick out so the file and claims decide.
pub fn with_local_pick(entry: &str, pick: Option<&str>) -> String {
    let mut root = match serde_json::from_str::<Value>(entry) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    match pick.map(str::trim) {
        None => {
            root.remove("identity");
        }
        Some("") => {
            root.insert("identity".into(), Value::Null);
        }
        Some(s) => {
            root.insert("identity".into(), Value::String(s.to_lowercase()));
        }
    }
    Value::Object(root).to_string()
}

/// The keys of the local store's projects whose pick is `slug`, so a
/// deleted identity's picks go with it and a later one by the same slug
/// does not inherit them.
pub fn picks_of(store: &str, slug: &str) -> Vec<String> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(strip_bom(store)) else {
        return Vec::new();
    };
    root.iter()
        .filter(|(_, e)| {
            e.get("identity")
                .and_then(Value::as_str)
                .is_some_and(|s| s.trim().eq_ignore_ascii_case(slug.trim()))
        })
        .map(|(k, _)| k.clone())
        .collect()
}

/// `claims` with `folder` added, unless it or the same folder spelled
/// otherwise is there.
pub fn with_claim(claims: &[String], folder: &str) -> Vec<String> {
    let mut out = claims.to_vec();
    let key = path_key(folder);
    if !key.is_empty() && !out.iter().any(|c| path_key(c) == key) {
        out.push(folder.trim().trim_end_matches(['\\', '/']).to_string());
    }
    out
}

/// `claims` without `folder`, however it was spelled.
pub fn without_claim(claims: &[String], folder: &str) -> Vec<String> {
    let key = path_key(folder);
    claims
        .iter()
        .filter(|c| path_key(c) != key)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn identity_vars_are_the_ones_env_sets() {
        let id = Identity {
            name: "m".into(),
            slug: "m".into(),
            tools: Tool::ALL.to_vec(),
            git: Some(Git {
                name: "M".into(),
                email: "m@x".into(),
                github_user: Some("m".into()),
            }),
            ..Default::default()
        };
        let (set, _) = env(&id, Path::new("d"));
        assert!(set.iter().all(|(k, _)| identity_var(k)));
        assert!(identity_var("git_config_key_7"));
        assert!(!identity_var("GIT_DIR"));
        assert!(!identity_var("PATH"));
    }

    #[test]
    fn picks_of_finds_only_that_slug() {
        let store = r#"{"c:/a":{"identity":"Mopra"},"c:/b":{"identity":null},"c:/c":{"identity":"other"},"c:/d":{}}"#;
        assert_eq!(picks_of(store, "mopra"), vec!["c:/a".to_string()]);
        assert!(picks_of("not json", "mopra").is_empty());
    }

    fn id(slug: &str, claims: &[&str]) -> Identity {
        Identity {
            name: slug.into(),
            slug: slug.into(),
            claims: claims.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn slugs_are_folder_names() {
        assert_eq!(slug("Acme"), "acme");
        assert_eq!(slug("Globex Labs"), "globex-labs");
        assert_eq!(slug("  Hello,  World! "), "hello-world");
        assert_eq!(slug("Søren's Æble"), "soren-s-aeble");
        assert_eq!(slug("!!!"), "identity");
        assert_eq!(slug(""), "identity");
        assert!(slug(&"a".repeat(100)).len() <= 40);
    }

    #[test]
    fn a_new_slug_is_unlike_the_others() {
        let all = [id("work", &[]), id("work-2", &[])];
        assert_eq!(new_slug("Work", &all), "work-3");
        assert_eq!(new_slug("Home", &all), "home");
    }

    #[test]
    fn the_pick_beats_the_file_and_the_claims() {
        let all = [id("opti", &["C:/code"]), id("me", &[])];
        assert_eq!(
            resolve(Some("me"), Some("opti"), "c:/code/app", &all),
            Resolved::Identity("me".into())
        );
        assert_eq!(
            resolve(Some(""), Some("opti"), "c:/code/app", &all),
            Resolved::None
        );
    }

    #[test]
    fn a_deleted_pick_is_no_pick() {
        let all = [id("opti", &[])];
        assert_eq!(
            resolve(Some("gone"), Some("OPTI"), "c:/x", &all),
            Resolved::Identity("opti".into())
        );
    }

    #[test]
    fn the_file_beats_claims_and_an_unknown_slug_is_unknown() {
        let all = [id("opti", &["c:/code"]), id("me", &[])];
        assert_eq!(
            resolve(None, Some("me"), "c:/code/app", &all),
            Resolved::Identity("me".into())
        );
        let r = resolve(None, Some("Colleague"), "c:/code/app", &all);
        assert_eq!(r, Resolved::Unknown("colleague".into()));
        assert_eq!(r.slug(), None);
    }

    #[test]
    fn the_longest_claim_wins() {
        let all = [
            id("outer", &[r"C:\Users\Me\Documents\Github\"]),
            id("inner", &[r"c:\users\me\documents\github\opti"]),
        ];
        let at = |f| resolve(None, None, f, &all);
        assert_eq!(
            at("c:/users/me/documents/github/opti/app"),
            Resolved::Identity("inner".into())
        );
        assert_eq!(
            at("C:/Users/Me/Documents/Github/OPTI"),
            Resolved::Identity("inner".into())
        );
        assert_eq!(
            at("c:/users/me/documents/github/optix"),
            Resolved::Identity("outer".into())
        );
        assert_eq!(at("c:/users/me/documents/other"), Resolved::None);
        assert_eq!(
            at(r"\\?\C:\Users\Me\Documents\Github\x"),
            Resolved::Identity("outer".into())
        );
    }

    #[test]
    fn env_sets_only_what_the_identity_has() {
        let dir = PathBuf::from("ids").join("opti");
        let mut i = id("opti", &[]);
        let (set, remove) = env(&i, &dir);
        assert!(set.is_empty());
        assert_eq!(remove.len(), 5);
        assert!(remove.contains(&"GH_TOKEN".to_string()));
        assert!(remove.contains(&"VERCEL_TOKEN".to_string()));

        i.tools = vec![Tool::Gh, Tool::Npm];
        let (set, _) = env(&i, &dir);
        let get = |k: &str| set.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            get("GH_CONFIG_DIR"),
            Some(dir.join("gh").to_string_lossy().into())
        );
        assert_eq!(
            get("NPM_CONFIG_USERCONFIG"),
            Some(dir.join("npm").join(".npmrc").to_string_lossy().into())
        );
        assert_eq!(get("XDG_DATA_HOME"), None);

        i.tools = vec![Tool::Vercel];
        let (set, _) = env(&i, &dir);
        assert_eq!(set[0].0, "XDG_DATA_HOME");
    }

    #[test]
    fn git_config_entries_are_numbered_from_zero_without_gaps() {
        let git = Git {
            name: "Mo".into(),
            email: String::new(),
            github_user: Some("Mopra".into()),
        };
        assert_eq!(
            git_env(&git),
            [
                ("GIT_CONFIG_COUNT", "2"),
                ("GIT_CONFIG_KEY_0", "user.name"),
                ("GIT_CONFIG_VALUE_0", "Mo"),
                ("GIT_CONFIG_KEY_1", "credential.https://github.com.username"),
                ("GIT_CONFIG_VALUE_1", "Mopra"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert!(git_env(&Git::default()).is_empty());
        let full = Git {
            name: "Mo".into(),
            email: "mo@x.dk".into(),
            github_user: None,
        };
        let e = git_env(&full);
        assert_eq!(e[0], ("GIT_CONFIG_COUNT".into(), "2".into()));
        assert_eq!(e[4], ("GIT_CONFIG_VALUE_1".into(), "mo@x.dk".into()));
    }

    #[test]
    fn config_names_an_identity_and_secrets() {
        let c = "\u{feff}{\"identity\": \" Acme \", \"secrets\": [\"GITHUB_TOKEN\", \"bad\", \"GITHUB_TOKEN\", \"CF_2\", 3]}";
        assert_eq!(config_identity(c), Some("acme".into()));
        assert_eq!(config_secrets(c), ["GITHUB_TOKEN", "CF_2"]);
        assert_eq!(config_identity("{}"), None);
        assert_eq!(config_identity("{\"identity\": \"\"}"), None);
        assert_eq!(config_identity("not json"), None);
        assert!(config_secrets("{\"secrets\": \"X\"}").is_empty());
    }

    #[test]
    fn the_active_gh_user_is_the_hosts_own() {
        let hosts = "github.com:\n    git_protocol: https\n    users:\n        acme-mo:\n        Mopra:\n            user: nested\n    user: acme-mo\n";
        assert_eq!(gh_user(hosts), Some("acme-mo".into()));
        let other = "ghe.example.com:\n    user: corp\ngithub.com:\n    user: \"Mopra\"\n";
        assert_eq!(gh_user(other), Some("Mopra".into()));
        assert_eq!(gh_user("ghe.example.com:\n    user: corp\n"), None);
        assert_eq!(gh_user("github.com:\n    users:\n        x:\n"), None);
        assert_eq!(gh_user(""), None);
    }

    #[test]
    fn npm_registries_are_those_with_a_login() {
        let rc = "; comment\nregistry=https://registry.npmjs.org/\n//registry.npmjs.org/:_authToken=npm_abc\n//npm.pkg.github.com/:_authToken=ghp\n//registry.npmjs.org/:_authToken=again\n//empty.example/:_authToken=\n//corp.example/npm/:_auth=Zm9v\n//x.example/:email=a@b\n";
        assert_eq!(
            npm_registries(rc),
            [
                "registry.npmjs.org",
                "npm.pkg.github.com",
                "corp.example/npm"
            ]
        );
        assert!(npm_registries("").is_empty());
    }

    #[test]
    fn vercel_auth_holds_a_token_or_not() {
        assert!(vercel_logged_in("{\"token\": \"abc\"}"));
        assert!(!vercel_logged_in("{\"token\": \"\"}"));
        assert!(!vercel_logged_in("{}"));
    }

    #[test]
    fn prompt_lines_say_whose_and_who() {
        let mut i = id("opti", &[]);
        i.name = "Acme".into();
        i.git = Some(Git {
            name: "Mo".into(),
            email: "mo@acme.test".into(),
            github_user: Some("acme-mo".into()),
        });
        let who = [
            (Tool::Gh, Some("acme-mo".to_string())),
            (Tool::Npm, None),
            (Tool::Vercel, Some(String::new())),
        ];
        let lines = prompt_lines(&i, &who);
        let all = lines.join("\n");
        assert!(lines[0].contains("\"Acme\""));
        assert!(all.contains(
            "Git commits as Mo <mo@acme.test> and pushes to GitHub over HTTPS as acme-mo."
        ));
        assert!(all.contains("gh: logged in as acme-mo."));
        assert!(all.contains("npm: not logged in for Acme."));
        assert!(all.contains("vercel: logged in."));
        assert!(all.contains("gh auth switch"));
        assert!(all.contains("horadric quest blocked --on-login <tool>"));
        assert!(!all.contains('\u{2014}'));
    }

    #[test]
    fn the_local_pick_has_three_states() {
        let e = with_local_pick("{\"mode\":\"auto\"}", Some("Opti"));
        assert_eq!(local_pick(&e), Some("opti".into()));
        assert!(e.contains("mode"));
        let e = with_local_pick(&e, Some(""));
        assert_eq!(local_pick(&e), Some(String::new()));
        let e = with_local_pick(&e, None);
        assert_eq!(local_pick(&e), None);
        assert_eq!(local_pick("{}"), None);
    }

    #[test]
    fn claims_are_one_per_folder() {
        let c = with_claim(&[], r"C:\Code\Opti\");
        assert_eq!(c, [r"C:\Code\Opti"]);
        assert_eq!(with_claim(&c, "c:/code/opti"), c);
        assert!(without_claim(&c, "c:/code/opti/").is_empty());
    }

    #[test]
    fn tools_by_name() {
        assert_eq!(Tool::from_name(" GH "), Some(Tool::Gh));
        assert_eq!(Tool::from_name("git"), None);
        assert_eq!(serde_json::to_string(&Tool::Vercel).unwrap(), "\"vercel\"");
    }
}
