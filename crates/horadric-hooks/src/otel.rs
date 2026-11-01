//! Claude Code's OpenTelemetry export, received by the listener.
//!
//! The status line gives a session's running totals; the `api_request`
//! event gives each request, with its turn (`prompt.id`), tokens and cost.
//! A `claude` Horadric starts is pointed at the listener with OTLP over
//! HTTP as JSON, so no OpenTelemetry crate is needed: the JSON is read
//! here and nothing leaves the machine. Only logs are asked for, since
//! the events carry everything the metrics would.
//! <https://code.claude.com/docs/en/monitoring-usage>

use horadric_core::usage::Spent;
use serde_json::Value;

use crate::SESSION_HEADER;

/// Where OTLP over HTTP posts logs, under the endpoint.
pub const LOGS_PATH: &str = "/v1/logs";
/// Where it posts metrics. Not asked for, but a user who turns metrics on
/// for a session must not get errors from it, so it is answered.
pub const METRICS_PATH: &str = "/v1/metrics";

/// Any of these set means the person exports telemetry of their own, and
/// their settings win: Horadric adds nothing.
const THEIRS: &[&str] = &[
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "OTEL_LOGS_EXPORTER",
    "OTEL_METRICS_EXPORTER",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
];

/// The variables that point the `claude` of session `id` at the listener
/// on `port`, unless `set` says the person has telemetry of their own.
/// The session's tag rides in a header, as a hook's does.
pub fn env(id: &str, port: u16, set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    if THEIRS.iter().any(|v| set(v)) {
        return Vec::new();
    }
    [
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_string()),
        ("OTEL_LOGS_EXPORTER", "otlp".into()),
        ("OTEL_METRICS_EXPORTER", "none".into()),
        ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json".into()),
        (
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            format!("http://127.0.0.1:{port}{LOGS_PATH}"),
        ),
        (
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
            format!("{SESSION_HEADER}={id}"),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The `api_request` events in an OTLP JSON logs export, by the Claude
/// session (`session.id`) each came from, in the order they came.
pub fn api_requests(body: &[u8]) -> Vec<(String, Spent)> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for resource in list(&v, "resourceLogs") {
        let shared = resource.pointer("/resource/attributes");
        for scope in list(resource, "scopeLogs") {
            for record in list(scope, "logRecords") {
                let own = record.get("attributes");
                let attr = |key: &str| attribute(own, key).or_else(|| attribute(shared, key));
                let name = attr("event.name").and_then(text);
                let body = record.pointer("/body/stringValue").and_then(Value::as_str);
                let is_request = name.as_deref() == Some("api_request")
                    || body == Some("claude_code.api_request");
                if !is_request {
                    continue;
                }
                let int = |key: &str| attr(key).and_then(number).map_or(0, |n| n.max(0.0) as u64);
                let spent = Spent {
                    prompt: attr("prompt.id").and_then(text).unwrap_or_default(),
                    model: attr("model").and_then(text).unwrap_or_default(),
                    cost: attr("cost_usd")
                        .and_then(number)
                        .filter(|c| c.is_finite() && *c >= 0.0)
                        .unwrap_or(0.0),
                    input: int("input_tokens"),
                    output: int("output_tokens"),
                    cache_read: int("cache_read_tokens"),
                    cache_creation: int("cache_creation_tokens"),
                };
                let session = attr("session.id").and_then(text).unwrap_or_default();
                out.push((session, spent));
            }
        }
    }
    out
}

fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// An OTLP `KeyValue` list's value for `key`.
fn attribute<'a>(attrs: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    attrs?
        .as_array()?
        .iter()
        .find(|a| a.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")
}

fn text(v: &Value) -> Option<String> {
    v.get("stringValue")?.as_str().map(str::to_string)
}

/// OTLP JSON writes a 64 bit integer as a string, and Claude Code sends
/// some numbers as strings outright.
fn number(v: &Value) -> Option<f64> {
    ["doubleValue", "intValue", "stringValue"]
        .iter()
        .find_map(|k| v.get(*k))
        .and_then(|n| n.as_f64().or_else(|| n.as_str()?.trim().parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_is_pointed_here_unless_it_exports_its_own() {
        let env = env("tile-3", 43118, |_| false);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("CLAUDE_CODE_ENABLE_TELEMETRY"), Some("1"));
        assert_eq!(get("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL"), Some("http/json"));
        assert_eq!(
            get("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
            Some("http://127.0.0.1:43118/v1/logs")
        );
        assert_eq!(
            get("OTEL_EXPORTER_OTLP_LOGS_HEADERS"),
            Some("x-horadric-session=tile-3")
        );
        assert!(super::env("t", 1, |k| k == "OTEL_EXPORTER_OTLP_ENDPOINT").is_empty());
    }

    #[test]
    fn api_requests_are_read_from_an_export() {
        let body = br#"{"resourceLogs":[{
            "resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
            "scopeLogs":[{"logRecords":[
                {"body":{"stringValue":"claude_code.api_request"},"attributes":[
                    {"key":"event.name","value":{"stringValue":"api_request"}},
                    {"key":"session.id","value":{"stringValue":"abc"}},
                    {"key":"prompt.id","value":{"stringValue":"p1"}},
                    {"key":"model","value":{"stringValue":"claude-opus-5-5"}},
                    {"key":"cost_usd","value":{"doubleValue":0.25}},
                    {"key":"input_tokens","value":{"intValue":"120"}},
                    {"key":"output_tokens","value":{"stringValue":"40"}},
                    {"key":"cache_read_tokens","value":{"intValue":9000}}
                ]},
                {"body":{"stringValue":"claude_code.tool_result"},"attributes":[
                    {"key":"event.name","value":{"stringValue":"tool_result"}}
                ]}
            ]}]
        }]}"#;
        let got = api_requests(body);
        assert_eq!(got.len(), 1);
        let (session, s) = &got[0];
        assert_eq!(session, "abc");
        assert_eq!(s.prompt, "p1");
        assert_eq!(s.cost, 0.25);
        assert_eq!(
            (s.input, s.output, s.cache_read, s.cache_creation),
            (120, 40, 9000, 0)
        );
        assert!(api_requests(b"nope").is_empty());
    }
}
