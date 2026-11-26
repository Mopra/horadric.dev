//! Splitting a site off: a project signs in to one site as another
//! account while every other site stays as the shared browser has it. See
//! docs/plan/vault.md, "Seamless: nothing to set up".
//!
//! A split is kept on the group (an [`Identity`] with `sites`), never on
//! the project alone: the projects that took the same login share one
//! browser profile, so they must keep the same sites out of it, or one
//! project's copy of the shared cookies would overwrite another's split
//! login in the profile they share.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use super::identity::{git_env, Identity};

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
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then(|| registrable(host))
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
    d == site || d.ends_with(&format!(".{site}"))
}

fn split(domain: &str, sites: &[String]) -> bool {
    sites.iter().any(|s| in_site(domain, s))
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
pub fn env(id: &Identity, sites: &[String], dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
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
            "git" => {
                if let Some(git) = &id.git {
                    set.extend(git_env(git));
                }
            }
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

/// The cookies of every site not in `kept_out`, as `Network.setCookies`
/// takes them, and what each was, for [`changed`] to know later which of
/// them the other side changed.
pub fn inherited(all: &[Value], kept_out: &[String]) -> (Vec<Value>, Snapshot) {
    let mut out = Vec::new();
    let mut seen = Snapshot::new();
    for c in all {
        let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
        if split(domain, kept_out) {
            continue;
        }
        if let Some(p) = param(c) {
            seen.insert(key_of(c), value_of(c));
            out.push(p);
        }
    }
    (out, seen)
}

/// Each cookie's value by [`key_of`], as copied last.
pub type Snapshot = HashMap<String, String>;

fn value_of(c: &Value) -> String {
    c.get("value").and_then(Value::as_str).unwrap_or("").to_string()
}

/// The cookies of sites not in `kept_out` that are new or changed since
/// `last` was copied in, for copying back. One unchanged here is left
/// out: copying it back could put an old value over one the shared
/// profile rotated meanwhile, which signs some sites out.
pub fn changed(all: &[Value], kept_out: &[String], last: &Snapshot) -> Vec<Value> {
    all.iter()
        .filter(|c| {
            let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
            !split(domain, kept_out) && last.get(&key_of(c)) != Some(&value_of(c))
        })
        .filter_map(param)
        .collect()
}

/// The `Network.deleteCookies` calls that take every cookie of `sites`
/// out, as when a site is split off and its inherited login must go.
pub fn removals(all: &[Value], sites: &[String]) -> Vec<Value> {
    all.iter()
        .filter(|c| split(c.get("domain").and_then(Value::as_str).unwrap_or(""), sites))
        .map(|c| {
            let mut p = json!({
                "name": c.get("name").cloned().unwrap_or(Value::Null),
                "domain": c.get("domain").cloned().unwrap_or(Value::Null),
                "path": c.get("path").cloned().unwrap_or(Value::Null),
            });
            if let Some(k) = c.get("partitionKey") {
                p["partitionKey"] = k.clone();
            }
            p
        })
        .collect()
}

/// The `Network.deleteCookies` calls that take out of a split profile
/// the cookies of its split sites that are still the shared profile's
/// copies (same name, place and value): copied in before the site was
/// split, they would keep it signed in as everywhere else. A login made
/// in the split profile has a value of its own and stays.
pub fn stale_copies(own: &[Value], shared: &[Value], kept_out: &[String]) -> Vec<Value> {
    let theirs: Snapshot = shared.iter().map(|c| (key_of(c), value_of(c))).collect();
    let copies: Vec<Value> = own
        .iter()
        .filter(|c| theirs.get(&key_of(c)) == Some(&value_of(c)))
        .cloned()
        .collect();
    removals(&copies, kept_out)
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
        assert_eq!(registrable("acme.github.io"), "acme.github.io");
        assert_eq!(registrable("x.acme.vercel.app"), "acme.vercel.app");
        assert_eq!(registrable("localhost"), "localhost");
        assert_eq!(registrable("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn site_of_urls() {
        assert_eq!(site_of("https://github.com/acme/x?y#z").as_deref(), Some("github.com"));
        assert_eq!(site_of("https://u:p@api.github.com:443/").as_deref(), Some("github.com"));
        assert_eq!(site_of("http://localhost:3000/").as_deref(), Some("localhost"));
        assert_eq!(site_of("http://[::1]:8080/").as_deref(), Some("::1"));
        assert_eq!(site_of("about:blank"), None);
        assert_eq!(site_of("file:///C:/x.html"), None);
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
        assert!(!in_site("notgithub.com", "github.com"));
        assert!(!in_site("github.com.evil.test", "github.com"));
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
        let (params, seen) = inherited(&all, &s(&["github.com"]));
        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["name"], "sid");
        // A session cookie has no expiry to give, and size is not a param.
        assert!(params[0].get("expires").is_none());
        assert!(params[0].get("size").is_none());
        assert!(params[0].get("session").is_none());
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn a_lasting_cookie_keeps_its_expiry() {
        let mut c = cookie("sid", "x.test", "v");
        c["session"] = json!(false);
        c["expires"] = json!(1_900_000_000.5);
        let (params, _) = inherited(&[c], &[]);
        assert_eq!(params[0]["expires"], json!(1_900_000_000.5));
    }

    #[test]
    fn only_changed_cookies_go_back() {
        let before = vec![cookie("a", "x.test", "1"), cookie("b", "x.test", "1")];
        let (_, seen) = inherited(&before, &[]);
        let now = vec![
            cookie("a", "x.test", "1"),
            cookie("b", "x.test", "2"),
            cookie("c", "x.test", "1"),
            cookie("user_session", "github.com", "other account"),
        ];
        let back = changed(&now, &s(&["github.com"]), &seen);
        let names: Vec<&str> = back.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["b", "c"]);
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
    fn env_points_only_split_tools() {
        let id = Identity {
            name: "Opti".into(),
            slug: "opti".into(),
            git: Some(crate::vault::identity::Git {
                name: "A".into(),
                email: "a@acme.test".into(),
                github_user: None,
            }),
            ..Default::default()
        };
        let dir = Path::new("C:/ids/opti");
        let (set, remove) = env(&id, &s(&["github.com"]), dir);
        let names: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"GH_CONFIG_DIR"));
        assert!(names.contains(&"GIT_CONFIG_COUNT"));
        assert!(!names.contains(&"NPM_CONFIG_USERCONFIG"));
        assert!(!names.contains(&"XDG_DATA_HOME"));
        assert!(remove.contains(&"GH_TOKEN".to_string()));
        assert!(!remove.contains(&"NPM_TOKEN".to_string()));

        let (set, remove) = env(&id, &s(&["vercel.com", "azure.com"]), dir);
        let names: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["XDG_DATA_HOME", "AZURE_CONFIG_DIR"]);
        assert_eq!(remove, ["VERCEL_TOKEN"]);

        // Wrangler has no folder variable, so it stays the machine's.
        assert_eq!(env(&id, &s(&["cloudflare.com"]), dir), (vec![], vec![]));
    }
}
