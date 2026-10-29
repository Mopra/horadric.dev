//! What the CLI says about the session rather than the conversation: its
//! `system/init` (sent again at every turn's start, latest wins), the
//! answer to `initialize` (commands with descriptions, models with their
//! effort levels), what `get_context_usage` returns, and each turn's usage.

use serde_json::Value;

/// The session as `system/init` describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInfo {
    pub cwd: String,
    pub claude_version: String,
    pub output_style: String,
    /// `none` when the subscription login is used.
    pub api_key_source: String,
    pub permission_mode: String,
    pub tools: Vec<String>,
    /// Slash command names without the slash: built in, custom and skills.
    pub slash_commands: Vec<String>,
    pub skills: Vec<String>,
    pub agents: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
    pub plugins: Vec<String>,
    /// Protocol features this CLI has, such as `msg_lifecycle_v1`.
    pub capabilities: Vec<String>,
    /// From `initialize` only: each command with its description.
    pub commands: Vec<CommandInfo>,
    /// From `initialize` only: the models `/model` offers.
    pub models: Vec<ModelInfo>,
    /// From `initialize` only.
    pub output_styles: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    /// Such as `connected`, `pending`, `needs-auth`, `failed`.
    pub status: String,
}

/// A slash command as `initialize` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
}

/// A model the session can switch to, as `initialize` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelInfo {
    /// What `set_model` takes, such as `opus` or `default`.
    pub value: String,
    /// The model id it is now, such as `claude-opus-5-5`.
    pub resolved: String,
    pub display_name: String,
    pub description: String,
    /// The levels `/effort` takes for it, empty when it has none.
    pub effort_levels: Vec<String>,
}

/// One entry of the composer's `/` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// Without the slash.
    pub name: String,
    /// Empty until `initialize` has answered.
    pub description: String,
    pub argument_hint: String,
    pub skill: bool,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl SessionInfo {
    /// Takes in a `system/init`, keeping what only `initialize` gives.
    pub fn read_init(&mut self, v: &Value) {
        let keep = (
            std::mem::take(&mut self.commands),
            std::mem::take(&mut self.models),
            std::mem::take(&mut self.output_styles),
        );
        *self = SessionInfo {
            cwd: text(v, "cwd"),
            claude_version: text(v, "claude_code_version"),
            output_style: text(v, "output_style"),
            api_key_source: text(v, "apiKeySource"),
            permission_mode: text(v, "permissionMode"),
            tools: strings(&v["tools"]),
            slash_commands: strings(&v["slash_commands"]),
            skills: strings(&v["skills"]),
            agents: strings(&v["agents"]),
            mcp_servers: v["mcp_servers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| McpServer {
                    name: text(s, "name"),
                    status: text(s, "status"),
                })
                .collect(),
            plugins: v["plugins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["name"].as_str().map(str::to_string))
                .collect(),
            capabilities: strings(&v["capabilities"]),
            commands: keep.0,
            models: keep.1,
            output_styles: keep.2,
        };
    }

    /// Takes in a whole command list, as `initialize` answers it and
    /// `commands_changed` sends it again when skills turn up mid session.
    pub fn read_commands(&mut self, commands: &Value) {
        self.commands = (commands.as_array().into_iter().flatten())
            .map(|c| CommandInfo {
                name: text(c, "name"),
                description: text(c, "description"),
                argument_hint: text(c, "argumentHint"),
            })
            .collect();
    }

    /// Takes in the answer to `initialize`.
    pub fn read_initialize(&mut self, r: &Value) {
        self.read_commands(&r["commands"]);
        self.models = r["models"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| ModelInfo {
                value: text(m, "value"),
                resolved: text(m, "resolvedModel"),
                display_name: text(m, "displayName"),
                description: text(m, "description"),
                effort_levels: strings(&m["supportedEffortLevels"]),
            })
            .collect();
        self.output_styles = strings(&r["available_output_styles"]);
        if let Some(m) = r["current_permission_mode"].as_str() {
            self.permission_mode = m.to_string();
        }
        if let Some(s) = r["output_style"].as_str() {
            self.output_style = s.to_string();
        }
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c == name)
    }

    /// The commands for what the human typed after `/`, names that start
    /// with it first, then those that hold it, each group in the CLI's
    /// order. Descriptions come from `initialize` once it answered.
    pub fn slash_commands_for(&self, typed: &str) -> Vec<SlashCommand> {
        let typed = typed.trim_start_matches('/').to_lowercase();
        let mut names: Vec<&str> = self.slash_commands.iter().map(String::as_str).collect();
        for c in &self.commands {
            if !names.contains(&c.name.as_str()) {
                names.push(&c.name);
            }
        }
        let entry = |name: &str| {
            let info = self.commands.iter().find(|c| c.name == name);
            SlashCommand {
                name: name.to_string(),
                description: info.map(|c| c.description.clone()).unwrap_or_default(),
                argument_hint: info.map(|c| c.argument_hint.clone()).unwrap_or_default(),
                skill: self.skills.iter().any(|s| s == name),
            }
        };
        let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let starts = names
            .iter()
            .zip(&lower)
            .filter(|(_, l)| l.starts_with(&typed));
        let holds = names
            .iter()
            .zip(&lower)
            .filter(|(_, l)| !l.starts_with(&typed) && l.contains(&typed));
        starts.chain(holds).map(|(n, _)| entry(n)).collect()
    }
}

/// The context window, from `get_context_usage` or a turn's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextUsage {
    pub used: u64,
    pub limit: u64,
    /// Where auto compact starts, when the CLI said.
    pub auto_compact_at: Option<u64>,
    /// What fills it, from `get_context_usage` only: system prompt, tools,
    /// memory files, messages. Deferred ones are listed but not loaded.
    pub categories: Vec<ContextCategory>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u64,
    pub deferred: bool,
}

impl ContextUsage {
    /// The answer to `get_context_usage`.
    pub fn from_response(r: &Value) -> ContextUsage {
        let n = |k: &str| r[k].as_u64().unwrap_or_default();
        ContextUsage {
            used: n("totalTokens"),
            limit: n("maxTokens"),
            auto_compact_at: r["isAutoCompactEnabled"]
                .as_bool()
                .unwrap_or(false)
                .then(|| r["autoCompactThreshold"].as_u64())
                .flatten(),
            categories: r["categories"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| ContextCategory {
                    name: text(c, "name"),
                    tokens: c["tokens"].as_u64().unwrap_or_default(),
                    deferred: c["isDeferred"].as_bool().unwrap_or(false) || c["kind"] == "deferred",
                })
                .collect(),
        }
    }

    /// Tokens left before the window is full.
    pub fn left(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }
}

/// What one turn used, from its `result`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnUsage {
    /// Not cached: new to this turn.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// The context window after the turn's last call: what its prompt held
    /// plus what it wrote. None when the result does not say.
    pub context_used: Option<u64>,
    pub context_limit: Option<u64>,
}

impl TurnUsage {
    /// From a `result` line. The context is its last API call's, from
    /// `usage.iterations`, against the window `modelUsage` gives the model.
    pub fn from_result(v: &Value) -> TurnUsage {
        let u = &v["usage"];
        let n = |u: &Value, k: &str| u[k].as_u64().unwrap_or_default();
        let context_used = u["iterations"]
            .as_array()
            .and_then(|i| i.last())
            .map(|last| {
                n(last, "input_tokens")
                    + n(last, "cache_read_input_tokens")
                    + n(last, "cache_creation_input_tokens")
                    + n(last, "output_tokens")
            });
        let context_limit = v["modelUsage"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|m| m["contextWindow"].as_u64())
            .max();
        TurnUsage {
            input_tokens: n(u, "input_tokens"),
            output_tokens: n(u, "output_tokens"),
            cache_read_tokens: n(u, "cache_read_input_tokens"),
            cache_creation_tokens: n(u, "cache_creation_input_tokens"),
            context_used,
            context_limit,
        }
    }

    /// Tokens left in the window after the turn, when both are known.
    pub fn context_left(&self) -> Option<u64> {
        Some(self.context_limit?.saturating_sub(self.context_used?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info() -> SessionInfo {
        let mut i = SessionInfo::default();
        i.read_init(&json!({
            "cwd": "C:\\scratch\\play", "claude_code_version": "2.1.312",
            "permissionMode": "auto", "output_style": "default", "apiKeySource": "none",
            "tools": ["Bash", "Read"], "slash_commands": ["compact", "clear", "review", "commit"],
            "skills": ["commit"], "agents": ["Explore"],
            "mcp_servers": [{"name": "horadric", "status": "connected"}],
            "plugins": [{"name": "horadric", "path": "x"}],
            "capabilities": ["msg_lifecycle_v1"],
        }));
        i
    }

    #[test]
    fn init_fills_the_session() {
        let i = info();
        assert_eq!(i.claude_version, "2.1.312");
        assert_eq!(i.permission_mode, "auto");
        assert_eq!(
            i.mcp_servers,
            [McpServer {
                name: "horadric".into(),
                status: "connected".into()
            }]
        );
        assert_eq!(i.plugins, ["horadric"]);
        assert!(i.has_capability("msg_lifecycle_v1"));
    }

    #[test]
    fn the_picker_puts_prefixes_first_and_marks_skills() {
        let mut i = info();
        let names = |v: Vec<SlashCommand>| v.into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(
            names(i.slash_commands_for("/c")),
            ["compact", "clear", "commit"]
        );
        assert_eq!(names(i.slash_commands_for("e")), ["clear", "review"]);
        assert_eq!(names(i.slash_commands_for("re")), ["review"]);
        assert!(i.slash_commands_for("commit")[0].skill);
        assert!(i.slash_commands_for("compact")[0].description.is_empty());

        i.read_initialize(&json!({
            "commands": [{"name": "compact", "description": "Clear history but keep a summary", "argumentHint": "<instructions>"}],
            "models": [{"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus",
                "description": "d", "supportedEffortLevels": ["low", "high"]}],
            "available_output_styles": ["default", "Explanatory"],
            "current_permission_mode": "plan",
        }));
        let c = &i.slash_commands_for("comp")[0];
        assert_eq!(c.description, "Clear history but keep a summary");
        assert_eq!(c.argument_hint, "<instructions>");
        assert_eq!(i.models[0].effort_levels, ["low", "high"]);
        assert_eq!(i.permission_mode, "plan");

        // A later init keeps what initialize said.
        i.read_init(&json!({"slash_commands": ["compact"]}));
        assert_eq!(i.commands.len(), 1);
        assert_eq!(i.models.len(), 1);
    }

    #[test]
    fn a_turn_s_context_is_its_last_call() {
        let u = TurnUsage::from_result(&json!({
            "usage": {"input_tokens": 66, "output_tokens": 850, "cache_read_input_tokens": 90054,
                "cache_creation_input_tokens": 50505,
                "iterations": [{"input_tokens": 32, "output_tokens": 3,
                    "cache_read_input_tokens": 49844, "cache_creation_input_tokens": 661}]},
            "modelUsage": {"claude-fable-5-1": {"contextWindow": 1000000}},
        }));
        assert_eq!(u.output_tokens, 850);
        assert_eq!(u.context_used, Some(32 + 3 + 49844 + 661));
        assert_eq!(u.context_limit, Some(1_000_000));
        assert_eq!(u.context_left(), Some(1_000_000 - 50540));
        assert_eq!(TurnUsage::from_result(&json!({})).context_left(), None);
    }
}
