//! The Claude accounts Horadric can switch between.
//!
//! Claude Code keeps one login. On Windows its token is in
//! `.credentials.json` under `claudeAiOauth`, beside the tokens of MCP
//! servers, and who it belongs to is in `.claude.json` under
//! `oauthAccount`. Everything else, the hooks, settings and conversations,
//! does not care which account is logged in. So an account is those two
//! pieces, kept aside, and switching puts another pair in their place and
//! leaves the rest of both files as it was.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::Agent;
use crate::usage::Usage;

/// What a session a usage limit stopped is told once it is back on
/// another account.
pub const GO_ON: &str = "The usage limit stopped your last turn, so Horadric \
    switched to another account. Go on where you left off.";

/// The key in `.credentials.json` that holds the Claude login.
const OAUTH: &str = "claudeAiOauth";
/// The organization the login is for, beside it.
const ORG: &str = "organizationUuid";
/// The key in `.claude.json` that says whose login it is.
const PROFILE: &str = "oauthAccount";

/// One account's login, kept while another is in use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Whose login it is. Missing reads as Claude's, so the accounts kept
    /// before there was a choice load. One another agent kept does not
    /// read, and [`Accounts`] drops it.
    #[serde(default)]
    pub agent: Agent,
    /// The account and organization, which together pick one login: the
    /// same person can be in two organizations with a plan each.
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub org_name: Option<String>,
    /// `claudeAiOauth`, as Claude Code wrote it.
    #[serde(default)]
    pub oauth: Value,
    /// `organizationUuid` beside it, when there was one.
    #[serde(default)]
    pub org: Option<Value>,
    /// `oauthAccount`, as Claude Code wrote it.
    #[serde(default)]
    pub profile: Value,
    /// Its limits as last heard, shown again when it comes back.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Until when, in Unix seconds, a limit that refused a turn holds it,
    /// though the numbers last heard may not show one full: a switch away
    /// from it does not come straight back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_until: Option<u64>,
}

impl Account {
    /// The login in `credentials` and `config`, the contents of
    /// `.credentials.json` and `.claude.json`. None without a Claude login,
    /// as with an API key, and None when the two disagree on the
    /// organization: a login caught half written, its token new and its
    /// profile still the old account's, which kept would give the old
    /// account the new one's token.
    pub fn capture(credentials: &Value, config: &Value) -> Option<Account> {
        let oauth = credentials.get(OAUTH).filter(|v| v.is_object())?;
        oauth.get("refreshToken")?.as_str()?;
        let profile = config.get(PROFILE).filter(|v| v.is_object())?;
        if let (Some(a), Some(b)) = (
            credentials.get(ORG).and_then(Value::as_str),
            profile.get(ORG).and_then(Value::as_str),
        ) {
            if a != b {
                return None;
            }
        }
        Some(Account {
            agent: Agent::Claude,
            id: identify(config)?,
            email: profile.get("emailAddress")?.as_str()?.to_string(),
            org_name: profile
                .get("organizationName")
                .and_then(Value::as_str)
                .map(str::to_string),
            oauth: oauth.clone(),
            org: credentials.get(ORG).cloned(),
            profile: profile.clone(),
            usage: None,
            out_until: None,
        })
    }

    /// Puts this login into `.credentials.json`'s contents, leaving the
    /// MCP servers' tokens and anything else there alone.
    pub fn put_credentials(&self, credentials: &mut Value) {
        if !credentials.is_object() {
            *credentials = Value::Object(Default::default());
        }
        let Some(map) = credentials.as_object_mut() else {
            return;
        };
        map.insert(OAUTH.into(), self.oauth.clone());
        match &self.org {
            Some(org) => map.insert(ORG.into(), org.clone()),
            None => map.remove(ORG),
        };
    }

    /// Puts whose login it is into `.claude.json`'s contents.
    pub fn put_profile(&self, config: &mut Value) {
        if let Some(map) = config.as_object_mut() {
            map.insert(PROFILE.into(), self.profile.clone());
        }
    }

    /// What the plan is called, "Max 5x", from the login's tier.
    pub fn plan(&self) -> Option<String> {
        let tier = self.oauth.get("rateLimitTier").and_then(Value::as_str);
        let kind = self.oauth.get("subscriptionType").and_then(Value::as_str);
        plan_name(kind, tier)
    }

    /// What a menu calls it: the address, then the plan and the
    /// organization when there is one worth naming.
    pub fn label(&self) -> String {
        let mut detail = Vec::new();
        if let Some(p) = self.plan() {
            detail.push(p);
        }
        if let Some(o) = self
            .org_name
            .as_deref()
            .filter(|o| !is_personal(o, &self.email))
        {
            detail.push(o.to_string());
        }
        match detail.is_empty() {
            true => self.email.clone(),
            false => format!("{}\t{}", self.email, detail.join(", ")),
        }
    }

    /// Its [`Account::label`] with what it has left at `now`, so a pick
    /// between accounts shows which has room.
    pub fn label_at(&self, now: u64) -> String {
        let left = match self.out_until.filter(|&t| t > now) {
            Some(_) => Some("used up".to_string()),
            None => self.usage.as_ref().and_then(|u| u.limits.left_text(now)),
        };
        match (left, self.label()) {
            (None, label) => label,
            (Some(left), label) if label.contains('\t') => format!("{label}, {left}"),
            (Some(left), label) => format!("{label}\t{left}"),
        }
    }

    /// What it has left at `now` of its fullest limit, in percent: none
    /// while a limit holds it, all of it when nothing was heard yet.
    pub fn room(&self, now: u64) -> f32 {
        if self.out_until.is_some_and(|t| t > now) {
            return 0.0;
        }
        self.usage
            .as_ref()
            .and_then(|u| u.limits.left(now))
            .unwrap_or(100.0)
    }
}

/// Who is logged in, by `.claude.json`'s contents: the account and the
/// organization.
pub fn identify(config: &Value) -> Option<String> {
    let profile = config.get(PROFILE)?;
    let account = profile.get("accountUuid")?.as_str()?;
    let org = profile
        .get("organizationUuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!("{account}:{org}"))
}

/// An organization Claude names after its one member says nothing.
fn is_personal(org: &str, email: &str) -> bool {
    org.contains(email) || org.ends_with("'s Organization") || org.ends_with("’s Organization")
}

/// "Max 20x" from `default_claude_max_20x`, "Pro" from `pro`.
fn plan_name(kind: Option<&str>, tier: Option<&str>) -> Option<String> {
    if let Some(n) = tier.and_then(|t| t.rsplit_once("max_")).map(|(_, n)| n) {
        return Some(format!("Max {n}"));
    }
    capitalised(kind?)
}

fn capitalised(word: &str) -> Option<String> {
    let mut c = word.chars();
    let first = c.next()?.to_uppercase();
    Some(first.chain(c).collect())
}

/// Every account logged in to so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default, deserialize_with = "readable")]
    pub list: Vec<Account>,
}

/// The accounts in a list that read, so an account another agent kept,
/// from when Horadric ran more than Claude, does not lose the rest.
fn readable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Account>, D::Error> {
    let items = Vec::<Value>::deserialize(d)?;
    Ok(items
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

impl Accounts {
    pub fn get(&self, id: &str) -> Option<&Account> {
        self.list.iter().find(|a| a.id == id)
    }

    /// Keeps `account`'s login as it is now, over what was kept for it,
    /// and its limits unless it brings some. A token Claude Code refreshed
    /// replaces the old one this way, which may no longer work. True when
    /// anything changed.
    pub fn remember(&mut self, mut account: Account) -> bool {
        match self.list.iter_mut().find(|a| a.id == account.id) {
            Some(kept) => {
                if account.usage.is_none() {
                    account.usage = kept.usage.clone();
                }
                if account.out_until.is_none() {
                    account.out_until = kept.out_until;
                }
                let changed = *kept != account;
                *kept = account;
                changed
            }
            None => {
                self.list.push(account);
                true
            }
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.list.retain(|a| a.id != id);
    }

    /// The account, other than `live`, with the most room at `now`, the
    /// first kept among equals. None when every other one is used up.
    pub fn roomiest(&self, live: Option<&str>, now: u64) -> Option<&Account> {
        self.list
            .iter()
            .filter(|a| Some(a.id.as_str()) != live)
            .map(|a| (a, a.room(now)))
            .filter(|(_, room)| *room > 0.0)
            .fold(
                None,
                |best: Option<(&Account, f32)>, (a, room)| match best {
                    Some((_, r)) if r >= room => best,
                    _ => Some((a, room)),
                },
            )
            .map(|(a, _)| a)
    }

    /// Notes that a limit refused account `id` a turn, until `until`.
    pub fn set_out(&mut self, id: &str, until: Option<u64>) {
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            a.out_until = until;
        }
    }

    /// Notes the limits last heard for account `id`.
    pub fn set_usage(&mut self, id: &str, usage: Option<Usage>) {
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            if usage.is_some() {
                a.usage = usage;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Limit, Limits};
    use serde_json::json;

    fn credentials(token: &str) -> Value {
        json!({
            "mcpOAuth": { "figma": { "accessToken": "keep" } },
            "claudeAiOauth": {
                "accessToken": format!("a-{token}"),
                "refreshToken": format!("r-{token}"),
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_5x"
            },
            "organizationUuid": format!("org-{token}")
        })
    }

    /// The profile for `credentials(token)`.
    fn config(token: &str, email: &str) -> Value {
        json!({
            "numStartups": 3,
            "oauthAccount": {
                "accountUuid": format!("u-{token}"),
                "organizationUuid": format!("org-{token}"),
                "emailAddress": email,
                "organizationName": format!("{email}'s Organization")
            }
        })
    }

    #[test]
    fn a_login_is_its_token_and_its_profile() {
        let a = Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap();
        assert_eq!(a.id, "u-one:org-one");
        assert_eq!(a.email, "a@x.dk");
        assert_eq!(a.oauth["refreshToken"], "r-one");
        assert_eq!(a.org, Some(json!("org-one")));
        assert_eq!(a.plan().as_deref(), Some("Max 5x"));
        assert_eq!(a.label(), "a@x.dk\tMax 5x");
    }

    #[test]
    fn no_login_without_a_claude_token_or_a_profile() {
        let key_only = json!({ "mcpOAuth": {} });
        assert!(Account::capture(&key_only, &config("one", "a@x.dk")).is_none());
        assert!(Account::capture(&credentials("one"), &json!({})).is_none());
    }

    #[test]
    fn switching_keeps_the_rest_of_both_files() {
        let b = Account::capture(&credentials("two"), &config("two", "b@x.dk")).unwrap();
        let mut creds = credentials("one");
        let mut conf = config("one", "a@x.dk");
        b.put_credentials(&mut creds);
        b.put_profile(&mut conf);
        assert_eq!(creds["mcpOAuth"]["figma"]["accessToken"], "keep");
        assert_eq!(creds["claudeAiOauth"]["refreshToken"], "r-two");
        assert_eq!(creds["organizationUuid"], "org-two");
        assert_eq!(conf["numStartups"], 3);
        assert_eq!(identify(&conf).as_deref(), Some("u-two:org-two"));
    }

    #[test]
    fn a_login_without_an_organization_takes_the_old_one_out() {
        let mut b = Account::capture(&credentials("two"), &config("two", "b@x.dk")).unwrap();
        b.org = None;
        let mut creds = credentials("one");
        b.put_credentials(&mut creds);
        assert!(creds.get("organizationUuid").is_none());
    }

    #[test]
    fn a_refreshed_token_replaces_the_kept_one_and_keeps_the_limits() {
        let mut all = Accounts::default();
        let mut a = Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap();
        a.usage = Some(Usage {
            limits: Limits {
                five_hour: Some(Limit {
                    used: 40.0,
                    resets_at: None,
                    minutes: None,
                }),
                ..Default::default()
            },
            at: 1,
        });
        assert!(all.remember(a));
        let mut refreshed = credentials("three");
        refreshed["organizationUuid"] = json!("org-one");
        let fresh = Account::capture(&refreshed, &config("one", "a@x.dk")).unwrap();
        assert!(all.remember(fresh.clone()));
        assert!(!all.remember(fresh));
        assert_eq!(all.list.len(), 1);
        let kept = all.get("u-one:org-one").unwrap();
        assert_eq!(kept.oauth["refreshToken"], "r-three");
        assert!(kept.usage.is_some());
    }

    #[test]
    fn a_login_caught_half_written_is_not_kept() {
        assert!(Account::capture(&credentials("two"), &config("one", "a@x.dk")).is_none());
    }

    #[test]
    fn plans_are_named_by_tier_then_by_kind() {
        assert_eq!(
            plan_name(Some("team"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(plan_name(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(plan_name(None, None), None);
    }

    #[test]
    fn accounts_kept_before_other_agents_load_as_claude() {
        let mut old = serde_json::to_value(
            Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap(),
        )
        .unwrap();
        old.as_object_mut().unwrap().remove("agent");
        let a: Account = serde_json::from_value(old).unwrap();
        assert_eq!(a.agent, Agent::Claude);
    }

    #[test]
    fn an_account_another_agent_kept_is_dropped_and_the_rest_load() {
        let claude = serde_json::to_value(
            Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap(),
        )
        .unwrap();
        let codex = json!({
            "agent": "codex", "id": "codex:acc:user", "email": "a@x.dk",
            "login": { "tokens": {} }
        });
        let all: Accounts = serde_json::from_value(json!({ "list": [codex, claude] })).unwrap();
        assert_eq!(all.list.len(), 1);
        assert_eq!(all.list[0].id, "u-one:org-one");
    }

    #[test]
    fn a_named_organization_shows_in_the_label() {
        let mut conf = config("one", "a@x.dk");
        conf["oauthAccount"]["organizationName"] = json!("Optipeople");
        let a = Account::capture(&credentials("one"), &conf).unwrap();
        assert_eq!(a.label(), "a@x.dk\tMax 5x, Optipeople");
    }

    fn with_usage(token: &str, session: f32, week: f32) -> Account {
        let mut a = Account::capture(
            &credentials(token),
            &config(token, &format!("{token}@x.dk")),
        )
        .unwrap();
        let limit = |used, resets_at| {
            Some(Limit {
                used,
                resets_at: Some(resets_at),
                minutes: None,
            })
        };
        a.usage = Some(Usage {
            limits: Limits {
                five_hour: limit(session, 500),
                seven_day: limit(week, 9000),
                spend: None,
            },
            at: 0,
        });
        a
    }

    #[test]
    fn the_label_says_what_is_left() {
        let a = with_usage("one", 38.0, 20.0);
        assert_eq!(
            a.label_at(100),
            "one@x.dk\tMax 5x, 62% session, 80% week left"
        );
        let mut out = a.clone();
        out.out_until = Some(600);
        assert_eq!(out.label_at(100), "one@x.dk\tMax 5x, used up");
        assert_eq!(
            out.label_at(700),
            "one@x.dk\tMax 5x, 100% session, 80% week left"
        );
    }

    #[test]
    fn the_roomiest_other_account_is_picked() {
        let mut all = Accounts::default();
        all.remember(with_usage("one", 100.0, 50.0));
        all.remember(with_usage("two", 70.0, 10.0));
        all.remember(with_usage("three", 20.0, 90.0));
        let pick = |all: &Accounts, live: &str, now| {
            all.roomiest(Some(live), now).map(|a| a.email.clone())
        };
        // Two has 30 left of its session, three 10 of its week.
        assert_eq!(
            pick(&all, "u-one:org-one", 100).as_deref(),
            Some("two@x.dk")
        );
        // Never the one in use, and a used up one is no pick.
        assert_eq!(
            pick(&all, "u-two:org-two", 100).as_deref(),
            Some("three@x.dk")
        );
        // Once the session windows start over, one has the most.
        assert_eq!(
            pick(&all, "u-two:org-two", 600).as_deref(),
            Some("one@x.dk")
        );
        all.set_out("u-one:org-one", Some(1000));
        assert_eq!(
            pick(&all, "u-two:org-two", 600).as_deref(),
            Some("three@x.dk")
        );
    }

    #[test]
    fn a_refreshed_login_keeps_its_limit_hold() {
        let mut all = Accounts::default();
        all.remember(with_usage("one", 10.0, 10.0));
        all.set_out("u-one:org-one", Some(1000));
        all.remember(Account::capture(&credentials("one"), &config("one", "one@x.dk")).unwrap());
        assert_eq!(all.get("u-one:org-one").unwrap().out_until, Some(1000));
    }

    #[test]
    fn an_account_never_heard_from_has_all_its_room() {
        let a = Account::capture(&credentials("one"), &config("one", "a@x.dk")).unwrap();
        assert_eq!(a.room(100), 100.0);
        assert_eq!(a.label_at(100), a.label());
    }
}
