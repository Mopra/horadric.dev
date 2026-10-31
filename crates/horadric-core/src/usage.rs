//! What Claude Code says about usage, and the defaults Horadric starts its
//! sessions with.
//!
//! No hook carries usage. Claude Code hands it to the status line command
//! instead, as JSON on stdin after every reply: the model, how full the
//! context is, and for a subscription the five hour and weekly limits. A
//! session Horadric starts gets `horadric status` as its status line, which
//! passes that JSON on to the app. The limits belong to the account, not to
//! the session, so every session's reading is taken in, the truest kept.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::Agent;

/// One usage limit: how much of it is used, and when it starts over.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    /// Percent, 0 to 100, past 100 once a spend limit is overrun.
    pub used: f32,
    /// Unix seconds.
    #[serde(default)]
    pub resets_at: Option<u64>,
    /// How long the window is, when the source says, which then names it
    /// over its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

impl Limit {
    /// How much is used at `now`, in Unix seconds, and how many seconds
    /// until it starts over. A limit whose reset has passed is empty, though
    /// no reply has said so yet.
    pub fn at(&self, now: u64) -> (f32, Option<u64>) {
        match self.resets_at {
            Some(t) if t <= now => (0.0, None),
            Some(t) => (self.used, Some(t - now)),
            None => (self.used, None),
        }
    }

    /// The truer of two readings of the same limit, `self` held and
    /// `heard` just in. Every session repeats the numbers of its own last
    /// reply each time its status line runs, so one idle for an hour still
    /// speaks of an hour ago. Within one window use only grows, so the
    /// higher reading is the newer. A reading whose reset has passed at
    /// `now` is of a window already over, and says nothing of this one.
    fn merge(self, heard: Limit, now: u64) -> Limit {
        let (Some(held_at), Some(heard_at)) = (self.resets_at, heard.resets_at) else {
            return heard;
        };
        if heard_at.abs_diff(held_at) <= SAME_WINDOW {
            if heard.used >= self.used {
                heard
            } else {
                self
            }
        } else if heard_at <= now && held_at > now {
            self
        } else {
            heard
        }
    }

    fn from_json(v: &Value) -> Option<Limit> {
        Some(Limit {
            used: v.get("used_percentage")?.as_f64()? as f32,
            resets_at: v.get("resets_at").and_then(Value::as_u64),
            minutes: None,
        })
    }

    /// What a window of this length is called, when it is one a person
    /// would name.
    fn window_name(&self) -> Option<&'static str> {
        match self.minutes? {
            0..=360 => Some("Session"),
            1_380..=1_500 => Some("Day"),
            9_000..=11_000 => Some("Week"),
            40_000..=46_000 => Some("Month"),
            _ => None,
        }
    }
}

/// The account's limits. Each can be missing: an API key has none, and
/// the spend limit is only there for plans that have one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub five_hour: Option<Limit>,
    #[serde(default)]
    pub seven_day: Option<Limit>,
    #[serde(default)]
    pub spend: Option<Limit>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none() && self.spend.is_none()
    }

    /// The held limits with `heard` taken in, each limit the truer of its
    /// two readings at `now`, see [`Limit::merge`]. One `heard` does not
    /// name is kept as it was.
    pub fn merge(&self, heard: &Limits, now: u64) -> Limits {
        let one = |held: Option<Limit>, heard: Option<Limit>| match (held, heard) {
            (Some(a), Some(b)) => Some(a.merge(b, now)),
            (a, b) => b.or(a),
        };
        Limits {
            five_hour: one(self.five_hour, heard.five_hour),
            seven_day: one(self.seven_day, heard.seven_day),
            spend: one(self.spend, heard.spend),
        }
    }

    /// Each limit with what to call it, the ones there are.
    pub fn named(&self) -> Vec<(&'static str, Limit)> {
        [
            ("Session", self.five_hour),
            ("Week", self.seven_day),
            ("Spend", self.spend),
        ]
        .into_iter()
        .filter_map(|(name, l)| Some((name, l?)))
        .map(|(name, l)| (l.window_name().unwrap_or(name), l))
        .collect()
    }

    /// The limits in a headless CLI's `rate_limit_event`, from its
    /// `unifiedWindows`, where use is a fraction and not a percent. An
    /// older CLI's event names only which limit applies, and tells nothing.
    pub fn from_rate_limit_event(v: &Value) -> Option<Limits> {
        let windows = v.pointer("/rate_limit_info/unifiedWindows")?;
        let limit = |name: &str| {
            let w = windows.get(name)?;
            Some(Limit {
                used: (w.get("utilization")?.as_f64()? * 100.0) as f32,
                resets_at: w.get("resetsAt").and_then(Value::as_u64),
                minutes: None,
            })
        };
        let limits = Limits {
            five_hour: limit("five_hour"),
            seven_day: limit("seven_day"),
            spend: None,
        };
        (!limits.is_empty()).then_some(limits)
    }

    /// When the account can work again, in Unix seconds, if a limit is
    /// used up at `now`: the latest reset among the full ones, since
    /// nothing runs until every one of them starts over. With `hit`, a
    /// session was just refused for its limit, though the numbers last
    /// heard may not show one full yet, so the fullest limit still to
    /// reset is taken as the one that ran out. None when no reset is known.
    pub fn out_until(&self, now: u64, hit: bool) -> Option<u64> {
        let pending: Vec<(f32, u64)> = [self.five_hour, self.seven_day, self.spend]
            .into_iter()
            .flatten()
            .filter_map(|l| Some((l.used, l.resets_at.filter(|&t| t > now)?)))
            .collect();
        let full = pending.iter().filter(|(used, _)| *used >= 100.0);
        if let Some(t) = full.map(|&(_, t)| t).max() {
            return Some(t);
        }
        if !hit {
            return None;
        }
        pending
            .iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|&(_, t)| t)
    }
    /// How much is left at `now` of the fullest limit, in percent, which
    /// is what an account has to work with. None when no limit is known.
    pub fn left(&self, now: u64) -> Option<f32> {
        self.named()
            .into_iter()
            .map(|(_, l)| (100.0 - l.at(now).0).max(0.0))
            .min_by(f32::total_cmp)
    }

    /// What is left of each limit at `now`, "62% session, 80% week left",
    /// for a menu to set accounts side by side.
    pub fn left_text(&self, now: u64) -> Option<String> {
        let parts: Vec<String> = self
            .named()
            .into_iter()
            .map(|(name, l)| {
                let left = (100.0 - l.at(now).0).clamp(0.0, 100.0);
                format!("{left:.0}% {}", name.to_lowercase())
            })
            .collect();
        (!parts.is_empty()).then(|| format!("{} left", parts.join(", ")))
    }

    /// The fullest limit at `now`, with its name, when it is at
    /// `PACE_AT` or more: the runner starts nothing then, rather than learn
    /// at 100 % that a session it started cannot finish its turn. A limit
    /// whose reset has passed is empty again.
    pub fn too_full(&self, now: u64) -> Option<(&'static str, f32)> {
        self.named()
            .into_iter()
            .map(|(name, l)| (name, l.at(now).0))
            .filter(|&(_, used)| used >= PACE_AT)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// How far apart, in seconds, two readings' resets may be and still be of
/// one window. Far less than the shortest window, so a new window is never
/// taken for the old one.
const SAME_WINDOW: u64 = 15 * 60;

/// How full, in percent, a limit may be before the runner starts no more.
pub const PACE_AT: f32 = 90.0;

/// The limits as last heard, and when, in Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub limits: Limits,
    pub at: u64,
}

impl Usage {
    /// The held usage with a reading from elsewhere taken in, as
    /// [`Limits::merge`] does, heard when the newer of the two was.
    pub fn merge(&self, heard: &Usage, now: u64) -> Usage {
        Usage {
            limits: self.limits.merge(&heard.limits, now),
            at: self.at.max(heard.at),
        }
    }
}

/// One account's limits as a brother tells them: the account in use there
/// for `agent`, by the id every machine knows it by (see
/// `crate::accounts::identify`), when the brother knows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Told {
    pub agent: Agent,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    pub usage: Usage,
}

/// The `usage` message a brother sends: its accounts' limits, one per
/// agent. A limit belongs to the account, so a human working on a
/// brother's sessions sees it run down here, though no session here spoke.
pub fn usage_message(told: &[Told]) -> Value {
    serde_json::json!({"t": "usage", "list": told})
}

/// What a `usage` message tells, the entries it can read.
pub fn told(msg: &Value) -> Vec<Told> {
    msg.get("list")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|t| serde_json::from_value::<Told>(t.clone()).ok())
                .filter(|t| !t.usage.limits.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// What one status line call said about its session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The model's name as Claude Code shows it, "Opus 5.5".
    pub model: Option<String>,
    /// How full the context window is, in percent.
    pub context: Option<f32>,
    pub limits: Limits,
}

impl Status {
    /// Reads the status line's input. Everything is optional: early in a
    /// session the context is not measured yet and the limits come after
    /// the first reply.
    pub fn from_json(body: &[u8]) -> Option<Status> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let limits = v.get("rate_limits");
        let limit = |name: &str| limits.and_then(|l| l.get(name)).and_then(Limit::from_json);
        Some(Status {
            model: v
                .pointer("/model/display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            context: v
                .pointer("/context_window/used_percentage")
                .and_then(Value::as_f64)
                .map(|p| p as f32),
            limits: Limits {
                five_hour: limit("five_hour"),
                seven_day: limit("seven_day"),
                spend: limit("spend_limit"),
            },
        })
    }

    /// What the status line prints in the terminal: the model, the context
    /// and the five hour limit, which is the one that runs out first.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(m) = &self.model {
            parts.push(m.clone());
        }
        if let Some(c) = self.context {
            parts.push(format!("context {}%", c.round()));
        }
        if let Some(l) = self.limits.five_hour {
            parts.push(format!("session {}%", l.used.round()));
        }
        parts.join(" \u{00B7} ")
    }
}

/// "40 min", "2 h 10 min", "3 d 4 h". Only as fine as a reset needs.
pub fn format_until(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{} min", m.max(1)),
        (0, h) => format!("{h} h {m:02} min"),
        (d, h) => format!("{d} d {h} h"),
    }
}

/// A setting Horadric passes to every `claude` it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Model,
    Effort,
    Permissions,
    /// The model of every agent a workflow spawns, Claude Code's
    /// `CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL`. Not in [`Setting::ALL`]:
    /// it is no flag and no running session can switch it, so the usage
    /// window leaves it to Settings.
    Workflow,
}

/// What Claude Code reads the workflow agents' model from.
pub const WORKFLOW_MODEL_ENV: &str = "CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL";

impl Setting {
    pub const ALL: [Setting; 3] = [Setting::Model, Setting::Effort, Setting::Permissions];

    pub fn label(self) -> &'static str {
        match self {
            Setting::Model => "Model",
            Setting::Effort => "Effort",
            Setting::Permissions => "Permissions",
            Setting::Workflow => "Workflow agents' model",
        }
    }

    /// What it takes, and what the window calls it. Models by their
    /// full names, so the version picked is the version that runs, and an
    /// alias moving on to a newer model never changes it behind your back.
    /// The last permission mode sits apart in its list, so it is never
    /// picked by a slip.
    pub fn choices(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Setting::Model | Setting::Workflow => &[
                ("claude-fable-5-1", "Fable 5.1"),
                ("claude-opus-5-5", "Opus 5.5"),
                ("claude-opus-5-5[1m]", "Opus 5.5 1M"),
                ("claude-sonnet-5", "Sonnet 5"),
                ("claude-haiku-4-5-20251001", "Haiku 4.5"),
            ],
            Setting::Effort => &[
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
                ("max", "Max"),
            ],
            Setting::Permissions => &[
                ("manual", "Ask first"),
                ("acceptEdits", "Accept edits"),
                ("plan", "Plan"),
                ("auto", "Auto"),
                ("dontAsk", "Don't ask"),
                ("bypassPermissions", "Bypass permissions"),
            ],
        }
    }

    /// Effort is a scale, so the window shows it as a slider. The others
    /// are lists.
    pub fn is_scale(self) -> bool {
        self == Setting::Effort
    }

    /// What to type into a running `claude` to switch it to `value`, None
    /// for the default. Only model and effort have a command. Both switch
    /// this session alone and leave Claude Code's own settings be. The
    /// permission mode has none, so it waits for the next start.
    pub fn command(self, value: Option<&str>) -> Option<String> {
        match self {
            Setting::Model => Some(format!("/model {}", value.unwrap_or("default"))),
            Setting::Effort => Some(format!("/effort {}", value.unwrap_or("auto"))),
            Setting::Permissions | Setting::Workflow => None,
        }
    }

    /// Whether a session started with `args` chose this setting itself,
    /// so the defaults leave it alone.
    pub fn chosen_by(self, args: &[String]) -> bool {
        Agent::Claude.chosen(self, args)
    }

    /// What a value is called, or "Default" for none, which leaves it to
    /// the agent's own settings.
    pub fn name_of(self, value: Option<&str>) -> &'static str {
        let Some(value) = value else {
            return "Default";
        };
        self.choices()
            .iter()
            .find(|(v, _)| *v == value)
            .map_or("Custom", |(_, name)| name)
    }
}

/// What every session Horadric starts or resumes gets, unless its own
/// arguments say otherwise. None leaves a setting to Claude Code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub workflow_model: Option<String>,
}

impl Defaults {
    pub fn get(&self, s: Setting) -> Option<&str> {
        match s {
            Setting::Model => self.model.as_deref(),
            Setting::Effort => self.effort.as_deref(),
            Setting::Permissions => self.permission_mode.as_deref(),
            Setting::Workflow => self.workflow_model.as_deref(),
        }
    }

    pub fn set(&mut self, s: Setting, value: Option<String>) {
        match s {
            Setting::Model => self.model = value,
            Setting::Effort => self.effort = value,
            Setting::Permissions => self.permission_mode = value,
            Setting::Workflow => self.workflow_model = value,
        }
    }

    /// The environment a session starts with. None set leaves the
    /// variable to whatever the user's own environment says.
    pub fn env(&self) -> Vec<(String, String)> {
        self.workflow_model
            .iter()
            .map(|m| (WORKFLOW_MODEL_ENV.into(), m.clone()))
            .collect()
    }

    /// The flags to put before a session's own `args`. A setting its
    /// arguments already make is left to them.
    pub fn flags(&self, args: &[String]) -> Vec<String> {
        let agent = Agent::Claude;
        let mut out = Vec::new();
        for s in Setting::ALL {
            let Some(value) = self.get(s) else {
                continue;
            };
            if agent.chosen(s, args) {
                continue;
            }
            out.extend(agent.setting_args(s, value).unwrap_or_default());
        }
        out
    }

    /// `flags`, with permission prompts bypassed in place of the mode
    /// picked when `bypass`. A mode the session's own `args` chose still
    /// wins.
    pub fn flags_for(&self, args: &[String], bypass: bool) -> Vec<String> {
        if !bypass {
            return self.flags(args);
        }
        let agent = Agent::Claude;
        let others = Defaults {
            permission_mode: None,
            ..self.clone()
        };
        let mut out = others.flags(args);
        if !agent.chosen(Setting::Permissions, args) {
            out.extend(agent.bypass_args());
        }
        out
    }
}

/// Whether `args` hold `flag`, alone or as `flag=value`.
pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| {
        a == flag
            || a.strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &[u8] = br#"{
        "session_id": "abc",
        "model": {"id": "claude-opus-5-5", "display_name": "Opus 5.5"},
        "cost": {"total_cost_usd": 0.42},
        "context_window": {"used_percentage": 23.4, "context_window_size": 200000},
        "rate_limits": {
            "five_hour": {"used_percentage": 41.2, "resets_at": 1738425600},
            "seven_day": {"used_percentage": 12, "resets_at": 1738857600},
            "unknown_window": {"used_percentage": 1}
        }
    }"#;

    #[test]
    fn what_is_left_is_the_fullest_limits_rest() {
        let l = Limits {
            five_hour: Some(Limit {
                used: 38.0,
                resets_at: Some(500),
                minutes: None,
            }),
            seven_day: Some(Limit {
                used: 20.0,
                resets_at: Some(9000),
                minutes: None,
            }),
            spend: None,
        };
        assert_eq!(l.left(100), Some(62.0));
        assert_eq!(
            l.left_text(100).as_deref(),
            Some("62% session, 80% week left")
        );
        // The session window has started over since.
        assert_eq!(l.left(600), Some(80.0));
        assert_eq!(Limits::default().left(100), None);
        assert_eq!(Limits::default().left_text(100), None);
    }

    #[test]
    fn reads_the_status_line_input() {
        let s = Status::from_json(FULL).unwrap();
        assert_eq!(s.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(s.context, Some(23.4));
        assert_eq!(
            s.limits.five_hour,
            Some(Limit {
                used: 41.2,
                resets_at: Some(1738425600),
                minutes: None,
            })
        );
        assert_eq!(s.limits.seven_day.map(|l| l.used), Some(12.0));
        assert_eq!(s.limits.spend, None);
        let names: Vec<&str> = s.limits.named().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Session", "Week"]);
    }

    #[test]
    fn early_input_has_nothing_measured_yet() {
        let s = Status::from_json(
            br#"{"model": {"display_name": "Haiku"},
            "context_window": {"used_percentage": null}}"#,
        )
        .unwrap();
        assert_eq!(s.context, None);
        assert!(s.limits.is_empty());
        assert_eq!(s.line(), "Haiku");
        assert_eq!(Status::from_json(b"not json"), None);
    }

    #[test]
    fn the_line_says_model_context_and_session() {
        let s = Status::from_json(FULL).unwrap();
        assert_eq!(
            s.line(),
            "Opus 5.5 \u{00B7} context 23% \u{00B7} session 41%"
        );
        assert_eq!(Status::default().line(), "");
    }

    fn limit(used: f32, resets_at: u64) -> Option<Limit> {
        Some(Limit {
            used,
            resets_at: Some(resets_at),
            minutes: None,
        })
    }

    #[test]
    fn a_brothers_usage_reads_back_as_told() {
        let told = vec![Told {
            agent: Agent::Claude,
            account: Some("acc:org".into()),
            email: Some("a@b.c".into()),
            usage: Usage {
                limits: Limits {
                    five_hour: limit(30.0, 10_000),
                    ..Limits::default()
                },
                at: 900,
            },
        }];
        let msg = usage_message(&told);
        assert_eq!(msg["t"], "usage");
        assert_eq!(super::told(&msg), told);
        // An entry with no limits, or one this build cannot read, is left out.
        let odd = serde_json::json!({"t": "usage", "list": [
            {"agent": "claude", "usage": {"limits": {}, "at": 1}},
            {"agent": "someone new", "usage": {"limits": {}, "at": 1}},
            {"nothing": true},
        ]});
        assert!(super::told(&odd).is_empty());
    }

    #[test]
    fn a_brothers_reading_merges_and_keeps_the_newer_time() {
        let held = Usage {
            limits: Limits {
                five_hour: limit(40.0, 10_000),
                ..Limits::default()
            },
            at: 2_000,
        };
        let heard = Usage {
            limits: Limits {
                five_hour: limit(55.0, 10_000),
                seven_day: limit(10.0, 500_000),
                spend: None,
            },
            at: 1_500,
        };
        let merged = held.merge(&heard, 2_100);
        assert_eq!(merged.limits.five_hour, limit(55.0, 10_000));
        assert_eq!(merged.limits.seven_day, limit(10.0, 500_000));
        assert_eq!(merged.at, 2_000);
    }

    #[test]
    fn an_idle_sessions_old_reading_does_not_pull_the_limit_back() {
        let held = Limits {
            five_hour: limit(40.0, 10_000),
            seven_day: limit(20.0, 500_000),
            spend: None,
        };
        // Same windows, one reset a little off, both lower: an hour old.
        let old = Limits {
            five_hour: limit(25.0, 10_060),
            seven_day: limit(18.0, 500_000),
            spend: None,
        };
        assert_eq!(held.merge(&old, 5_000), held);
        // Higher is newer.
        let newer = Limits {
            five_hour: limit(45.0, 10_000),
            ..Limits::default()
        };
        let m = held.merge(&newer, 5_000);
        assert_eq!(m.five_hour, limit(45.0, 10_000));
        assert_eq!(m.seven_day, held.seven_day);
    }

    #[test]
    fn a_reading_of_a_window_over_is_passed_over() {
        let held = Limits {
            five_hour: limit(5.0, 30_000),
            ..Limits::default()
        };
        let over = Limits {
            five_hour: limit(90.0, 10_000),
            ..Limits::default()
        };
        assert_eq!(held.merge(&over, 12_000), held);
        // A new window, or another account's, is taken as it is.
        let next = Limits {
            five_hour: limit(2.0, 50_000),
            ..Limits::default()
        };
        assert_eq!(held.merge(&next, 32_000), next);
        let other = Limits {
            five_hour: limit(70.0, 20_000),
            ..Limits::default()
        };
        assert_eq!(held.merge(&other, 12_000), other);
        assert_eq!(Limits::default().merge(&over, 12_000), over);
    }

    #[test]
    fn a_full_limit_holds_until_it_resets() {
        let l = Limits {
            five_hour: limit(100.0, 500),
            seven_day: limit(60.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, false), Some(500));
        // Its reset has passed, so it is empty again.
        assert_eq!(l.out_until(500, false), None);
    }

    #[test]
    fn every_full_limit_has_to_reset() {
        let l = Limits {
            five_hour: limit(100.0, 500),
            seven_day: limit(100.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, false), Some(9000));
    }

    #[test]
    fn a_limit_not_full_holds_nothing() {
        let l = Limits {
            five_hour: limit(99.0, 500),
            ..Limits::default()
        };
        assert_eq!(l.out_until(100, false), None);
        assert_eq!(Limits::default().out_until(100, true), None);
    }

    #[test]
    fn a_refused_session_waits_for_the_fullest_limit() {
        let l = Limits {
            five_hour: limit(97.0, 500),
            seven_day: limit(40.0, 9000),
            spend: None,
        };
        assert_eq!(l.out_until(100, true), Some(500));
        // Nothing left to reset, so nothing to wait for.
        assert_eq!(l.out_until(9000, true), None);
    }

    #[test]
    fn the_runner_paces_at_ninety_percent_of_the_fullest_limit() {
        let l = Limits {
            five_hour: limit(92.0, 500),
            seven_day: limit(95.0, 9000),
            spend: None,
        };
        assert_eq!(l.too_full(100), Some(("Week", 95.0)));
        let under = Limits {
            five_hour: limit(89.9, 500),
            ..Limits::default()
        };
        assert_eq!(under.too_full(100), None);
        assert_eq!(Limits::default().too_full(100), None);
        let edge = Limits {
            five_hour: limit(90.0, 500),
            ..Limits::default()
        };
        assert_eq!(edge.too_full(100), Some(("Session", 90.0)));
        // Reset since it was heard, so it is empty again.
        assert_eq!(edge.too_full(500), None);
    }

    #[test]
    fn resets_read_like_a_human_wrote_them() {
        assert_eq!(format_until(20), "1 min");
        assert_eq!(format_until(40 * 60), "40 min");
        assert_eq!(format_until(2 * 3600 + 5 * 60), "2 h 05 min");
        assert_eq!(format_until(3 * 86_400 + 4 * 3600 + 59), "3 d 4 h");
    }

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn defaults_go_in_front_unless_the_session_says_otherwise() {
        let d = Defaults {
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
            // An environment variable, never a flag.
            workflow_model: Some("claude-sonnet-5".into()),
        };
        assert_eq!(
            d.flags(&[]),
            args("--model opus --effort high --permission-mode plan")
        );
        assert_eq!(
            d.flags(&args("--model=haiku --dangerously-skip-permissions")),
            args("--effort high")
        );
        assert_eq!(
            d.flags(&args("--effort max --permission-mode auto")),
            args("--model opus")
        );
        assert!(Defaults::default().flags(&[]).is_empty());
    }

    #[test]
    fn the_workflow_model_goes_in_claudes_environment_when_set() {
        let d = Defaults {
            workflow_model: Some("claude-haiku-4-5-20251001".into()),
            ..Defaults::default()
        };
        assert_eq!(
            d.env(),
            vec![(
                WORKFLOW_MODEL_ENV.to_string(),
                "claude-haiku-4-5-20251001".to_string()
            )]
        );
        assert!(Defaults::default().env().is_empty());
        assert_eq!(Setting::Workflow.command(Some("x")), None);
        assert!(!Setting::ALL.contains(&Setting::Workflow));
    }

    #[test]
    fn a_bypass_takes_the_place_of_the_mode_picked() {
        let d = Defaults {
            model: Some("opus".into()),
            effort: None,
            permission_mode: Some("plan".into()),
            workflow_model: None,
        };
        assert_eq!(
            d.flags_for(&[], true),
            args("--model opus --permission-mode bypassPermissions")
        );
        assert_eq!(
            d.flags_for(&[], false),
            d.flags(&[])
        );
        assert_eq!(
            d.flags_for(&args("--permission-mode auto"), true),
            args("--model opus")
        );
    }

    #[test]
    fn a_flag_is_itself_or_itself_with_a_value() {
        assert!(has_flag(&args("-p x --model haiku"), "--model"));
        assert!(has_flag(&args("--model=haiku"), "--model"));
        assert!(!has_flag(&args("--models haiku"), "--model"));
        assert!(!has_flag(&args("--fallback-model haiku"), "--model"));
    }

    #[test]
    fn settings_name_their_values() {
        let mut d = Defaults::default();
        assert_eq!(
            Setting::Effort.name_of(d.get(Setting::Effort)),
            "Default"
        );
        d.set(Setting::Effort, Some("xhigh".into()));
        assert_eq!(
            Setting::Effort.name_of(d.get(Setting::Effort)),
            "Extra high"
        );
        assert_eq!(
            Setting::Model.name_of(Some("claude-opus-5-5")),
            "Opus 5.5"
        );
        assert_eq!(Setting::Model.name_of(Some("opus")), "Custom");
        for s in Setting::ALL {
            assert!(!s.choices().is_empty(), "{s:?}");
        }
        assert_eq!(Setting::Effort.choices().last().map(|c| c.0), Some("max"));
    }

    #[test]
    fn model_and_effort_switch_a_running_session_permissions_do_not() {
        assert_eq!(
            Setting::Model.command(Some("claude-sonnet-5")).as_deref(),
            Some("/model claude-sonnet-5")
        );
        assert_eq!(
            Setting::Model.command(None).as_deref(),
            Some("/model default")
        );
        assert_eq!(
            Setting::Effort.command(Some("xhigh")).as_deref(),
            Some("/effort xhigh")
        );
        assert_eq!(
            Setting::Effort.command(None).as_deref(),
            Some("/effort auto")
        );
        assert_eq!(Setting::Permissions.command(Some("plan")), None);
        assert!(Setting::Effort.is_scale() && !Setting::Model.is_scale());
    }

    #[test]
    fn a_session_that_chose_a_setting_keeps_it() {
        assert!(Setting::Model.chosen_by(&args("--model=haiku")));
        assert!(Setting::Permissions.chosen_by(&args("--dangerously-skip-permissions")));
        assert!(!Setting::Effort.chosen_by(&args("--model haiku")));
    }

    #[test]
    fn defaults_and_usage_round_trip() {
        let d = Defaults {
            model: Some("sonnet".into()),
            ..Default::default()
        };
        let back: Defaults = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        let u = Usage {
            limits: Status::from_json(FULL).unwrap().limits,
            at: 5,
        };
        let back: Usage = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(back, u);
    }
}
