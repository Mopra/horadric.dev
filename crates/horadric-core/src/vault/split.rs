//! Splitting a site off: a project signs in to one site as another
//! account while every other site stays as the shared browser has it. See
//! docs/plan/vault.md, "Seamless: nothing to set up".
//!
//! Cookies go one way only, shared to group: Horadric never writes the
//! human's main profile. Copying back was turned down on 2026-11-26: a
//! login made in a split pane can reach other sites through a sign-in
//! provider (Vercel's "Continue with GitHub" as the split account), and
//! copied back it would have signed the shared profile in as that account,
//! undone a logout there, or put an old value over a fresh one.
//!
//! A split is kept on the group (an [`Identity`] with `sites`), never on
//! the project alone: the projects that took the same login share one
//! browser profile, so they must keep the same sites out of it, or one
//! project's copy of the shared cookies would overwrite another's split
//! login in the profile they share.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{json, Value};

use super::identity::{git_env, Git, Identity};

/// The sites whose command line tools follow them, by the host the plan
/// names them by. Compared by registrable domain, so dash.cloudflare.com
/// is cloudflare.com: Cloudflare's login cookies are kept there.
const SITE_TOOLS: [(&str, &[&str]); 6] = [
    ("github.com", &["gh", "git"]),
    ("vercel.com", &["vercel"]),
    ("npmjs.com", &["npm"]),
    ("dash.cloudflare.com", &["wrangler"]),
    ("portal.azure.com", &["az"]),
    ("console.cloud.google.com", &["gcloud"]),
];

/// The sign-in domains a site's login goes through, kept out with it:
/// without them a split azure.com would still sign in through the shared
/// profile's Microsoft login.
const COMPANIONS: [(&str, &[&str]); 1] = [(
    "azure.com",
    &["login.microsoftonline.com", "login.live.com"],
)];

/// Suffixes under which every name is a site of its own, the short part of
/// the public suffix list that a split is likely to meet. Not the whole
/// list: that is a dependency for a guess that only names a split.
const SUFFIXES: [&str; 26] = [
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "com.au",
    "net.au",
    "org.au",
    "co.nz",
    "co.jp",
    "com.br",
    "co.za",
    "com.cn",
    "co.in",
    "com.mx",
    "github.io",
    "vercel.app",
    "pages.dev",
    "workers.dev",
    "netlify.app",
    "herokuapp.com",
    "azurewebsites.net",
    "web.app",
    "firebaseapp.com",
    "blob.core.windows.net",
    "cloudfront.net",
    "fly.dev",
];

/// The registrable domain of `host`: the part one owner controls, so
/// gist.github.com is github.com and acme.github.io is itself. An address
/// or a single label (localhost) is its own.
pub fn registrable(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host.to_string();
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return host.to_string();
    }
    let keep = SUFFIXES
        .iter()
        .filter(|s| host == **s || host.ends_with(&format!(".{s}")))
        .map(|s| s.split('.').count() + 1)
        .max()
        .unwrap_or(2)
        .min(labels.len());
    labels[labels.len() - keep..].join(".")
}

/// The site a page at `url` is on, None for a page that is on none (a
/// file, about:blank, the Forge's own).
pub fn site_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    // A cookie for an IPv6 address names it in brackets, so the site does.
    let host = match host.strip_prefix('[') {
        Some(v6) => format!("[{}]", v6.split(']').next()?),
        None => host.split(':').next()?.to_string(),
    };
    (!host.is_empty() && host != "[]").then(|| registrable(&host))
}

/// The command line tools a split of `site` carries with it.
pub fn tools_of(site: &str) -> &'static [&'static str] {
    SITE_TOOLS
        .iter()
        .find(|(host, _)| registrable(host) == site)
        .map_or(&[], |(_, tools)| tools)
}

/// Whether a cookie set for `domain` (".github.com", "gist.github.com")
/// belongs to `site`.
pub fn in_site(domain: &str, site: &str) -> bool {
    let d = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    let site = site.to_ascii_lowercase();
    d == site || d.ends_with(&format!(".{site}"))
}

/// The domains kept out for `sites`: the sites and their sign-in domains.
pub fn kept_out(sites: &[String]) -> Vec<String> {
    let mut out = sites.to_vec();
    for site in sites {
        for (s, more) in COMPANIONS {
            if s == site {
                out.extend(more.iter().map(|m| m.to_string()));
            }
        }
    }
    out
}

/// The host of a partitioned cookie's top level site.
fn top_site(c: &Value) -> Option<String> {
    let top = match c.get("partitionKey")? {
        Value::String(s) => s.as_str(),
        k => k.get("topLevelSite")?.as_str()?,
    };
    let rest = top.split_once("://").map_or(top, |(_, r)| r);
    let host = rest.split(['/', ':']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// Whether cookie `c` belongs to one of `sites` or their sign-in domains:
/// by its domain, or for a partitioned cookie by the top level site it was
/// set under, since it holds that site's state too.
fn split(c: &Value, sites: &[String]) -> bool {
    let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
    let top = top_site(c);
    kept_out(sites)
        .iter()
        .any(|s| in_site(domain, s) || top.as_deref().is_some_and(|t| in_site(t, s)))
}

/// `sites` with `site` added, each once.
pub fn with_site(sites: &[String], site: &str) -> Vec<String> {
    let mut out = sites.to_vec();
    if !out.iter().any(|s| s == site) {
        out.push(site.to_string());
    }
    out
}

pub fn without_site(sites: &[String], site: &str) -> Vec<String> {
    sites.iter().filter(|s| *s != site).cloned().collect()
}

/// The groups another project could take `site` as: those that split it.
pub fn offers<'a>(groups: &'a [Identity], site: &str) -> Vec<&'a Identity> {
    groups
        .iter()
        .filter(|g| g.sites.as_ref().is_some_and(|s| s.iter().any(|s| s == site)))
        .collect()
}

/// What a session of the split group `id`, its folder `dir`, sets and
/// removes: only the tools of the sites it split are pointed at its
/// folders, every other tool stays the machine's. Tools with no variable
/// for their folder (wrangler) stay global even when their site is split.
/// `gh_user` is who the group's gh is signed in as, where known.
pub fn env(
    id: &Identity,
    sites: &[String],
    dir: &Path,
    gh_user: Option<&str>,
) -> (Vec<(String, String)>, Vec<String>) {
    let at = |p: &Path| p.to_string_lossy().into_owned();
    let mut set = Vec::new();
    let mut remove = Vec::new();
    let mut tools: Vec<&str> = sites.iter().flat_map(|s| tools_of(s)).copied().collect();
    tools.dedup();
    for tool in tools {
        match tool {
            "gh" => {
                set.push(("GH_CONFIG_DIR".into(), at(&dir.join("gh"))));
                remove.extend(["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"].map(String::from));
            }
            "git" => match (&id.git, gh_user.map(str::trim).filter(|u| !u.is_empty())) {
                (Some(git), _) => set.extend(git_env(git)),
                // Git Credential Manager then answers with the account the
                // group's gh is signed in as, not the machine's.
                (None, Some(user)) => set.extend(git_env(&Git {
                    github_user: Some(user.to_string()),
                    ..Default::default()
                })),
                (None, None) => {}
            },
            "npm" => {
                set.push((
                    "NPM_CONFIG_USERCONFIG".into(),
                    at(&dir.join("npm").join(".npmrc")),
                ));
                remove.push("NPM_TOKEN".into());
            }
            "vercel" => {
                set.push(("XDG_DATA_HOME".into(), at(&dir.join("xdg-data"))));
                remove.push("VERCEL_TOKEN".into());
            }
            "az" => set.push(("AZURE_CONFIG_DIR".into(), at(&dir.join("azure")))),
            "gcloud" => set.push(("CLOUDSDK_CONFIG".into(), at(&dir.join("gcloud")))),
            _ => {}
        }
    }
    (set, remove)
}

/// What names one cookie across profiles: a second cookie of the same
/// name, domain, path and partition replaces it.
fn key_of(c: &Value) -> String {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let partition = c.get("partitionKey").map(Value::to_string).unwrap_or_default();
    format!("{}\n{}\n{}\n{partition}", s("name"), s("domain"), s("path"))
}

fn value_of(c: &Value) -> String {
    c.get("value").and_then(Value::as_str).unwrap_or("").to_string()
}

/// The cookies in a `Network.getAllCookies` answer.
pub fn cookies(answer: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|v| v.get("cookies")?.as_array().cloned())
        .unwrap_or_default()
}

/// A cookie as `Network.setCookies` takes it back: only the fields a
/// `CookieParam` has, and no expiry for a session cookie, which has none.
fn param(c: &Value) -> Option<Value> {
    let mut p = serde_json::Map::new();
    for k in ["name", "value", "domain", "path"] {
        p.insert(k.into(), c.get(k)?.clone());
    }
    for k in [
        "secure",
        "httpOnly",
        "sameSite",
        "priority",
        "sourceScheme",
        "sourcePort",
        "partitionKey",
    ] {
        if let Some(v) = c.get(k) {
            p.insert(k.into(), v.clone());
        }
    }
    let session = c.get("session").and_then(Value::as_bool).unwrap_or(false);
    if let Some(e) = c.get("expires").filter(|e| !session && e.as_f64().is_some_and(|e| e > 0.0)) {
        p.insert("expires".into(), e.clone());
    }
    Some(Value::Object(p))
}

/// The cookies of every site not in `sites`, as `Network.setCookies`
/// takes them.
pub fn inherited(all: &[Value], sites: &[String]) -> Vec<Value> {
    all.iter().filter(|c| !split(c, sites)).filter_map(param).collect()
}

/// What names each cookie, to remember which were copied.
pub fn keys(cookies: &[Value]) -> HashSet<String> {
    cookies.iter().map(key_of).collect()
}

fn removal(c: &Value) -> Value {
    let mut p = json!({
        "name": c.get("name").cloned().unwrap_or(Value::Null),
        "domain": c.get("domain").cloned().unwrap_or(Value::Null),
        "path": c.get("path").cloned().unwrap_or(Value::Null),
    });
    if let Some(k) = c.get("partitionKey") {
        p["partitionKey"] = k.clone();
    }
    p
}

/// The `Network.deleteCookies` calls that take every cookie of `sites`
/// out, as when a site is given back and the group's login must go.
pub fn removals(all: &[Value], sites: &[String]) -> Vec<Value> {
    all.iter().filter(|c| split(c, sites)).map(removal).collect()
}

/// The `Network.deleteCookies` calls that make the group profile follow
/// the shared one out: a cookie of an inherited site that was `copied`
/// from it before and that it no longer has, so a logout there reaches the
/// group. One the group set itself was never copied, so a login made in a
/// split pane on an inherited site stays. Split sites are never touched.
pub fn mirror(
    own: &[Value],
    shared: &[Value],
    copied: &HashSet<String>,
    sites: &[String],
) -> Vec<Value> {
    let theirs = keys(shared);
    own.iter()
        .filter(|c| !split(c, sites))
        .filter(|c| {
            let k = key_of(c);
            copied.contains(&k) && !theirs.contains(&k)
        })
        .map(removal)
        .collect()
}

/// The `Network.deleteCookies` calls that take out of a group profile, as
/// `sites` are split off, the cookies of those sites that are still the
/// shared profile's copies (same name, place and value): copied in before
/// the split, they would keep it signed in as everywhere else. A login
/// made in the group has values of its own and stays. Only at the split:
/// run on every copy, it would take a constant cookie (`logged_in=yes`)
/// of the group's own login each time.
pub fn stale_copies(own: &[Value], shared: &[Value], sites: &[String]) -> Vec<Value> {
    let theirs: HashMap<String, String> = shared.iter().map(|c| (key_of(c), value_of(c))).collect();
    own.iter()
        .filter(|c| split(c, sites) && theirs.get(&key_of(c)) == Some(&value_of(c)))
        .map(removal)
        .collect()
}

/// The params of one `Network.setCookies` call.
pub fn set_params(cookies: Vec<Value>) -> String {
    json!({ "cookies": cookies }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn registrable_domains() {
        assert_eq!(registrable("gist.github.com"), "github.com");
        assert_eq!(registrable("GitHub.com."), "github.com");
        assert_eq!(registrable("dash.cloudflare.com"), "cloudflare.com");
        assert_eq!(registrable("console.cloud.google.com"), "google.com");
        assert_eq!(registrable("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable("github.io"), "github.io");
        assert_eq!(registrable("acme.github.io"), "acme.github.io");
        assert_eq!(registrable("x.acme.vercel.app"), "acme.vercel.app");
        assert_eq!(registrable("localhost"), "localhost");
        assert_eq!(registrable("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn a_github_io_site_is_not_its_neighbour() {
        let acme = s(&[registrable("acme.github.io").as_str()]);
        assert!(split(&cookie("a", "acme.github.io", "1"), &acme));
        assert!(!split(&cookie("a", "evil.github.io", "1"), &acme));
        assert!(!split(&cookie("a", ".github.io", "1"), &acme));
    }

    #[test]
    fn site_of_urls() {
        assert_eq!(site_of("https://github.com/acme/x?y#z").as_deref(), Some("github.com"));
        assert_eq!(site_of("https://u:p@api.github.com:443/").as_deref(), Some("github.com"));
        assert_eq!(site_of("http://localhost:3000/").as_deref(), Some("localhost"));
        assert_eq!(site_of("http://[::1]:8080/").as_deref(), Some("[::1]"));
        assert_eq!(site_of("about:blank"), None);
        assert_eq!(site_of("file:///C:/x.html"), None);
    }

    #[test]
    fn an_ipv6_site_matches_its_cookies() {
        let site = site_of("http://[::1]:8080/").unwrap();
        assert!(split(&cookie("a", "[::1]", "1"), &[site]));
    }

    #[test]
    fn tools_follow_their_sites() {
        assert_eq!(tools_of("github.com"), &["gh", "git"]);
        assert_eq!(tools_of("cloudflare.com"), &["wrangler"]);
        assert_eq!(tools_of("azure.com"), &["az"]);
        assert_eq!(tools_of("google.com"), &["gcloud"]);
        assert_eq!(tools_of("npmjs.com"), &["npm"]);
        assert!(tools_of("example.com").is_empty());
    }

    #[test]
    fn cookie_domains() {
        assert!(in_site(".github.com", "github.com"));
        assert!(in_site("gist.github.com", "github.com"));
        assert!(in_site("GitHub.com", "github.com"));
        assert!(in_site(".GIST.GitHub.com", "github.com"));
        assert!(!in_site("notgithub.com", "github.com"));
        assert!(!in_site("github.com.evil.test", "github.com"));
    }

    #[test]
    fn azure_keeps_its_microsoft_login_out() {
        let azure = s(&["azure.com"]);
        assert!(split(&cookie("ESTSAUTH", "login.microsoftonline.com", "1"), &azure));
        assert!(split(&cookie("MSPAuth", ".login.live.com", "1"), &azure));
        assert!(!split(&cookie("x", "microsoft.com", "1"), &azure));
        assert!(!split(&cookie("ESTSAUTH", "login.microsoftonline.com", "1"), &s(&["github.com"])));
        assert_eq!(
            kept_out(&azure),
            s(&["azure.com", "login.microsoftonline.com", "login.live.com"])
        );
    }

    #[test]
    fn a_partitioned_cookie_under_a_split_site_is_split() {
        let mut c = cookie("embed", "widgets.test", "1");
        c["partitionKey"] = json!({"topLevelSite": "https://github.com", "hasCrossSiteAncestor": false});
        assert!(split(&c, &s(&["github.com"])));
        assert!(inherited(&[c.clone()], &s(&["github.com"])).is_empty());
        let mut old = cookie("embed", "widgets.test", "1");
        old["partitionKey"] = json!("https://gist.github.com");
        assert!(split(&old, &s(&["github.com"])));
        assert!(!split(&c, &s(&["vercel.com"])));
    }

    #[test]
    fn sites_added_once_and_removed() {
        let a = with_site(&s(&["github.com"]), "github.com");
        assert_eq!(a, s(&["github.com"]));
        let b = with_site(&a, "vercel.com");
        assert_eq!(without_site(&b, "github.com"), s(&["vercel.com"]));
    }

    #[test]
    fn offers_only_groups_that_split_the_site() {
        let g = |slug: &str, sites: Option<&[&str]>| Identity {
            name: slug.into(),
            slug: slug.into(),
            sites: sites.map(s),
            ..Default::default()
        };
        let all = vec![
            g("opti", Some(&["github.com"])),
            g("home", Some(&["vercel.com"])),
            g("old", None),
        ];
        let o: Vec<&str> = offers(&all, "github.com").iter().map(|i| i.slug.as_str()).collect();
        assert_eq!(o, ["opti"]);
    }

    fn cookie(name: &str, domain: &str, value: &str) -> Value {
        json!({"name": name, "value": value, "domain": domain, "path": "/",
               "expires": -1, "size": 3, "httpOnly": true, "secure": true,
               "session": true, "sameSite": "Lax", "priority": "Medium"})
    }

    #[test]
    fn inherited_leaves_split_sites_out() {
        let all = vec![
            cookie("user_session", "github.com", "a"),
            cookie("_gh", ".github.com", "b"),
            cookie("sid", ".vercel.com", "c"),
        ];
        let params = inherited(&all, &s(&["github.com"]));
        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["name"], "sid");
        // A session cookie has no expiry to give, and size is not a param.
        assert!(params[0].get("expires").is_none());
        assert!(params[0].get("size").is_none());
        assert!(params[0].get("session").is_none());
    }

    #[test]
    fn a_lasting_cookie_keeps_its_expiry() {
        let mut c = cookie("sid", "x.test", "v");
        c["session"] = json!(false);
        c["expires"] = json!(1_900_000_000.5);
        let params = inherited(&[c], &[]);
        assert_eq!(params[0]["expires"], json!(1_900_000_000.5));
    }

    #[test]
    fn mirror_follows_a_shared_logout_only() {
        let split_sites = s(&["github.com"]);
        let before = vec![cookie("sid", "x.test", "1"), cookie("keep", "y.test", "1")];
        let copied = keys(&inherited(&before, &split_sites));
        // The shared profile signed out of x.test since.
        let shared = vec![cookie("keep", "y.test", "1")];
        let own = vec![
            cookie("sid", "x.test", "1"),
            cookie("keep", "y.test", "1"),
            // Signed in here, never copied: stays.
            cookie("mine", "z.test", "1"),
            // A split site is never touched.
            cookie("user_session", "github.com", "other"),
        ];
        let gone = mirror(&own, &shared, &copied, &split_sites);
        assert_eq!(gone, vec![json!({"name": "sid", "domain": "x.test", "path": "/"})]);
    }

    #[test]
    fn stale_copies_are_only_the_shared_ones() {
        let shared = vec![cookie("user_session", "github.com", "main"), cookie("x", "x.test", "1")];
        let own = vec![
            cookie("user_session", "github.com", "main"),
            cookie("logged_in", "github.com", "yes"),
            cookie("x", "x.test", "1"),
        ];
        let r = stale_copies(&own, &shared, &s(&["github.com"]));
        assert_eq!(r, vec![json!({"name": "user_session", "domain": "github.com", "path": "/"})]);
        let fresh = vec![cookie("user_session", "github.com", "other")];
        assert!(stale_copies(&fresh, &shared, &s(&["github.com"])).is_empty());
    }

    #[test]
    fn removals_name_each_cookie_of_the_sites() {
        let mut part = cookie("p", "gist.github.com", "1");
        part["partitionKey"] = json!({"topLevelSite": "https://github.com", "hasCrossSiteAncestor": false});
        let all = vec![cookie("a", ".github.com", "1"), part, cookie("b", "x.test", "1")];
        let r = removals(&all, &s(&["github.com"]));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], json!({"name": "a", "domain": ".github.com", "path": "/"}));
        assert!(r[1].get("partitionKey").is_some());
    }

    #[test]
    fn env_points_only_split_tools() {
        let id = Identity {
            name: "Opti".into(),
            slug: "opti".into(),
            git: Some(Git {
                name: "A".into(),
                email: "a@acme.test".into(),
                github_user: None,
            }),
            ..Default::default()
        };
        let dir = Path::new("C:/ids/opti");
        let (set, remove) = env(&id, &s(&["github.com"]), dir, None);
        let names: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"GH_CONFIG_DIR"));
        assert!(names.contains(&"GIT_CONFIG_COUNT"));
        assert!(!names.contains(&"NPM_CONFIG_USERCONFIG"));
        assert!(!names.contains(&"XDG_DATA_HOME"));
        assert!(remove.contains(&"GH_TOKEN".to_string()));
        assert!(!remove.contains(&"NPM_TOKEN".to_string()));

        let (set, remove) = env(&id, &s(&["vercel.com", "azure.com"]), dir, None);
        let names: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["XDG_DATA_HOME", "AZURE_CONFIG_DIR"]);
        assert_eq!(remove, ["VERCEL_TOKEN"]);

        // Wrangler has no folder variable, so it stays the machine's.
        assert_eq!(env(&id, &s(&["cloudflare.com"]), dir, None), (vec![], vec![]));
    }

    #[test]
    fn git_uses_the_groups_gh_user_without_a_git_of_its_own() {
        let id = Identity::default();
        let dir = Path::new("C:/ids/opti");
        let (set, _) = env(&id, &s(&["github.com"]), dir, Some("acme-bot"));
        assert!(set.contains(&("GIT_CONFIG_KEY_0".into(), "credential.https://github.com.username".into())));
        assert!(set.contains(&("GIT_CONFIG_VALUE_0".into(), "acme-bot".into())));
        let (set, _) = env(&id, &s(&["github.com"]), dir, None);
        assert!(!set.iter().any(|(k, _)| k == "GIT_CONFIG_COUNT"));
    }
}
