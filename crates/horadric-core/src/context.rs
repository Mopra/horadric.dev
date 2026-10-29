//! What a session carries before it does any work, item by item, and which
//! of those items the project's sessions ever use.
//!
//! Claude Code writes what it put into the window into the transcript as
//! attachments: the deferred tool names (`deferred_tools_delta`), the skill
//! listing (`skill_listing`), the agent types (`agent_listing_delta`), the
//! MCP servers' instructions (`mcp_instructions_delta`), the CLAUDE.md files
//! (`instructions`) and what hooks added (`hook_additional_context`). Each
//! carries its `rendered` text, the very characters the model was sent. So
//! the cost of each connector, plugin, skill and hook is measured from a
//! real session rather than guessed from config files, and a transcript's
//! tool calls say which of them were ever used (measured 2026-10-29: about
//! 52k tokens a session, most of it connectors and skills an inbox sweep
//! never touches).
//!
//! Everything a session loads belongs to one [`Item`], keyed by where it
//! comes from, since that is what can be turned off: a claude.ai connector,
//! a plugin, an MCP server, a skill of the user's or the project's own.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// Where an item comes from, which says how it is turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// A connector of the claude.ai account, loaded in every project.
    Account,
    /// A plugin, with its skills, agents and MCP servers.
    Plugin,
    /// An MCP server from `~/.claude.json`, `.mcp.json` or `--mcp-config`.
    Mcp,
    /// A skill in `~/.claude/skills` or the project's `.claude/skills`.
    Skill,
    /// Text a hook added to the conversation.
    Hook,
    /// A CLAUDE.md or the memory index.
    Instructions,
    /// Claude Code's own tools and agents, which nothing turns off.
    ClaudeCode,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Account => "claude.ai connector",
            Source::Plugin => "plugin",
            Source::Mcp => "MCP server",
            Source::Skill => "skill",
            Source::Hook => "hook",
            Source::Instructions => "instructions",
            Source::ClaudeCode => "Claude Code",
        }
    }
}

/// One thing a session loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable across sessions: `connector:Slack`, `plugin:figma`,
    /// `mcp:horadric`, `skill:wrangler`, `hook:UserPromptSubmit`,
    /// `file:<path>`, `claude-code`.
    pub key: String,
    /// The name to show.
    pub name: String,
    pub source: Source,
    /// Characters it put into the first window. A hook's are those of the
    /// whole session, since a hook speaks again on every prompt.
    pub chars: u64,
    /// How many times a hook spoke in the session; 1 for the rest.
    pub times: u64,
}

impl Item {
    pub fn tokens(&self) -> u64 {
        crate::spend::tokens_of(self.chars)
    }
}

/// The item a tool name belongs to.
pub fn key_of_tool(tool: &str) -> String {
    let Some(rest) = tool.strip_prefix("mcp__") else {
        return "claude-code".into();
    };
    let server = rest.split("__").next().unwrap_or(rest);
    if let Some(c) = server.strip_prefix("claude_ai_") {
        return format!("connector:{c}");
    }
    if let Some(p) = server.strip_prefix("plugin_") {
        // `plugin_<plugin>_<server>`: plugin names carry hyphens, not
        // underscores, so the first underscore ends the plugin.
        return format!("plugin:{}", p.split('_').next().unwrap_or(p));
    }
    format!("mcp:{server}")
}

/// A connector's name as its tools spell it: `monday.com` is `monday_com`.
fn squash(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The item an MCP server's display name belongs to, as the instructions
/// and the failed list name them: `claude.ai Slack`, `plugin:figma:figma`,
/// `horadric`.
pub fn key_of_server(name: &str) -> String {
    if let Some(c) = name.strip_prefix("claude.ai ") {
        return format!("connector:{}", squash(c));
    }
    if let Some(p) = name.strip_prefix("plugin:") {
        return format!("plugin:{}", p.split(':').next().unwrap_or(p));
    }
    format!("mcp:{name}")
}

/// The item a skill or agent type belongs to: a plugin's are prefixed with
/// it (`vercel:deploy`), the rest are the user's or the project's own, or
/// Claude Code's.
pub fn key_of_skill(name: &str) -> String {
    match name.split_once(':') {
        Some((p, _)) => format!("plugin:{p}"),
        None => format!("skill:{name}"),
    }
}

fn source_of(key: &str) -> Source {
    match key.split_once(':').map(|(k, _)| k).unwrap_or(key) {
        "connector" => Source::Account,
        "plugin" => Source::Plugin,
        "mcp" => Source::Mcp,
        "skill" => Source::Skill,
        "hook" => Source::Hook,
        "file" => Source::Instructions,
        _ => Source::ClaudeCode,
    }
}

fn name_of(key: &str) -> String {
    match key.split_once(':') {
        Some(("connector", c)) => format!("claude.ai {}", c.replace('_', " ")),
        Some((_, n)) => n.to_string(),
        None => "Claude Code".into(),
    }
}

/// The lines of a listing (`- name: what it does`), each with its name.
fn listed(text: &str) -> Vec<(&str, u64)> {
    let mut out: Vec<(&str, u64)> = Vec::new();
    for line in text.lines() {
        let len = line.len() as u64 + 1;
        match line.strip_prefix("- ") {
            Some(rest) => {
                // A plugin's skill keeps its prefix (`vercel:deploy: ...`),
                // and a skill with no description is its name alone.
                let name = rest.split_once(": ").map_or(rest, |(n, _)| n);
                out.push((name.trim(), len));
            }
            // A description running onto more lines stays with its skill.
            None => {
                if let Some(last) = out.last_mut() {
                    last.1 += len;
                }
            }
        }
    }
    out
}

fn rendered(entry: &Value) -> String {
    entry
        .get("rendered")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(|p| p.get("content").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

fn strs(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// What one transcript's session loaded, largest first. Only the main
/// conversation counts (a subagent's lines are its own window), and a
/// listing counts once, from the first time it was sent: a later delta
/// adds to what is there, so it is counted too.
pub fn loaded(jsonl: &str) -> Vec<Item> {
    let mut chars: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut add = |key: String, n: u64, times: u64| {
        let e = chars.entry(key).or_default();
        e.0 += n;
        e.1 += times;
    };
    for line in jsonl.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("attachment")
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(a) = entry.get("attachment") else {
            continue;
        };
        let text = rendered(&entry);
        match a.get("type").and_then(Value::as_str).unwrap_or("") {
            "deferred_tools_delta" => {
                // The rendered text is the tool names, one a line, plus a
                // preamble and the failed servers, charged to Claude Code.
                let names = strs(a.get("addedNames"));
                let mut named = 0;
                for n in &names {
                    let len = n.len() as u64 + 1;
                    named += len;
                    add(key_of_tool(n), len, 1);
                }
                add(
                    "claude-code".into(),
                    (text.len() as u64).saturating_sub(named),
                    1,
                );
            }
            "skill_listing" => {
                let mut named = 0;
                for (name, len) in listed(&text) {
                    named += len;
                    add(key_of_skill(name), len, 1);
                }
                add(
                    "claude-code".into(),
                    (text.len() as u64).saturating_sub(named),
                    1,
                );
            }
            "agent_listing_delta" => {
                let builtin = strs(a.get("builtInTypes"));
                let mut named = 0;
                for (name, len) in listed(&text) {
                    if builtin.contains(&name) {
                        continue;
                    }
                    named += len;
                    add(key_of_skill(name), len, 1);
                }
                add(
                    "claude-code".into(),
                    (text.len() as u64).saturating_sub(named),
                    1,
                );
            }
            "mcp_instructions_delta" => {
                let names = strs(a.get("addedNames"));
                let blocks = strs(a.get("addedBlocks"));
                let mut named = 0;
                for (n, b) in names.iter().zip(&blocks) {
                    named += b.len() as u64;
                    add(key_of_server(n), b.len() as u64, 1);
                }
                add(
                    "claude-code".into(),
                    (text.len() as u64).saturating_sub(named),
                    1,
                );
            }
            "instructions" => {
                for f in a
                    .get("files")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let path = f.get("path").and_then(Value::as_str).unwrap_or("");
                    let n = f.get("content").and_then(Value::as_str).map_or(0, str::len);
                    add(format!("file:{path}"), n as u64, 1);
                }
            }
            "hook_additional_context" => {
                let event = a.get("hookName").and_then(Value::as_str).unwrap_or("hook");
                let event = event.split(':').next().unwrap_or(event);
                add(format!("hook:{event}"), text.len() as u64, 1);
            }
            _ => {}
        }
    }
    let mut items: Vec<Item> = chars
        .into_iter()
        .filter(|(_, (n, _))| *n > 0)
        .map(|(key, (chars, times))| Item {
            source: source_of(&key),
            name: name_of(&key),
            times: if key.starts_with("hook:") { times } else { 1 },
            key,
            chars,
        })
        .collect();
    items.sort_by(|a, b| b.chars.cmp(&a.chars).then(a.key.cmp(&b.key)));
    items
}

/// How often an item was used, and when last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Use {
    pub calls: u64,
    /// The last call's timestamp, as the transcript writes it (RFC 3339,
    /// so later sorts later).
    pub last: String,
}

/// The items a transcript's calls used: an MCP tool, a skill through the
/// Skill tool, a plugin's agent through the Agent tool. Subagents' calls
/// count too, since a connector a subagent used was needed.
pub fn used(jsonl: &str, into: &mut BTreeMap<String, Use>) {
    for line in jsonl.lines() {
        if !line.contains("\"tool_use\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = entry.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let blocks = entry
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for b in blocks {
            if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
            let input = b.get("input");
            let key = match name {
                "Skill" => input
                    .and_then(|i| i.get("skill"))
                    .and_then(Value::as_str)
                    .map(key_of_skill),
                "Agent" | "Task" => input
                    .and_then(|i| i.get("subagent_type"))
                    .and_then(Value::as_str)
                    .filter(|t| t.contains(':'))
                    .map(key_of_skill),
                n if n.starts_with("mcp__") => Some(key_of_tool(n)),
                _ => None,
            };
            if let Some(key) = key {
                let u = into.entry(key).or_default();
                u.calls += 1;
                if at > u.last.as_str() {
                    u.last = at.to_string();
                }
            }
        }
    }
}

/// How an item is turned off in a project's `.claude/settings.json`.
/// Verified against Claude Code 2.1.312 (2026-10-29): each of these drops
/// the item from the window, not only its calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Switch {
    /// `enabledPlugins["<id>"]: false`, the plugin's skills, agents and
    /// servers together. A project's false wins over the user's true.
    Plugin(String),
    /// `deniedMcpServers: [{"serverName": ...}]`, one server by the name
    /// Claude Code gives it: `claude.ai Slack`, `plugin:figma:figma`,
    /// `horadric`. claude.ai connectors go one by one this way.
    Server(String),
    /// `skillOverrides["<name>"]: "off"`, one skill of the user's or the
    /// project's own.
    Skill(String),
    /// Nothing in the project's settings turns it off: Claude Code itself,
    /// a hook, a CLAUDE.md.
    None,
}

/// Turns every claude.ai connector off at once, the same as the env
/// variable `ENABLE_CLAUDEAI_MCP_SERVERS=false`. Any settings file saying
/// true wins, so a project can opt out but never back in.
pub const NO_CONNECTORS: &str = "disableClaudeAiConnectors";

/// Every settings key a switch writes.
pub const SWITCH_KEYS: [&str; 4] = [
    "enabledPlugins",
    "deniedMcpServers",
    "skillOverrides",
    NO_CONNECTORS,
];

/// The name Claude Code gives the connector `key`, from `servers`, the
/// names it has connected (`claudeAiMcpEverConnected` in `.claude.json`);
/// failing that, guessed from the key, which loses a dot.
pub fn connector_name(key: &str, servers: &[String]) -> Option<String> {
    let c = key.strip_prefix("connector:")?;
    Some(
        servers
            .iter()
            .find(|s| key_of_server(s) == key)
            .cloned()
            .unwrap_or_else(|| format!("claude.ai {}", c.replace('_', " "))),
    )
}

/// Names the connectors among `items` as Claude Code does, see
/// [`connector_name`].
pub fn name_connectors(items: &mut [Item], servers: &[String]) {
    for i in items {
        if let Some(n) = connector_name(&i.key, servers) {
            i.name = n;
        }
    }
}

/// How `key` is turned off. `plugins` are the plugin ids the user's
/// settings know (`figma@claude-plugins-official`), `servers` the
/// connector names, see [`connector_name`].
pub fn switch_of(key: &str, plugins: &[String], servers: &[String]) -> Switch {
    match key.split_once(':') {
        Some(("connector", _)) => Switch::Server(connector_name(key, servers).unwrap_or_default()),
        Some(("plugin", p)) => plugins
            .iter()
            .find(|id| id.split('@').next() == Some(p))
            .map_or(Switch::None, |id| Switch::Plugin(id.clone())),
        Some(("mcp", s)) => Switch::Server(s.to_string()),
        Some(("skill", s)) => Switch::Skill(s.to_string()),
        _ => Switch::None,
    }
}

fn denied(settings: &Value) -> impl Iterator<Item = &str> {
    settings
        .get("deniedMcpServers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("serverName").and_then(Value::as_str))
}

/// Whether a project's settings leave `switch` on.
pub fn is_on(settings: &Value, switch: &Switch) -> bool {
    match switch {
        Switch::Plugin(id) => settings
            .get("enabledPlugins")
            .and_then(|p| p.get(id))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        Switch::Server(s) => {
            let all_off = s.starts_with("claude.ai ")
                && settings.get(NO_CONNECTORS).and_then(Value::as_bool) == Some(true);
            !all_off && !denied(settings).any(|d| d == s)
        }
        Switch::Skill(s) => {
            settings
                .get("skillOverrides")
                .and_then(|o| o.get(s))
                .and_then(Value::as_str)
                != Some("off")
        }
        Switch::None => true,
    }
}

fn object<'a>(v: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !v.is_object() {
        *v = json!({});
    }
    let o = v.as_object_mut().expect("made an object above");
    let e = o.entry(key).or_insert_with(|| json!({}));
    if !e.is_object() {
        *e = json!({});
    }
    e.as_object_mut().expect("made an object above")
}

/// Writes `switch` into a project's settings. Turning a thing back on
/// removes the line rather than writing it on, so the user's own setting
/// shows through again.
pub fn set(settings: &mut Value, switch: &Switch, on: bool) {
    match switch {
        Switch::Plugin(id) => {
            let o = object(settings, "enabledPlugins");
            if on {
                o.remove(id);
            } else {
                o.insert(id.clone(), json!(false));
            }
        }
        Switch::Skill(s) => {
            let o = object(settings, "skillOverrides");
            if on {
                o.remove(s);
            } else {
                o.insert(s.clone(), json!("off"));
            }
        }
        Switch::Server(s) => {
            if !settings.is_object() {
                *settings = json!({});
            }
            let o = settings.as_object_mut().expect("made an object above");
            let list = o.entry("deniedMcpServers").or_insert_with(|| json!([]));
            if !list.is_array() {
                *list = json!([]);
            }
            let a = list.as_array_mut().expect("made an array above");
            a.retain(|d| d.get("serverName").and_then(Value::as_str) != Some(s));
            if !on {
                a.push(json!({ "serverName": s }));
            }
            // Turning one connector on undoes the switch for all of them,
            // which this project's settings would otherwise still hold.
            if on && s.starts_with("claude.ai ") {
                o.remove(NO_CONNECTORS);
            }
        }
        Switch::None => {}
    }
    // Leave no empty switch behind for the user to wonder about, and
    // never touch a key the switches do not own.
    if let Some(o) = settings.as_object_mut() {
        o.retain(|k, v| {
            !SWITCH_KEYS.contains(&k.as_str())
                || match v {
                    Value::Object(m) => !m.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    _ => true,
                }
        });
    }
}

/// The `--settings` a lean quest starts with: every claude.ai connector
/// off but those its notes ask for (`Connectors: monday, Zoho`), matched
/// by their name without case or spaces. `known` are the connectors this
/// project's sessions loaded. None asked for turns them all off at once,
/// which also covers a connector added since; `all` keeps every one.
pub fn lean(known: &[String], wanted: &[String]) -> Value {
    let squash = |s: &str| {
        s.trim_start_matches("claude.ai ")
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    if wanted.iter().any(|w| w.eq_ignore_ascii_case("all")) {
        return json!({});
    }
    let wanted: Vec<String> = wanted.iter().map(|w| squash(w)).collect();
    if wanted.is_empty() {
        return json!({ NO_CONNECTORS: true });
    }
    let deny: Vec<Value> = known
        .iter()
        .filter(|k| {
            let k = squash(k);
            !wanted.iter().any(|w| k.starts_with(w.as_str()))
        })
        .map(|k| json!({ "serverName": k }))
        .collect();
    json!({ "deniedMcpServers": deny })
}

/// The connectors' server names among `items`, as [`lean`] wants them.
pub fn connectors(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter(|i| i.source == Source::Account)
        .map(|i| i.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(a: Value, text: &str) -> String {
        json!({"type": "attachment", "attachment": a, "rendered": [{"content": text}]}).to_string()
    }

    #[test]
    fn tools_belong_to_their_connector_plugin_or_server() {
        assert_eq!(
            key_of_tool("mcp__claude_ai_Read_AI__list"),
            "connector:Read_AI"
        );
        assert_eq!(
            key_of_tool("mcp__plugin_monday-com_monday__auth"),
            "plugin:monday-com"
        );
        assert_eq!(key_of_tool("mcp__horadric__lore_find"), "mcp:horadric");
        assert_eq!(key_of_tool("WebFetch"), "claude-code");
        assert_eq!(key_of_server("claude.ai Read AI"), "connector:Read_AI");
        assert_eq!(key_of_server("plugin:figma:figma"), "plugin:figma");
        assert_eq!(key_of_skill("vercel:deploy"), "plugin:vercel");
        assert_eq!(key_of_skill("wrangler"), "skill:wrangler");
        assert_eq!(name_of("connector:Read_AI"), "claude.ai Read AI");
        assert_eq!(
            key_of_server("claude.ai monday.com"),
            "connector:monday_com"
        );
        let servers = vec!["claude.ai monday.com".to_string()];
        assert_eq!(
            switch_of("connector:monday_com", &[], &servers),
            Switch::Server("claude.ai monday.com".into())
        );
        assert_eq!(
            listed(
                "- monday-com:board-setup
- a:b: c d
"
            ),
            [("monday-com:board-setup", 25), ("a:b", 11)]
        );
    }

    #[test]
    fn a_session_loaded_each_item_by_its_characters() {
        let jsonl = [
            attachment(
                json!({"type": "deferred_tools_delta",
                       "addedNames": ["WebFetch", "mcp__claude_ai_Slack__send", "mcp__claude_ai_Slack__read"]}),
                "Deferred:\nWebFetch\nmcp__claude_ai_Slack__send\nmcp__claude_ai_Slack__read\n",
            ),
            attachment(
                json!({"type": "skill_listing"}),
                "- wrangler: Deploy workers.\n- vercel:deploy: Deploy: to Vercel.\n  more about it\n",
            ),
            attachment(
                json!({"type": "mcp_instructions_delta", "addedNames": ["claude.ai Slack"],
                       "addedBlocks": ["## claude.ai Slack\nRead the topic."]}),
                "# MCP\n## claude.ai Slack\nRead the topic.",
            ),
            attachment(
                json!({"type": "hook_additional_context", "hookName": "UserPromptSubmit"}),
                "recalled",
            ),
            attachment(
                json!({"type": "hook_additional_context", "hookName": "UserPromptSubmit"}),
                "recalled",
            ),
            attachment(
                json!({"type": "instructions", "files": [{"path": "C:/p/CLAUDE.md", "content": "rules"}]}),
                "",
            ),
            // A subagent's window is its own.
            json!({"type": "attachment", "isSidechain": true,
                   "attachment": {"type": "hook_additional_context", "hookName": "SessionStart"},
                   "rendered": [{"content": "sub"}]})
            .to_string(),
        ]
        .join("\n");
        let items = loaded(&jsonl);
        let get = |k: &str| items.iter().find(|i| i.key == k).cloned();
        let slack = get("connector:Slack").unwrap();
        assert_eq!(slack.source, Source::Account);
        assert_eq!(
            slack.chars,
            27 + 27 + "## claude.ai Slack\nRead the topic.".len() as u64
        );
        assert_eq!(get("skill:wrangler").unwrap().chars, 28);
        assert_eq!(get("plugin:vercel").unwrap().chars, 36 + 16);
        let hook = get("hook:UserPromptSubmit").unwrap();
        assert_eq!((hook.chars, hook.times), (16, 2));
        assert_eq!(get("file:C:/p/CLAUDE.md").unwrap().chars, 5);
        assert!(get("hook:SessionStart").is_none());
        assert_eq!(get("claude-code").unwrap().chars, 10 + 9 + 6);
        assert!(items.windows(2).all(|w| w[0].chars >= w[1].chars));
    }

    #[test]
    fn calls_say_which_items_were_used_and_when_last() {
        let call = |at: &str, name: &str, input: Value| {
            json!({"type": "assistant", "timestamp": at, "message": {"content": [
                {"type": "tool_use", "name": name, "input": input}]}})
            .to_string()
        };
        let jsonl = [
            call(
                "2026-10-01T00:00:00Z",
                "mcp__claude_ai_Slack__send",
                json!({}),
            ),
            call(
                "2026-10-03T00:00:00Z",
                "mcp__claude_ai_Slack__read",
                json!({}),
            ),
            call(
                "2026-10-02T00:00:00Z",
                "Skill",
                json!({"skill": "vercel:deploy"}),
            ),
            call(
                "2026-10-02T00:00:00Z",
                "Agent",
                json!({"subagent_type": "Explore"}),
            ),
            call(
                "2026-10-02T00:00:00Z",
                "Agent",
                json!({"subagent_type": "vercel:ai-architect"}),
            ),
        ]
        .join("\n");
        let mut uses = BTreeMap::new();
        used(&jsonl, &mut uses);
        assert_eq!(
            uses["connector:Slack"],
            Use {
                calls: 2,
                last: "2026-10-03T00:00:00Z".into()
            }
        );
        assert_eq!(uses["plugin:vercel"].calls, 2);
        assert_eq!(uses.len(), 2);
    }

    #[test]
    fn switches_write_the_real_settings_and_undo_cleanly() {
        let plugins = vec!["figma@claude-plugins-official".to_string()];
        let figma = switch_of("plugin:figma", &plugins, &[]);
        let read_ai = switch_of("connector:Read_AI", &plugins, &[]);
        assert_eq!(read_ai, Switch::Server("claude.ai Read AI".into()));
        let wrangler = switch_of("skill:wrangler", &plugins, &[]);
        assert_eq!(switch_of("plugin:gone", &plugins, &[]), Switch::None);
        assert_eq!(switch_of("hook:Stop", &plugins, &[]), Switch::None);

        let mut s = json!({"permissions": {"allow": ["Bash"]}, "env": {}});
        for sw in [&figma, &read_ai, &wrangler] {
            assert!(is_on(&s, sw));
            set(&mut s, sw, false);
            assert!(!is_on(&s, sw));
        }
        assert_eq!(
            s,
            json!({"permissions": {"allow": ["Bash"]}, "env": {},
                   "enabledPlugins": {"figma@claude-plugins-official": false},
                   "deniedMcpServers": [{"serverName": "claude.ai Read AI"}],
                   "skillOverrides": {"wrangler": "off"}})
        );
        for sw in [&figma, &read_ai, &wrangler] {
            set(&mut s, sw, true);
            assert!(is_on(&s, sw));
        }
        assert_eq!(s, json!({"permissions": {"allow": ["Bash"]}, "env": {}}));
    }

    #[test]
    fn all_connectors_off_reads_as_each_off_and_one_on_undoes_it() {
        let slack = Switch::Server("claude.ai Slack".into());
        let mut s = json!({ NO_CONNECTORS: true });
        assert!(!is_on(&s, &slack));
        assert!(is_on(&s, &Switch::Server("horadric".into())));
        set(&mut s, &slack, true);
        assert_eq!(s, json!({}));
    }

    #[test]
    fn a_lean_quest_keeps_only_the_connectors_its_notes_ask_for() {
        let known = vec![
            "claude.ai monday.com".to_string(),
            "claude.ai Slack".to_string(),
            "claude.ai Microsoft 365".to_string(),
        ];
        assert_eq!(lean(&known, &[]), json!({ NO_CONNECTORS: true }));
        assert_eq!(lean(&known, &["All".into()]), json!({}));
        assert_eq!(
            lean(&known, &["monday".into(), "microsoft365".into()]),
            json!({"deniedMcpServers": [{"serverName": "claude.ai Slack"}]})
        );
    }
}
