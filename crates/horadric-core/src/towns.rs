//! Towns: the places above the projects, for work that belongs to none.
//! Tristram is the first and stays; the human adds others, a work town and
//! a private one, each able to run on a Claude login and a browser profile
//! of its own so the two never see each other's mail. See docs/plan/towns.md.
//!
//! A town is an ordinary project keyed by its folder. What makes it a town
//! is only that it is in this list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The first town's slug, whose folder is where Tristram always was.
pub const TRISTRAM: &str = "tristram";

/// The variable Claude Code reads its home from instead of `~/.claude`.
pub const CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

/// The folder inside a town that is its Claude home. The folder being
/// there is what gives the town a login of its own: the hooks installer,
/// the transcript lookups and the sessions all go by it, so no process has
/// to read the app's state to know.
pub const HOME_DIR: &str = "claude";

/// A town as the state keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SavedTown {
    pub name: String,
    /// Its folder's name, fixed when the town is made so a rename moves
    /// nothing.
    pub slug: String,
    /// Its tile is hidden. It shows again while a session runs there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Its browser pane keeps its own logins. Off for Tristram as it came
    /// from before towns, so the logins it has stay where they are.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_browser: bool,
    /// What the town is for, in the human's words, for its CLAUDE.md.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
    /// The claude.ai connectors its account has, so its agents do not guess.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors: Vec<String>,
}

impl SavedTown {
    /// The WebView2 profile its browser pane opens in, None for the shared
    /// one every project uses.
    pub fn profile(&self) -> Option<String> {
        self.own_browser.then(|| format!("town-{}", self.slug))
    }
}

/// The towns a state holds, Tristram first. A file from before towns had
/// one town and a `town_hidden` flag for it.
pub fn migrate(mut towns: Vec<SavedTown>, town_hidden: bool) -> Vec<SavedTown> {
    let mut seen = std::collections::HashSet::new();
    towns.retain(|t| !t.slug.is_empty() && seen.insert(t.slug.clone()));
    match towns.iter().position(|t| t.slug == TRISTRAM) {
        Some(0) => {}
        Some(i) => {
            let t = towns.remove(i);
            towns.insert(0, t);
        }
        None => towns.insert(
            0,
            SavedTown {
                name: "Tristram".into(),
                slug: TRISTRAM.into(),
                hidden: town_hidden,
                ..Default::default()
            },
        ),
    }
    towns
}

/// A folder name for a town called `name`, not one of `taken`: lower case
/// letters and digits joined by single hyphens.
pub fn slug_for(name: &str, taken: &[&str]) -> String {
    let mut base = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            base.push(c);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = match base.trim_end_matches('-') {
        "" => "town".to_string(),
        b => b.to_string(),
    };
    let free = |s: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(s));
    if free(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| free(s))
        .expect("some number is free")
}

/// Where the town with `slug` lives under `base`, the instance's own
/// folder in the user's home (`Horadric` or `Horadric-dev`). Tristram stays
/// where it always was, the rest go under `towns`.
pub fn folder(base: &Path, slug: &str) -> PathBuf {
    match slug {
        TRISTRAM => base.join(TRISTRAM),
        _ => base.join("towns").join(slug),
    }
}

/// The Claude home a town in `folder` has when it has one.
pub fn home_in(folder: &Path) -> PathBuf {
    folder.join(HOME_DIR)
}

/// Every place a town's Claude home can be under `base`: Tristram's and one
/// per folder in `towns`, named by `entries`, the folder names found there.
pub fn possible_homes(base: &Path, entries: &[String]) -> Vec<PathBuf> {
    std::iter::once(home_in(&folder(base, TRISTRAM)))
        .chain(
            entries
                .iter()
                .filter(|e| e.as_str() != TRISTRAM)
                .map(|e| home_in(&folder(base, e))),
        )
        .collect()
}

/// The town home among `homes` that `cwd` works under, if any: a session in
/// a town folder or below it runs on that town's login.
pub fn home_for<'a>(cwd: &str, homes: &'a [PathBuf]) -> Option<&'a PathBuf> {
    let norm = |p: &str| {
        let s = p.replace('\\', "/").trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    let cwd = norm(cwd);
    homes.iter().find(|h| {
        let Some(town) = h.parent() else {
            return false;
        };
        let town = norm(&town.to_string_lossy());
        !town.is_empty()
            && (cwd == town || cwd.strip_prefix(&town).is_some_and(|r| r.starts_with('/')))
    })
}

/// What a session in a town with `home` sets in its environment.
pub fn env(home: Option<&Path>) -> Vec<(String, String)> {
    home.map(|h| (CONFIG_ENV.to_string(), h.to_string_lossy().into_owned()))
        .into_iter()
        .collect()
}

/// The part of a town's generated CLAUDE.md that says what the town is.
pub fn about_lines(town: &SavedTown, own_login: bool) -> String {
    let mut out = String::new();
    if !town.about.trim().is_empty() {
        out.push_str(&format!("{}\n\n", town.about.trim()));
    }
    out.push_str(match own_login {
        true => "This town runs on a Claude account of its own, not the one the projects use.\n\n",
        false => "This town runs on the same Claude account as the projects.\n\n",
    });
    out.push_str("## Connectors\n\n");
    if town.connectors.is_empty() {
        out.push_str(
            "None listed. Do not assume a connector is there: ask before you rely on one.\n\n",
        );
    } else {
        for c in &town.connectors {
            out.push_str(&format!("- {c}\n"));
        }
        out.push_str(
            "\nThese are the claude.ai connectors this town's account has. Use them for \
             mail, chat and tickets; anything else is in the browser pane.\n\n",
        );
    }
    out
}

/// A list typed in one line, by commas, as the connectors are.
pub fn split_list(typed: &str) -> Vec<String> {
    typed
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn town(slug: &str) -> SavedTown {
        SavedTown {
            name: slug.into(),
            slug: slug.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_state_from_before_towns_gets_tristram_with_its_hidden_flag() {
        let towns = migrate(Vec::new(), true);
        assert_eq!(towns.len(), 1);
        assert_eq!(towns[0].slug, TRISTRAM);
        assert_eq!(towns[0].name, "Tristram");
        assert!(towns[0].hidden);
        assert_eq!(towns[0].profile(), None);
    }

    #[test]
    fn tristram_comes_first_and_doubles_go() {
        let towns = migrate(
            vec![town("acme"), town("tristram"), town("acme"), town("")],
            false,
        );
        let slugs: Vec<&str> = towns.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, ["tristram", "acme"]);
        assert!(!towns[0].hidden);
    }

    #[test]
    fn a_kept_list_ignores_the_old_flag() {
        let mut t = town("tristram");
        t.hidden = false;
        assert!(!migrate(vec![t], true)[0].hidden);
    }

    #[test]
    fn slugs_are_folder_names_and_unique() {
        assert_eq!(slug_for("Acme Work", &[]), "acme-work");
        assert_eq!(slug_for("  Globex / private!  ", &[]), "globex-private");
        assert_eq!(slug_for("???", &[]), "town");
        assert_eq!(slug_for("Tristram", &["tristram"]), "tristram-2");
        assert_eq!(slug_for("acme", &["acme", "Acme-2"]), "acme-3");
    }

    #[test]
    fn tristram_stays_put_and_the_rest_go_under_towns() {
        let base = Path::new("home").join("Horadric");
        assert_eq!(folder(&base, TRISTRAM), base.join("tristram"));
        assert_eq!(folder(&base, "acme"), base.join("towns").join("acme"));
        assert_eq!(
            home_in(&folder(&base, "acme")),
            base.join("towns").join("acme").join("claude")
        );
        let homes = possible_homes(&base, &["acme".into(), "tristram".into()]);
        assert_eq!(
            homes,
            [
                base.join("tristram").join("claude"),
                base.join("towns").join("acme").join("claude"),
            ]
        );
    }

    #[test]
    fn a_session_under_a_town_gets_its_home_and_others_none() {
        let homes = vec![
            PathBuf::from("C:/Users/x/Horadric/tristram/claude"),
            PathBuf::from("C:/Users/x/Horadric/towns/acme/claude"),
        ];
        assert_eq!(
            home_for(r"C:\Users\x\Horadric\towns\acme", &homes),
            Some(&homes[1])
        );
        assert_eq!(
            home_for(r"C:\Users\x\Horadric\towns\acme\notes\", &homes),
            Some(&homes[1])
        );
        assert_eq!(home_for(r"C:\Users\x\Horadric\towns\acme-2", &homes), None);
        assert_eq!(home_for(r"C:\work\app", &homes), None);
        if cfg!(windows) {
            assert_eq!(
                home_for(r"c:\users\X\horadric\TRISTRAM", &homes),
                Some(&homes[0])
            );
        }
    }

    #[test]
    fn a_town_with_a_home_sets_the_config_dir_and_one_without_sets_nothing() {
        let h = Path::new("t").join("claude");
        assert_eq!(
            env(Some(&h)),
            [(CONFIG_ENV.to_string(), h.to_string_lossy().into_owned())]
        );
        assert!(env(None).is_empty());
    }

    #[test]
    fn a_town_with_its_own_browser_has_a_profile_by_its_slug() {
        let mut t = town("acme");
        assert_eq!(t.profile(), None);
        t.own_browser = true;
        assert_eq!(t.profile().as_deref(), Some("town-acme"));
    }

    #[test]
    fn the_brief_lists_connectors_or_says_there_are_none() {
        let mut t = town("acme");
        t.about = "Work for Acme.".into();
        let none = about_lines(&t, false);
        assert!(none.starts_with("Work for Acme."));
        assert!(none.contains("same Claude account"));
        assert!(none.contains("None listed"));
        t.connectors = split_list("Microsoft 365, Slack;; Atlassian ");
        assert_eq!(t.connectors, ["Microsoft 365", "Slack", "Atlassian"]);
        let some = about_lines(&t, true);
        assert!(some.contains("account of its own"));
        assert!(some.contains("- Slack\n"));
    }
}
