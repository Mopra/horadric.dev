//! MCP from the Folio: the servers' status, signing in to one, and the
//! questions a server asks the human (elicitations).
//!
//! What 2.1.314 does headless, tried 2026-10-31 against a stdio server of
//! our own, a command that does not exist and an OAuth one over http:
//!
//! - `system/init` lists each server with its status only (`connected`,
//!   `pending`, `failed`, `needs-auth`, `disabled`), and comes only as a
//!   turn starts. `mcp_status` answers at any time, before the first turn
//!   too, with `mcpServers`: each with `error` when failed, `transport`,
//!   `serverInfo`, its `tools`. Nothing is sent when a status changes, so
//!   what waits on one asks again.
//! - `mcp_reconnect` (`serverName`) answers once dialled: success, or an
//!   error such as `Connection closed`. `mcp_toggle` (`serverName`,
//!   `enabled`) turns one off or on; off shows as `disabled`.
//! - `mcp_authenticate` (`serverName`) answers with `authUrl`,
//!   `requiresUserAction`, `callbackExpected` and, for the usual
//!   localhost redirect, `callbackPort`: the CLI listens there itself, so
//!   opening the address in the browser is all a host does. It then
//!   reconnects the server by itself. A server that cannot sign in (stdio)
//!   gets an error naming its transport.
//! - An MCP server's `elicitation/create` arrives as a `control_request`
//!   `elicitation` with `mcp_server_name`, `message`, `mode` (`form` or
//!   `url`), `requested_schema` and `property_order`, or `url` and
//!   `elicitation_id`. The answer is `{action: accept|decline|cancel,
//!   content}`; the server gets exactly that. A url one that finishes
//!   sends `system/elicitation_complete` with its id.

use serde_json::{Map, Value};

use super::{
    control_request_line, control_success, AskState, Block, Folio, McpRemoval, McpServer, Sent,
};

/// Asks for status this many times while a sign-in or a pending server
/// waits, every couple of seconds, then gives up on the sign-in: five
/// minutes is longer than anyone takes in a browser.
const POLLS: u32 = 150;

/// What the Folio keeps of MCP between lines.
#[derive(Debug, Default)]
pub struct McpState {
    /// Every server with all the CLI has said of it, in its order.
    pub servers: Vec<McpServer>,
    /// Sign-in pages to open in the browser, taken by the view: a replayed
    /// answer is not one to open again, so the view takes them only live.
    pub to_open: Vec<String>,
    /// Servers a card already said were in trouble, so a later status
    /// that says it again adds none.
    warned: std::collections::HashSet<String>,
    /// Whether `mcp_status` went to this process at all.
    asked: bool,
    /// Something changed that a fresh status should show.
    stale: bool,
    /// Status asks since a wait began, see [`POLLS`].
    polls: u32,
}

/// The servers as a card in the transcript shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCard {
    pub servers: Vec<McpServer>,
    /// `/mcp`: every server. Otherwise only those that were in trouble
    /// when it was added, kept up to date as they mend.
    pub all: bool,
}

/// When the view should ask for the servers' status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpWant {
    No,
    Now,
    /// In a moment: something waits on a status to change.
    Soon,
}

/// A server is in trouble the human can do something about.
pub fn troubled(status: &str) -> bool {
    matches!(status, "failed" | "needs-auth")
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl McpServer {
    /// One row of `mcp_status`'s `mcpServers`.
    pub fn from_status(v: &Value) -> McpServer {
        McpServer {
            name: text(v, "name"),
            status: text(v, "status"),
            error: text(v, "error"),
            transport: v["transport"]
                .as_str()
                .or(v["config"]["type"].as_str())
                .unwrap_or_default()
                .to_string(),
            tools: v["tools"].as_array().map_or(0, Vec::len),
            proxied: v["sessionProxied"].as_bool().unwrap_or(false),
            source: v["source"]
                .as_str()
                .or(v["scope"].as_str())
                .unwrap_or_default()
                .to_string(),
            ..McpServer::default()
        }
    }

    /// How it can be removed, None when Horadric cannot: one given on the
    /// command line or by managed settings.
    pub fn removal(&self) -> Option<McpRemoval> {
        match self.source.as_str() {
            "plugin" => {
                // Named `plugin:<plugin>:<server>`.
                let plugin = self.name.strip_prefix("plugin:")?.split(':').next()?;
                (!plugin.is_empty()).then(|| McpRemoval::Plugin(plugin.to_string()))
            }
            "claudeai" => Some(McpRemoval::ClaudeAi),
            "user" | "project" | "local" => Some(McpRemoval::Scope(self.source.clone())),
            _ => None,
        }
    }

    /// Whether a sign-in can be offered: it waits on one, and its sign-in
    /// is not held by a cloud session's proxy. A claude.ai connector's
    /// can: its page is claude.ai's.
    pub fn can_sign_in(&self) -> bool {
        self.status == "needs-auth" && !self.proxied
    }
}

impl Folio {
    /// `mcp_status`: the line to write. The answer fills
    /// [`McpState::servers`] and the cards that show them.
    pub fn mcp_status(&mut self) -> String {
        self.mcp.asked = true;
        self.mcp.stale = false;
        if self.mcp.waiting() {
            self.mcp.polls += 1;
        }
        let id = self.request(Sent::McpStatus);
        control_request_line(&id, "mcp_status", serde_json::json!({}))
    }

    /// Whether the view should ask for status, and when. Never while an
    /// ask is on its way, so a slow CLI is not asked twice.
    pub fn mcp_wanted(&self) -> McpWant {
        if self.requests.values().any(|s| *s == Sent::McpStatus) {
            return McpWant::No;
        }
        if !self.mcp.asked || self.mcp.stale {
            return McpWant::Now;
        }
        if self.mcp.waiting() && self.mcp.polls < POLLS {
            return McpWant::Soon;
        }
        McpWant::No
    }

    /// `/mcp`: a card with every server, and the line asking for their
    /// status, so it shows what is true now.
    pub fn show_mcp(&mut self) -> String {
        self.blocks.push(Block::Mcp(McpCard {
            servers: self.mcp.servers.clone(),
            all: true,
        }));
        self.mcp_status()
    }

    /// Starts signing in to `server`: the line to write. Its answer puts
    /// the sign-in page in [`McpState::to_open`].
    pub fn mcp_sign_in(&mut self, server: &str) -> String {
        self.mcp.polls = 0;
        self.mcp_mark(server, |s| {
            s.sign_in = Some(String::new());
            s.error.clear();
        });
        let id = self.request(Sent::McpAuth(server.to_string()));
        control_request_line(
            &id,
            "mcp_authenticate",
            serde_json::json!({"serverName": server}),
        )
    }

    /// Stops waiting on a sign-in the human gave up on.
    pub fn mcp_sign_in_cancel(&mut self, server: &str) {
        self.mcp_mark(server, |s| s.sign_in = None);
    }

    /// `mcp_reconnect`: the line to write.
    pub fn mcp_reconnect(&mut self, server: &str) -> String {
        self.mcp_mark(server, |s| s.busy = true);
        let id = self.request(Sent::McpReconnect(server.to_string()));
        control_request_line(
            &id,
            "mcp_reconnect",
            serde_json::json!({"serverName": server}),
        )
    }

    /// `mcp_toggle`: the line to write.
    pub fn mcp_toggle(&mut self, server: &str, enabled: bool) -> String {
        self.mcp_mark(server, |s| s.busy = true);
        let id = self.request(Sent::McpToggle(server.to_string()));
        control_request_line(
            &id,
            "mcp_toggle",
            serde_json::json!({"serverName": server, "enabled": enabled}),
        )
    }

    /// Asks the human to be sure before removing `server`, or stops
    /// asking.
    pub fn mcp_ask_remove(&mut self, server: &str, asking: bool) {
        self.mcp_mark(server, |s| {
            s.asking_remove = asking;
            s.error.clear();
        });
    }

    /// Removing `server` began: it shows as busy until [`Self::mcp_removed`].
    pub fn mcp_removing(&mut self, server: &str) {
        self.mcp_mark(server, |s| {
            s.asking_remove = false;
            s.busy = true;
        });
    }

    /// What removing `server` came to. Once removed it is turned off in
    /// this session too, so it stops asking now: the line to write.
    pub fn mcp_removed(&mut self, server: &str, result: Result<(), String>) -> Option<String> {
        match result {
            Ok(()) => {
                self.mcp_mark(server, |s| {
                    s.removed = true;
                    s.error.clear();
                });
                Some(self.mcp_toggle(server, false))
            }
            Err(e) => {
                self.mcp_mark(server, |s| {
                    s.busy = false;
                    s.error = e.clone();
                });
                None
            }
        }
    }

    /// Changes `server` in the list and in every card showing it.
    fn mcp_mark(&mut self, server: &str, f: impl Fn(&mut McpServer)) {
        for s in self.mcp.servers.iter_mut().filter(|s| s.name == server) {
            f(s);
        }
        for b in &mut self.blocks {
            if let Block::Mcp(card) = b {
                for s in card.servers.iter_mut().filter(|s| s.name == server) {
                    f(s);
                }
            }
        }
    }

    /// A `system/init`: its statuses, with what an earlier `mcp_status`
    /// said kept while a server's status holds. Returns whether a block
    /// changed.
    pub(super) fn mcp_init(&mut self) -> bool {
        let old = std::mem::take(&mut self.mcp.servers);
        let servers = (self.info.mcp_servers.iter())
            .map(|s| match old.iter().find(|o| o.name == s.name) {
                Some(o) if o.status == s.status => o.clone(),
                Some(o) => McpServer {
                    status: s.status.clone(),
                    transport: o.transport.clone(),
                    proxied: o.proxied,
                    source: o.source.clone(),
                    removed: o.removed,
                    sign_in: o.sign_in.clone().filter(|_| s.status == "needs-auth"),
                    ..s.clone()
                },
                None => s.clone(),
            })
            .collect();
        // A failed server's init says nothing of why: ask, so its card
        // can.
        if (self.info.mcp_servers.iter()).any(|s| troubled(&s.status) && s.error.is_empty()) {
            let new = |s: &McpServer| !old.iter().any(|o| o.name == s.name && o.status == s.status);
            if self.info.mcp_servers.iter().any(new) {
                self.mcp.stale = true;
            }
        }
        self.mcp_heard(servers)
    }

    /// Takes in the servers as they are now: cards follow, and a server
    /// newly in trouble gets one. Returns whether a block changed.
    fn mcp_heard(&mut self, mut servers: Vec<McpServer>) -> bool {
        for s in &mut servers {
            let was = self.mcp.servers.iter().find(|o| o.name == s.name);
            // The sign-in in progress stays until the server says it is
            // through, or the wait gives up.
            if s.status == "needs-auth" && s.sign_in.is_none() {
                s.sign_in = was.and_then(|o| o.sign_in.clone());
            }
            if s.status != "needs-auth" {
                s.sign_in = None;
            }
            // An error from signing in or reconnecting stays until the
            // status moves.
            if let Some(o) = was.filter(|o| o.status == s.status) {
                if s.error.is_empty() {
                    s.error = o.error.clone();
                }
                s.sign_in_failed = o.sign_in_failed;
            }
            s.busy = was.is_some_and(|o| o.busy);
            s.asking_remove = was.is_some_and(|o| o.asking_remove);
            s.removed = was.is_some_and(|o| o.removed);
            if s.source.is_empty() {
                s.source = was.map(|o| o.source.clone()).unwrap_or_default();
            }
        }
        if self.mcp.polls >= POLLS {
            for s in &mut servers {
                s.sign_in = None;
            }
        }
        self.mcp.servers = servers;
        let mut changed = false;
        for b in &mut self.blocks {
            let Block::Mcp(card) = b else {
                continue;
            };
            let now: Vec<McpServer> = if card.all {
                self.mcp.servers.clone()
            } else {
                (card.servers.iter())
                    .map(|c| {
                        let found = self.mcp.servers.iter().find(|s| s.name == c.name);
                        found.cloned().unwrap_or_else(|| c.clone())
                    })
                    .collect()
            };
            if now != card.servers {
                card.servers = now;
                changed = true;
            }
        }
        let fresh: Vec<McpServer> = (self.mcp.servers.iter())
            .filter(|s| troubled(&s.status) && !self.mcp.warned.contains(&s.name))
            .cloned()
            .collect();
        // A sign-in that went through and failed later says so again.
        let fine: Vec<String> = (self.mcp.servers.iter())
            .filter(|s| s.status == "connected")
            .map(|s| s.name.clone())
            .collect();
        for name in fine {
            self.mcp.warned.remove(&name);
        }
        if fresh.is_empty() {
            return changed;
        }
        self.mcp.warned.extend(fresh.iter().map(|s| s.name.clone()));
        self.blocks.push(Block::Mcp(McpCard {
            servers: fresh,
            all: false,
        }));
        true
    }

    /// The answer to a request of ours about MCP. Returns whether a block
    /// changed.
    pub(super) fn mcp_response(&mut self, kind: Sent, r: &Value) -> bool {
        let error = (r["subtype"] == "error").then(|| r["error"].as_str().unwrap_or("refused"));
        let body = &r["response"];
        match kind {
            Sent::McpStatus => {
                let Some(list) = body["mcpServers"].as_array() else {
                    return false;
                };
                let servers = list.iter().map(McpServer::from_status).collect();
                self.mcp_heard(servers)
            }
            Sent::McpAuth(server) => {
                let url = body["authUrl"].as_str().map(str::to_string);
                match (error, url) {
                    (Some(e), _) => {
                        let e = e.to_string();
                        self.mcp_mark(&server, |s| {
                            s.sign_in = None;
                            s.error = e.clone();
                            s.sign_in_failed = true;
                        });
                    }
                    (None, Some(url)) => {
                        self.mcp.to_open.push(url.clone());
                        self.mcp_mark(&server, |s| s.sign_in = Some(url.clone()));
                    }
                    // Signed in already: nothing to open, the status says.
                    (None, None) => {
                        self.mcp_mark(&server, |s| s.sign_in = None);
                        self.mcp.stale = true;
                    }
                }
                true
            }
            Sent::McpReconnect(server) | Sent::McpToggle(server) => {
                let e = error.unwrap_or_default().to_string();
                self.mcp_mark(&server, |s| {
                    s.busy = false;
                    if !e.is_empty() {
                        s.error = e.clone();
                    }
                });
                self.mcp.stale = true;
                true
            }
            _ => false,
        }
    }

    /// One of our MCP requests, replayed from what went to the CLI.
    pub(super) fn mcp_sent(&mut self, subtype: &str, request: &Value) -> Option<Sent> {
        let server = text(request, "serverName");
        Some(match subtype {
            "mcp_status" => {
                self.mcp.asked = true;
                Sent::McpStatus
            }
            "mcp_authenticate" => Sent::McpAuth(server),
            "mcp_reconnect" => Sent::McpReconnect(server),
            "mcp_toggle" => Sent::McpToggle(server),
            _ => return None,
        })
    }

    /// The sign-in pages to open now, oldest first.
    pub fn take_mcp_pages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.mcp.to_open)
    }

    /// An MCP server asks the human something. Returns whether a block
    /// changed.
    pub(super) fn elicitation(&mut self, request_id: String, r: &Value) -> bool {
        self.blocks
            .push(Block::Elicitation(Elicitation::from_request(request_id, r)));
        true
    }

    /// Answers an elicitation: `content` goes with an accept of a form.
    /// Returns the line to write, None when it was answered already or is
    /// not asked any more.
    pub fn answer_elicitation(
        &mut self,
        request_id: &str,
        action: ElicitAction,
        content: Option<Map<String, Value>>,
    ) -> Option<String> {
        let e = self.elicitation_mut(request_id)?;
        if e.state != AskState::Waiting {
            return None;
        }
        e.settle(action);
        Some(elicitation_response(request_id, action, content))
    }

    fn elicitation_mut(&mut self, request_id: &str) -> Option<&mut Elicitation> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Elicitation(e) if e.request_id == request_id => Some(e),
            _ => None,
        })
    }

    /// The elicitations waiting on the human, by request id.
    pub fn pending_elicitations(&self) -> Vec<&str> {
        (self.blocks.iter())
            .filter_map(|b| match b {
                Block::Elicitation(e) if e.state == AskState::Waiting => {
                    Some(e.request_id.as_str())
                }
                _ => None,
            })
            .collect()
    }

    /// A replayed answer to an elicitation. Returns whether it was one.
    pub(super) fn elicitation_answered(&mut self, request_id: &str, body: &Value) -> bool {
        let Some(action) = body["action"].as_str().and_then(ElicitAction::parse) else {
            return false;
        };
        let Some(e) = self.elicitation_mut(request_id) else {
            return false;
        };
        if e.state != AskState::Waiting {
            return false;
        }
        e.settle(action);
        true
    }

    /// The CLI withdrew an elicitation, or its turn ended.
    pub(super) fn elicitation_withdrawn(&mut self, request_id: Option<&str>) -> bool {
        let mut changed = false;
        for b in &mut self.blocks {
            if let Block::Elicitation(e) = b {
                if e.state == AskState::Waiting && request_id.is_none_or(|r| r == e.request_id) {
                    e.state = AskState::NotNeeded;
                    changed = true;
                }
            }
        }
        changed
    }

    /// `system/elicitation_complete`: a server says a url elicitation is
    /// through.
    pub(super) fn elicitation_complete(&mut self, v: &Value) -> bool {
        let id = v["elicitation_id"].as_str().unwrap_or_default();
        let found = self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Elicitation(e) if !id.is_empty() && e.elicitation_id == id => Some(e),
            _ => None,
        });
        match found {
            Some(e) if !e.complete => {
                e.complete = true;
                true
            }
            _ => false,
        }
    }
}

impl McpState {
    /// Whether something waits on a status to change: a sign-in in the
    /// browser, a server still starting.
    fn waiting(&self) -> bool {
        (self.servers.iter()).any(|s| s.sign_in.is_some() || s.status == "pending")
    }
}

/// How the human answers an elicitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitAction {
    Accept,
    /// No, to what was asked.
    Decline,
    /// Closed without saying.
    Cancel,
}

impl ElicitAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ElicitAction::Accept => "accept",
            ElicitAction::Decline => "decline",
            ElicitAction::Cancel => "cancel",
        }
    }

    fn parse(s: &str) -> Option<ElicitAction> {
        Some(match s {
            "accept" => ElicitAction::Accept,
            "decline" => ElicitAction::Decline,
            "cancel" => ElicitAction::Cancel,
            _ => return None,
        })
    }
}

/// The `control_response` answering an elicitation.
pub fn elicitation_response(
    request_id: &str,
    action: ElicitAction,
    content: Option<Map<String, Value>>,
) -> String {
    let mut r = serde_json::json!({"action": action.as_str()});
    if let (ElicitAction::Accept, Some(c)) = (action, content) {
        r["content"] = Value::Object(c);
    }
    control_success(request_id, r)
}

/// Between the values of a field that takes several, in the text the
/// view keeps for it: no value a server lists holds it.
pub const ELICIT_SEP: char = '\u{1f}';

/// What an MCP server asks of the human, as a card.
#[derive(Debug, Clone, PartialEq)]
pub struct Elicitation {
    pub request_id: String,
    pub server: String,
    pub message: String,
    /// From the server's permission display, when it gave one.
    pub title: String,
    /// A url elicitation's page; None for a form.
    pub url: Option<String>,
    pub elicitation_id: String,
    pub fields: Vec<ElicitField>,
    /// Allowed is accepted, Denied declined, NotNeeded cancelled or
    /// withdrawn.
    pub state: AskState,
    /// What the human said, once answered.
    pub said: Option<ElicitAction>,
    /// A url one the server said is through.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElicitField {
    pub name: String,
    /// Its title, else its name.
    pub label: String,
    pub description: String,
    pub kind: FieldKind,
    pub required: bool,
    /// What it holds until the human changes it, as the view's text.
    pub default: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text {
        /// `email`, `uri`, `date`, `date-time`, or empty.
        format: String,
        min_len: Option<usize>,
        max_len: Option<usize>,
    },
    Number {
        integer: bool,
        min: Option<f64>,
        max: Option<f64>,
    },
    Boolean,
    /// One of these, by value with its label.
    Choice(Vec<(String, String)>),
    /// Several of these.
    Choices {
        options: Vec<(String, String)>,
        min: Option<usize>,
        max: Option<usize>,
    },
}

/// A schema's options: `enum` with `enumNames`, or `oneOf`/`anyOf` of
/// `const` with `title`.
fn options(v: &Value) -> Option<Vec<(String, String)>> {
    if let Some(values) = v["enum"].as_array() {
        let names = v["enumNames"].as_array();
        return Some(
            (values.iter().enumerate())
                .filter_map(|(i, x)| {
                    let value = x.as_str()?.to_string();
                    let label = (names.and_then(|n| n.get(i)?.as_str()))
                        .map_or_else(|| value.clone(), str::to_string);
                    Some((value, label))
                })
                .collect(),
        );
    }
    let list = v["oneOf"].as_array().or(v["anyOf"].as_array())?;
    Some(
        list.iter()
            .filter_map(|o| {
                let value = o["const"].as_str()?.to_string();
                let label = o["title"].as_str().map_or_else(|| value.clone(), str::to_string);
                Some((value, label))
            })
            .collect(),
    )
}

fn count(v: &Value, k: &str) -> Option<usize> {
    v[k].as_u64().map(|n| n as usize)
}

impl ElicitField {
    fn parse(name: &str, v: &Value, required: bool) -> ElicitField {
        let kind = match v["type"].as_str().unwrap_or_default() {
            "boolean" => FieldKind::Boolean,
            "number" | "integer" => FieldKind::Number {
                integer: v["type"] == "integer",
                min: v["minimum"].as_f64(),
                max: v["maximum"].as_f64(),
            },
            "array" => FieldKind::Choices {
                options: options(&v["items"]).unwrap_or_default(),
                min: count(v, "minItems"),
                max: count(v, "maxItems"),
            },
            _ => match options(v) {
                Some(o) => FieldKind::Choice(o),
                None => FieldKind::Text {
                    format: text(v, "format"),
                    min_len: count(v, "minLength"),
                    max_len: count(v, "maxLength"),
                },
            },
        };
        let default = match &v["default"] {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Array(a) => (a.iter().filter_map(Value::as_str))
                .collect::<Vec<_>>()
                .join(&ELICIT_SEP.to_string()),
            _ => String::new(),
        };
        let title = text(v, "title");
        ElicitField {
            name: name.to_string(),
            label: if title.is_empty() {
                name.to_string()
            } else {
                title
            },
            description: text(v, "description"),
            kind,
            required,
            default,
        }
    }

    /// The value the human's `raw` text makes, None when it is left
    /// empty, or why it will not do.
    pub fn value(&self, raw: &str) -> Result<Option<Value>, String> {
        let raw = raw.trim();
        let empty = match &self.kind {
            FieldKind::Choices { .. } => raw.split(ELICIT_SEP).all(|s| s.trim().is_empty()),
            _ => raw.is_empty(),
        };
        if empty {
            return match self.required {
                true if matches!(self.kind, FieldKind::Boolean) => Ok(Some(Value::Bool(false))),
                true => Err(format!("{} is needed.", self.label)),
                false => Ok(None),
            };
        }
        let label = &self.label;
        let v = match &self.kind {
            FieldKind::Text {
                format,
                min_len,
                max_len,
            } => {
                let n = raw.chars().count();
                if min_len.is_some_and(|m| n < m) {
                    return Err(format!("{label} needs at least {} characters.", min_len.unwrap_or(0)));
                }
                if max_len.is_some_and(|m| n > m) {
                    return Err(format!("{label} takes at most {} characters.", max_len.unwrap_or(0)));
                }
                if format == "email" && !raw.contains('@') {
                    return Err(format!("{label} should be an email address."));
                }
                if format == "uri" && !raw.contains(':') {
                    return Err(format!("{label} should be an address, such as https://\u{2026}"));
                }
                Value::String(raw.to_string())
            }
            FieldKind::Number { integer, min, max } => {
                let n: f64 = raw
                    .parse()
                    .map_err(|_| format!("{label} should be a number."))?;
                if *integer && n.fract() != 0.0 {
                    return Err(format!("{label} should be a whole number."));
                }
                if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                    let range = match (min, max) {
                        (Some(a), Some(b)) => format!("from {a} to {b}"),
                        (Some(a), None) => format!("at least {a}"),
                        (None, Some(b)) => format!("at most {b}"),
                        _ => String::new(),
                    };
                    return Err(format!("{label} should be {range}."));
                }
                if *integer {
                    Value::from(n as i64)
                } else {
                    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
                }
            }
            FieldKind::Boolean => Value::Bool(raw == "true"),
            FieldKind::Choice(options) => {
                if !options.iter().any(|(v, _)| v == raw) {
                    return Err(format!("{label}: pick one of the options."));
                }
                Value::String(raw.to_string())
            }
            FieldKind::Choices { options, min, max } => {
                let picked: Vec<Value> = (raw.split(ELICIT_SEP))
                    .map(str::trim)
                    .filter(|s| options.iter().any(|(v, _)| v == s))
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                if min.is_some_and(|m| picked.len() < m) {
                    return Err(format!("{label}: pick at least {}.", min.unwrap_or(0)));
                }
                if max.is_some_and(|m| picked.len() > m) {
                    return Err(format!("{label}: pick at most {}.", max.unwrap_or(0)));
                }
                Value::Array(picked)
            }
        };
        Ok(Some(v))
    }
}

impl Elicitation {
    /// From an `elicitation` control request's `request`.
    pub fn from_request(request_id: String, r: &Value) -> Elicitation {
        let schema = &r["requested_schema"];
        let required: Vec<&str> = (schema["required"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        let props = schema["properties"].as_object();
        // The order the server listed them in, which a JSON map may lose.
        let mut order: Vec<String> = (r["property_order"].as_array().into_iter().flatten())
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        for k in props.into_iter().flat_map(|p| p.keys()) {
            if !order.contains(k) {
                order.push(k.clone());
            }
        }
        let fields = (order.iter())
            .filter_map(|k| {
                let v = props?.get(k)?;
                Some(ElicitField::parse(k, v, required.contains(&k.as_str())))
            })
            .collect();
        let url = (r["mode"] == "url")
            .then(|| r["url"].as_str().map(str::to_string))
            .flatten();
        let title = r["title"].as_str().or(r["display_name"].as_str());
        Elicitation {
            request_id,
            server: text(r, "mcp_server_name"),
            message: text(r, "message"),
            title: title.unwrap_or_default().to_string(),
            url,
            elicitation_id: text(r, "elicitation_id"),
            fields,
            state: AskState::Waiting,
            said: None,
            complete: false,
        }
    }

    /// The content an accept sends, from the text the view keeps for each
    /// field by name (None where the human left it alone, so its default
    /// holds), or the first reason it will not do.
    pub fn content(
        &self,
        raw: impl Fn(&str) -> Option<String>,
    ) -> Result<Map<String, Value>, String> {
        let mut out = Map::new();
        for f in &self.fields {
            let typed = raw(&f.name).unwrap_or_else(|| f.default.clone());
            if let Some(v) = f.value(&typed)? {
                out.insert(f.name.clone(), v);
            }
        }
        Ok(out)
    }

    fn settle(&mut self, action: ElicitAction) {
        self.said = Some(action);
        self.state = match action {
            ElicitAction::Accept => AskState::Allowed,
            ElicitAction::Decline => AskState::Denied,
            ElicitAction::Cancel => AskState::NotNeeded,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Recorded from 2.1.314 against a stdio server of our own.
    const FORM: &str = r#"{"type":"control_request","request_id":"88c6acaf","request":{"subtype":"elicitation","mcp_server_name":"elicit","message":"Tell me about the deploy","mode":"form","requested_schema":{"type":"object","properties":{"env":{"type":"string","title":"Environment","enum":["staging","production"],"enumNames":["Staging","Production"]},"replicas":{"type":"integer","title":"Replicas","minimum":1,"maximum":9,"default":2},"notify":{"type":"boolean","title":"Notify team","default":true},"note":{"type":"string","title":"Note","description":"Anything else"}},"required":["env","replicas"]},"property_order":["env","replicas","notify","note"]}}"#;
    const URL: &str = r#"{"type":"control_request","request_id":"2a10377c","request":{"subtype":"elicitation","mcp_server_name":"elicit","message":"Sign in to Example","mode":"url","url":"https://example.com/signin","elicitation_id":"el-1"}}"#;
    const STATUS: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"ID","response":{"mcpServers":[{"name":"elicit","status":"connected","serverInfo":{"name":"elicit","version":"0.1"},"config":{"type":"stdio","command":"python","args":[]},"transport":"stdio","scope":"dynamic","source":"dynamic","tools":[{"name":"ask_form","annotations":{}},{"name":"ask_url","annotations":{}}]},{"name":"broken","status":"failed","error":"Connection closed","config":{"type":"stdio","command":"nope","args":[]},"transport":"stdio","scope":"dynamic","source":"dynamic"},{"name":"remote","status":"needs-auth","config":{"type":"http","url":"https://mcp.example.com/mcp"},"transport":"http","scope":"dynamic","source":"dynamic"}]}}}"#;

    fn id_of(line: &str) -> String {
        serde_json::from_str::<Value>(line).unwrap()["request_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn answer(f: &mut Folio, line: &str, body: Value) {
        let id = id_of(line);
        f.feed(&json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": id, "response": body}}).to_string());
    }

    fn cards(f: &Folio) -> Vec<&McpCard> {
        (f.blocks.iter())
            .filter_map(|b| match b {
                Block::Mcp(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_form_is_read_in_the_server_s_order_and_answered() {
        let mut f = Folio::new();
        assert!(f.feed(FORM));
        assert!(f.take_outbox().is_empty(), "the human answers, not the Folio");
        let Some(Block::Elicitation(e)) = f.blocks.last() else {
            panic!("no card");
        };
        assert_eq!(e.server, "elicit");
        assert_eq!(e.url, None);
        let names: Vec<&str> = e.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["env", "replicas", "notify", "note"]);
        assert_eq!(
            e.fields[0].kind,
            FieldKind::Choice(vec![
                ("staging".into(), "Staging".into()),
                ("production".into(), "Production".into())
            ])
        );
        assert!(e.fields[1].required);
        assert_eq!(e.fields[1].default, "2");
        assert_eq!(e.fields[2].kind, FieldKind::Boolean);
        assert_eq!(e.fields[2].default, "true");

        // Required and unpicked will not do.
        assert_eq!(e.content(|_| None).unwrap_err(), "Environment is needed.");
        let typed = |k: &str| match k {
            "env" => Some("staging".to_string()),
            "replicas" => Some("3".to_string()),
            "notify" => Some("false".to_string()),
            _ => None,
        };
        let content = e.content(typed).unwrap();
        assert_eq!(
            Value::Object(content.clone()),
            json!({"env": "staging", "replicas": 3, "notify": false})
        );
        assert_eq!(f.pending_elicitations(), ["88c6acaf"]);

        let line = f
            .answer_elicitation("88c6acaf", ElicitAction::Accept, Some(content))
            .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["response"]["request_id"], "88c6acaf");
        assert_eq!(v["response"]["response"]["action"], "accept");
        assert_eq!(v["response"]["response"]["content"]["replicas"], 3);
        assert!(f.pending_elicitations().is_empty());
        assert!(f
            .answer_elicitation("88c6acaf", ElicitAction::Decline, None)
            .is_none());
    }

    #[test]
    fn fields_check_what_is_typed() {
        let field = |v: Value, required: bool| ElicitField::parse("x", &v, required);
        let n = field(json!({"type": "integer", "minimum": 1, "maximum": 9}), false);
        assert_eq!(n.value("4"), Ok(Some(json!(4))));
        assert_eq!(n.value(""), Ok(None));
        assert!(n.value("4.5").is_err());
        assert!(n.value("12").is_err());
        assert!(n.value("four").is_err());
        let r = field(json!({"type": "number"}), false);
        assert_eq!(r.value("2.5"), Ok(Some(json!(2.5))));
        let e = field(json!({"type": "string", "format": "email"}), true);
        assert!(e.value("nobody").is_err());
        assert!(e.value("").is_err());
        let s = field(json!({"type": "string", "maxLength": 3}), false);
        assert!(s.value("abcd").is_err());
        let b = field(json!({"type": "boolean"}), true);
        assert_eq!(b.value(""), Ok(Some(json!(false))));
        let titled = field(
            json!({"type": "string", "oneOf": [{"const": "a", "title": "Apple"}, {"const": "b"}]}),
            false,
        );
        assert_eq!(
            titled.kind,
            FieldKind::Choice(vec![("a".into(), "Apple".into()), ("b".into(), "b".into())])
        );
        assert!(titled.value("c").is_err());
        let many = field(
            json!({"type": "array", "items": {"enum": ["a", "b", "c"]}, "maxItems": 2}),
            false,
        );
        let both = format!("a{ELICIT_SEP}c");
        assert_eq!(many.value(&both), Ok(Some(json!(["a", "c"]))));
        assert!(many.value(&format!("a{ELICIT_SEP}b{ELICIT_SEP}c")).is_err());
    }

    #[test]
    fn a_url_elicitation_is_accepted_without_content_and_completes() {
        let mut f = Folio::new();
        f.feed(URL);
        let line = f
            .answer_elicitation("2a10377c", ElicitAction::Accept, None)
            .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["response"]["response"], json!({"action": "accept"}));
        assert!(f.feed(
            r#"{"type":"system","subtype":"elicitation_complete","mcp_server_name":"elicit","elicitation_id":"el-1"}"#
        ));
        let Some(Block::Elicitation(e)) = f.blocks.last() else {
            panic!("no card");
        };
        assert_eq!(e.url.as_deref(), Some("https://example.com/signin"));
        assert!(e.complete);
        let declined = elicitation_response("r", ElicitAction::Decline, Some(Map::new()));
        let v: Value = serde_json::from_str(&declined).unwrap();
        assert_eq!(v["response"]["response"], json!({"action": "decline"}));
    }

    #[test]
    fn a_withdrawn_or_replayed_elicitation_settles() {
        let mut f = Folio::new();
        f.feed(FORM);
        assert!(f.feed(r#"{"type":"control_cancel_request","request_id":"88c6acaf"}"#));
        assert!(f.pending_elicitations().is_empty());

        let mut r = Folio::new();
        r.feed(URL);
        r.feed_sent(
            &elicitation_response("2a10377c", ElicitAction::Decline, None),
        );
        let Some(Block::Elicitation(e)) = r.blocks.last() else {
            panic!("no card");
        };
        assert_eq!(e.state, AskState::Denied);
    }

    #[test]
    fn status_shows_a_card_for_servers_in_trouble_once() {
        let mut f = Folio::new();
        assert_eq!(f.mcp_wanted(), McpWant::Now);
        let line = f.mcp_status();
        assert_eq!(f.mcp_wanted(), McpWant::No, "one ask at a time");
        let reply = STATUS.replace("\"ID\"", &format!("\"{}\"", id_of(&line)));
        assert!(f.feed(&reply));
        let c = cards(&f);
        assert_eq!(c.len(), 1);
        assert!(!c[0].all);
        let names: Vec<&str> = c[0].servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["broken", "remote"]);
        assert_eq!(c[0].servers[0].error, "Connection closed");
        assert!(c[0].servers[1].can_sign_in());
        assert_eq!(f.mcp.servers[0].tools, 2);

        // The same trouble at the next turn's init adds no card.
        f.feed(r#"{"type":"system","subtype":"init","mcp_servers":[{"name":"elicit","status":"connected"},{"name":"broken","status":"failed"},{"name":"remote","status":"needs-auth"}]}"#);
        assert_eq!(cards(&f).len(), 1);
        assert_eq!(f.mcp.servers[1].error, "Connection closed", "kept from status");
        assert_eq!(f.mcp_wanted(), McpWant::No);

        // `/mcp` lists them all.
        let line = f.show_mcp();
        assert!(cards(&f)[1].all);
        assert_eq!(cards(&f)[1].servers.len(), 3);
        assert!(line.contains("mcp_status"));
    }

    #[test]
    fn a_new_failure_at_init_asks_why() {
        let mut f = Folio::new();
        f.feed(r#"{"type":"system","subtype":"init","mcp_servers":[{"name":"x","status":"failed"}]}"#);
        assert_eq!(cards(&f).len(), 1);
        let line = f.mcp_status();
        answer(
            &mut f,
            &line,
            json!({"mcpServers": [{"name": "x", "status": "failed", "error": "spawn ENOENT"}]}),
        );
        assert_eq!(cards(&f)[0].servers[0].error, "spawn ENOENT");
    }

    #[test]
    fn signing_in_opens_the_page_and_waits_for_the_server() {
        let mut f = Folio::new();
        let line = f.mcp_status();
        let reply = STATUS.replace("\"ID\"", &format!("\"{}\"", id_of(&line)));
        f.feed(&reply);
        let sign_in = f.mcp_sign_in("remote");
        let v: Value = serde_json::from_str(&sign_in).unwrap();
        assert_eq!(v["request"]["subtype"], "mcp_authenticate");
        assert_eq!(v["request"]["serverName"], "remote");
        answer(
            &mut f,
            &sign_in,
            json!({"authUrl": "https://auth.example.com/authorize?x=1",
                "requiresUserAction": true, "callbackExpected": true, "callbackPort": 46445}),
        );
        assert_eq!(f.take_mcp_pages(), ["https://auth.example.com/authorize?x=1"]);
        assert_eq!(
            cards(&f)[0].servers[1].sign_in.as_deref(),
            Some("https://auth.example.com/authorize?x=1")
        );
        assert_eq!(f.mcp_wanted(), McpWant::Soon);

        // Still waiting: the sign-in stays.
        let line = f.mcp_status();
        let reply = STATUS.replace("\"ID\"", &format!("\"{}\"", id_of(&line)));
        f.feed(&reply);
        assert!(cards(&f)[0].servers[1].sign_in.is_some());

        // Through: the card says connected and stops asking.
        let line = f.mcp_status();
        answer(
            &mut f,
            &line,
            json!({"mcpServers": [{"name": "remote", "status": "connected", "transport": "http"}]}),
        );
        let s = &cards(&f)[0].servers[1];
        assert_eq!(s.status, "connected");
        assert_eq!(s.sign_in, None);
        assert_eq!(f.mcp_wanted(), McpWant::No);
    }

    #[test]
    fn a_server_is_removed_where_it_was_configured() {
        let of = |name: &str, source: &str| {
            McpServer::from_status(&json!({"name": name, "status": "needs-auth", "source": source}))
                .removal()
        };
        assert_eq!(
            of("plugin:context7:context7", "plugin"),
            Some(McpRemoval::Plugin("context7".into()))
        );
        assert_eq!(of("claude.ai Gmail", "claudeai"), Some(McpRemoval::ClaudeAi));
        assert_eq!(of("db", "user"), Some(McpRemoval::Scope("user".into())));
        assert_eq!(of("elicit", "dynamic"), None);
        assert_eq!(of("plugin:", "plugin"), None);
    }

    #[test]
    fn a_removed_server_is_turned_off_here_too() {
        let mut f = Folio::new();
        let line = f.mcp_status();
        let reply = STATUS.replace("\"ID\"", &format!("\"{}\"", id_of(&line)));
        f.feed(&reply);
        f.mcp_ask_remove("remote", true);
        f.mcp_removing("remote");
        let line = f.mcp_removed("remote", Ok(())).unwrap();
        assert!(line.contains("mcp_toggle") && line.contains("\"enabled\":false"));
        let s = f.mcp.servers.iter().find(|s| s.name == "remote").unwrap();
        assert!(s.removed && !s.asking_remove);
    }

    #[test]
    fn a_server_that_cannot_sign_in_says_why() {
        let mut f = Folio::new();
        let line = f.mcp_status();
        let reply = STATUS.replace("\"ID\"", &format!("\"{}\"", id_of(&line)));
        f.feed(&reply);
        let sign_in = f.mcp_sign_in("remote");
        f.feed(&json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": id_of(&sign_in),
            "error": "Server type \"stdio\" does not support OAuth authentication"}})
        .to_string());
        let s = &cards(&f)[0].servers[1];
        assert_eq!(s.sign_in, None);
        assert!(s.sign_in_failed);
        assert!(s.error.contains("does not support OAuth"));
        assert!(f.take_mcp_pages().is_empty());

        let again = f.mcp_reconnect("broken");
        f.feed(&json!({"type": "control_response", "response": {"subtype": "error",
            "request_id": id_of(&again), "error": "Connection closed"}})
        .to_string());
        assert!(!cards(&f)[0].servers[0].busy);
        assert_eq!(f.mcp_wanted(), McpWant::Now, "the status may have moved");
        let off = f.mcp_toggle("broken", false);
        let v: Value = serde_json::from_str(&off).unwrap();
        assert_eq!(v["request"]["enabled"], false);
    }

    #[test]
    fn a_replayed_status_ask_is_not_asked_again() {
        let mut f = Folio::new();
        f.feed_sent(
            r#"{"type":"control_request","request_id":"horadric-3","request":{"subtype":"mcp_status"}}"#,
        );
        assert_eq!(f.mcp_wanted(), McpWant::No);
        f.feed(&STATUS.replace("\"ID\"", "\"horadric-3\""));
        assert_eq!(cards(&f).len(), 1);
    }
}
