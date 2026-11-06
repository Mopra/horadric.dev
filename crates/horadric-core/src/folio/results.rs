//! What a tool's result says, read into something a reader can take in:
//! the text without what was written for Claude alone (system reminders,
//! error tags, terminal colour codes), and for the common tools their
//! parts (a Read's lines and where they start, a Grep's files and hits, a
//! command's output streams and exit code).
//!
//! Each result comes as the `tool_result` block's text, and usually also
//! as the tool's own account of it (`tool_use_result` on stdout,
//! `toolUseResult` in a transcript). The account is cleaner, so it wins
//! where it has a field, and the text is parsed only where it is missing.

use std::borrow::Cow;
use std::sync::Arc;

use serde_json::Value;

/// How much of a result's text is kept for showing all of it. A Read is
/// at most 2000 lines and a command's output past this is not read anyway.
pub const RESULT_CAP: usize = 256 * 1024;

/// A step's result, cleaned and read.
///
/// The big texts sit behind an [`Arc`]: a folio is cloned and compared
/// often over a long session, and each step can hold a quarter megabyte.
/// Comparing two `Arc<str>` checks the pointer before the bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResult {
    /// The whole text, cleaned, at most [`RESULT_CAP`] bytes. Empty for a
    /// shell command whose streams hold its output, which would otherwise
    /// be kept twice; [`ToolResult::whole_text`] gives it either way.
    pub text: Arc<str>,
    /// Which end of `text` was cut to fit the cap.
    pub cut: Cut,
    /// The result is an error: the CLI marked it so, or it came wrapped
    /// in `<tool_use_error>`.
    pub error: bool,
    pub kind: ResultKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cut {
    #[default]
    None,
    /// The start was dropped: a command's output keeps its end.
    Start,
    /// The end was dropped: a file keeps its start.
    End,
}

impl ToolResult {
    /// The whole text: `text`, or for a shell command that keeps its
    /// output only in its streams, those streams joined.
    pub fn whole_text(&self) -> Cow<'_, str> {
        match &self.kind {
            ResultKind::Shell(sh) if self.text.is_empty() => match (sh.stdout.is_empty(), sh.stderr.is_empty()) {
                (_, true) => Cow::Borrowed(&sh.stdout),
                (true, false) => Cow::Borrowed(&sh.stderr),
                (false, false) => Cow::Owned(format!("{}\n{}", sh.stdout, sh.stderr)),
            },
            _ => Cow::Borrowed(&self.text),
        }
    }
}

/// The parts of a result, for the tools whose results have them.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ResultKind {
    /// Nothing more than the text.
    #[default]
    Plain,
    Read(ReadResult),
    Grep(GrepResult),
    Glob(GlobResult),
    /// Bash and PowerShell.
    Shell(ShellResult),
    WebSearch(WebSearchResult),
    Mcp(McpResult),
}

/// The lines a Read gave, without their number prefixes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadResult {
    pub path: String,
    /// The number of the first line, from 1.
    pub start: usize,
    pub lines: Vec<String>,
    /// How many lines the file has, where the CLI said.
    pub total: Option<usize>,
    /// Lines past the cap were dropped.
    pub cut: bool,
}

impl ReadResult {
    /// The number of the last line read, `start - 1` when none were.
    pub fn end(&self) -> usize {
        self.start + self.lines.len() - 1
    }

    /// What part of the file was read, as a few words: "lines 163 to 280",
    /// "all 120 lines".
    pub fn range_label(&self) -> String {
        let n = self.lines.len();
        if n == 0 {
            return "empty".into();
        }
        if self.start == 1 && self.total == Some(n) {
            return if n == 1 {
                "its one line".into()
            } else {
                format!("all {n} lines")
            };
        }
        if n == 1 {
            return format!("line {}", self.start);
        }
        format!("lines {} to {}", self.start, self.end())
    }
}

/// What a Grep found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GrepResult {
    /// `files_with_matches`, `content` or `count`, as the CLI names it.
    pub mode: String,
    /// The files it found, with their hits where known: counted lines in
    /// `content` mode, the count in `count` mode, none for file names alone.
    pub files: Vec<FileHits>,
    /// The lines found, in `content` mode.
    pub lines: Vec<GrepLine>,
    /// How many matches in all, where known.
    pub matches: Option<usize>,
    /// The CLI cut the list short at a `head_limit`.
    pub limited: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileHits {
    pub path: String,
    pub count: Option<usize>,
}

/// One line a Grep printed in `content` mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrepLine {
    /// Empty when one file was searched, so none was printed.
    pub file: String,
    pub line: Option<usize>,
    pub text: String,
    /// A line around a match (`-A`, `-B`, `-C`), not a match.
    pub context: bool,
    /// The `--` between groups of context.
    pub gap: bool,
}

/// What a Glob found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobResult {
    pub files: Vec<String>,
    /// How many matched, which is more than `files` when `truncated`.
    pub total: usize,
    pub truncated: bool,
}

/// What a shell command said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellResult {
    /// What Claude said the command is for.
    pub description: String,
    pub command: String,
    /// The end of what it printed. With `stderr` at most [`RESULT_CAP`]
    /// bytes together.
    pub stdout: Arc<str>,
    /// Empty where the record gives no separate stderr (an error, whose
    /// one text holds both). At most a quarter of [`RESULT_CAP`]: errors
    /// that long are mostly the same line again.
    pub stderr: Arc<str>,
    /// 0 for a command that succeeded, the code a failed one gave. None
    /// where it is not known: one stopped, sent to the background, or
    /// whose nonzero code the CLI took as no error (see `interpretation`).
    pub exit_code: Option<i32>,
    pub interrupted: bool,
    /// The task id, when it runs on in the background.
    pub background: Option<String>,
    /// How long it ran before it was moved to the background.
    pub timed_out_ms: Option<u64>,
    /// What the CLI read a nonzero code as, such as "No matches found".
    pub interpretation: String,
    /// Where the CLI saved output too large to hand Claude whole.
    pub saved_to: Option<String>,
}

/// What a WebSearch found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSearchResult {
    pub query: String,
    pub links: Vec<Link>,
    /// What the search said in words.
    pub summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    pub title: String,
    pub url: String,
}

/// The result of a tool an MCP server gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpResult {
    pub server: String,
    pub tool: String,
    /// The text pretty printed, when it is a JSON object or array.
    pub json: Option<String>,
}

/// Reads one result of `tool`, called with `input`. `text` is the
/// `tool_result` block's text, `failed` its `is_error`, `detail` the
/// tool's own account (Null where there is none).
pub fn read_result(
    tool: &str,
    input: &Value,
    text: &str,
    failed: bool,
    detail: &Value,
) -> ToolResult {
    let (clean, tagged) = clean_text(text);
    let error = failed || tagged;
    let shell = matches!(tool, "Bash" | "PowerShell");
    let (mut text, mut cut) = if shell {
        keep_end(&clean, RESULT_CAP)
    } else {
        keep_start(&clean, RESULT_CAP)
    };
    let kind = if error && !shell {
        ResultKind::Plain
    } else {
        match tool {
            "Read" => read_lines(input, &text, detail).map_or(ResultKind::Plain, ResultKind::Read),
            "Grep" => ResultKind::Grep(grep(&text, detail)),
            "Glob" => ResultKind::Glob(glob(&text, detail)),
            "Bash" | "PowerShell" => {
                let (sh, streams_cut) = shell_result(input, &text, error, detail);
                // The streams say what the text says, so it is kept once.
                // The text stays only where they are empty, as when the
                // command went to the background and the text says so.
                if !sh.stdout.is_empty() || !sh.stderr.is_empty() {
                    text.clear();
                    // Without an account stdout is the text, cut as it was.
                    if detail.is_object() {
                        cut = streams_cut;
                    }
                }
                ResultKind::Shell(sh)
            }
            "WebSearch" => ResultKind::WebSearch(web_search(input, &text, detail)),
            t if t.starts_with("mcp__") => ResultKind::Mcp(mcp(t, &text)),
            _ => ResultKind::Plain,
        }
    };
    ToolResult {
        text: text.into(),
        cut,
        error,
        kind,
    }
}

/// `text` without system reminders, terminal escape codes and carriage
/// returns, and with a `<tool_use_error>` wrapper taken off. Says whether
/// that wrapper was there.
pub fn clean_text(text: &str) -> (String, bool) {
    let text = strip_reminders(text);
    let text = strip_ansi(&text);
    let text = settle_returns(&text);
    let trimmed = text.trim();
    if let Some(inner) = trimmed.strip_prefix("<tool_use_error>") {
        let inner = inner.strip_suffix("</tool_use_error>").unwrap_or(inner);
        return (inner.trim().to_string(), true);
    }
    (text.trim_end().trim_start_matches('\n').to_string(), false)
}

fn strip_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(rest[..at].trim_end_matches([' ', '\t']));
        rest = match rest[at..].find(CLOSE) {
            Some(end) => &rest[at + end + CLOSE.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    // A reminder leaves its blank lines behind.
    let mut joined = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.split('\n') {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        if !joined.is_empty() || !line.trim().is_empty() {
            joined.push_str(line);
            joined.push('\n');
        }
    }
    joined.pop();
    joined
}

/// Terminal escape sequences: CSI (`ESC [ ... final`), OSC (`ESC ] ...`
/// ended by BEL or `ESC \`) and the two byte ones.
fn strip_ansi(text: &str) -> String {
    if !text.contains('\u{1b}') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// CRLF as LF, and a line redrawn with bare carriage returns (a progress
/// bar) as what it showed last.
fn settle_returns(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// At most `max` bytes from the start of `s`, at a line end where one is
/// near.
fn keep_start(s: &str, max: usize) -> (String, Cut) {
    if s.len() <= max {
        return (s.to_string(), Cut::None);
    }
    let mut at = max;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    let at = s[..at].rfind('\n').unwrap_or(at);
    (s[..at].to_string(), Cut::End)
}

/// At most `max` bytes from the end of `s`, at a line start where one is
/// near.
fn keep_end(s: &str, max: usize) -> (String, Cut) {
    if s.len() <= max {
        return (s.to_string(), Cut::None);
    }
    let mut at = s.len() - max;
    while !s.is_char_boundary(at) {
        at += 1;
    }
    let at = s[at..].find('\n').map_or(at, |n| at + n + 1);
    (s[at..].to_string(), Cut::Start)
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn usize_of(v: &Value, key: &str) -> Option<usize> {
    v[key].as_u64().map(|n| n as usize)
}

fn read_lines(input: &Value, text: &str, detail: &Value) -> Option<ReadResult> {
    let file = &detail["file"];
    if detail["type"] == "text" {
        if let Some(content) = file["content"].as_str() {
            let (content, cut) = keep_start(content, RESULT_CAP);
            return Some(ReadResult {
                path: file["filePath"]
                    .as_str()
                    .map_or_else(|| str_of(input, "file_path"), str::to_string),
                start: usize_of(file, "startLine").unwrap_or(1).max(1),
                lines: if content.is_empty() {
                    Vec::new()
                } else {
                    content.split('\n').map(str::to_string).collect()
                },
                total: usize_of(file, "totalLines"),
                cut: cut != Cut::None,
            });
        }
    }
    if !detail.is_null() {
        // An image, a notebook or a PDF: not lines.
        return None;
    }
    let mut read = numbered_lines(text)?;
    read.path = str_of(input, "file_path");
    Some(read)
}

/// The `   12→text` (or `12\ttext`) lines a Read hands Claude.
fn numbered_lines(text: &str) -> Option<ReadResult> {
    let mut start = None;
    let mut lines = Vec::new();
    for line in text.lines() {
        let body = line.trim_start_matches(' ');
        let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let rest = &body[digits..];
        let text = rest
            .strip_prefix('\u{2192}')
            .or_else(|| rest.strip_prefix('\t'));
        match (digits, text) {
            (1.., Some(text)) => {
                start.get_or_insert_with(|| body[..digits].parse::<usize>().unwrap_or(1));
                lines.push(text.to_string());
            }
            _ => return None,
        }
    }
    Some(ReadResult {
        start: start?,
        lines,
        ..ReadResult::default()
    })
}

fn grep(text: &str, detail: &Value) -> GrepResult {
    let mode = match detail["mode"].as_str() {
        Some(m) => m.to_string(),
        None if text.starts_with("Found ") && text.contains(" file") => "files_with_matches".into(),
        None => "content".into(),
    };
    let body = match detail["content"].as_str() {
        Some(c) => c.to_string(),
        None => text.to_string(),
    };
    let limited = !detail["appliedLimit"].is_null();
    match mode.as_str() {
        "files_with_matches" => {
            let files: Vec<String> = match detail["filenames"].as_array() {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => text
                    .lines()
                    .skip_while(|l| l.starts_with("Found "))
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect(),
            };
            GrepResult {
                mode,
                files: files
                    .into_iter()
                    .map(|path| FileHits { path, count: None })
                    .collect(),
                lines: Vec::new(),
                matches: None,
                limited,
            }
        }
        "count" => {
            let files: Vec<FileHits> = body
                .lines()
                .filter_map(|l| {
                    let (path, n) = l.rsplit_once(':')?;
                    Some(FileHits {
                        path: path.to_string(),
                        count: Some(n.trim().parse().ok()?),
                    })
                })
                .collect();
            let matches = usize_of(detail, "numMatches")
                .or_else(|| Some(files.iter().filter_map(|f| f.count).sum()));
            GrepResult {
                mode,
                files,
                lines: Vec::new(),
                matches,
                limited,
            }
        }
        _ => {
            let lines: Vec<GrepLine> = body
                .lines()
                .filter(|l| !l.starts_with("Found ") && !l.is_empty())
                .map(grep_line)
                .collect();
            let mut files: Vec<FileHits> = Vec::new();
            for l in lines.iter().filter(|l| !l.context && !l.gap) {
                match files.last_mut() {
                    Some(f) if f.path == l.file => *f.count.get_or_insert(0) += 1,
                    _ => files.push(FileHits {
                        path: l.file.clone(),
                        count: Some(1),
                    }),
                }
            }
            files.retain(|f| !f.path.is_empty());
            let matches = lines.iter().filter(|l| !l.context && !l.gap).count();
            GrepResult {
                mode,
                files,
                lines,
                matches: Some(matches),
                limited,
            }
        }
    }
}

/// One `content` mode line: `file:12:text` for a match, `file-12-text`
/// for context, either without the file when one file was searched, and
/// `--` between groups.
fn grep_line(l: &str) -> GrepLine {
    if l == "--" {
        return GrepLine {
            gap: true,
            ..GrepLine::default()
        };
    }
    // A Windows drive's colon is not a separator.
    let skip = match l.as_bytes() {
        [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic() => 2,
        _ => 0,
    };
    for sep in [':', '-'] {
        if let Some((file, line, text)) = split_numbered(l, skip, sep) {
            return GrepLine {
                file: file.to_string(),
                line: Some(line),
                text: text.to_string(),
                context: sep == '-',
                gap: false,
            };
        }
    }
    GrepLine {
        text: l.to_string(),
        ..GrepLine::default()
    }
}

/// `file<sep>12<sep>text` or `12<sep>text`, the file found as what comes
/// before the first `<sep>digits<sep>` past `skip`.
fn split_numbered(l: &str, skip: usize, sep: char) -> Option<(&str, usize, &str)> {
    let number_then = |s: &str| -> Option<(usize, usize)> {
        let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || !s[digits..].starts_with(sep) {
            return None;
        }
        Some((s[..digits].parse().ok()?, digits))
    };
    if let Some((n, digits)) = number_then(l) {
        return Some(("", n, &l[digits + 1..]));
    }
    let mut from = skip;
    while let Some(at) = l[from..].find(sep) {
        let at = from + at;
        if let Some((n, digits)) = number_then(&l[at + 1..]) {
            return Some((&l[..at], n, &l[at + 1 + digits + 1..]));
        }
        from = at + 1;
    }
    None
}

fn glob(text: &str, detail: &Value) -> GlobResult {
    let files: Vec<String> = match detail["filenames"].as_array() {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => text
            .lines()
            .filter(|l| !l.trim().is_empty() && *l != "No files found" && !l.starts_with('('))
            .map(str::to_string)
            .collect(),
    };
    GlobResult {
        total: usize_of(detail, "totalMatches")
            .or_else(|| usize_of(detail, "numFiles"))
            .unwrap_or(files.len()),
        truncated: detail["truncated"].as_bool().unwrap_or(false),
        files,
    }
}

/// The command's parts, and whether its streams lost their start to the cap.
fn shell_result(input: &Value, text: &str, error: bool, detail: &Value) -> (ShellResult, Cut) {
    let mut r = ShellResult {
        description: str_of(input, "description"),
        command: str_of(input, "command"),
        ..ShellResult::default()
    };
    let mut cut = Cut::None;
    let mut stdout = if detail.is_object() {
        let mut stream = |key: &str, max: usize| {
            let (s, _) = clean_text(detail[key].as_str().unwrap_or_default());
            let (s, c) = keep_end(&s, max);
            if c != Cut::None {
                cut = c;
            }
            s
        };
        let stderr = stream("stderr", RESULT_CAP / 4);
        let stdout = stream("stdout", RESULT_CAP - stderr.len());
        r.stderr = stderr.into();
        r.interrupted = detail["interrupted"].as_bool().unwrap_or(false);
        r.background = detail["backgroundTaskId"].as_str().map(str::to_string);
        r.timed_out_ms = detail["timedOutAfterMs"].as_u64();
        r.interpretation = str_of(detail, "returnCodeInterpretation");
        r.saved_to = detail["persistedOutputPath"].as_str().map(str::to_string);
        stdout
    } else {
        text.to_string()
    };
    // A failed command's text starts with its code, then its output.
    if let Some(rest) = stdout.strip_prefix("Exit code ") {
        let (code, after) = rest.split_once('\n').unwrap_or((rest, ""));
        if let Ok(code) = code.trim().parse() {
            r.exit_code = Some(code);
            stdout = after.to_string();
        }
    }
    r.stdout = stdout.into();
    if r.exit_code.is_none()
        && !error
        && !r.interrupted
        && r.background.is_none()
        && r.interpretation.is_empty()
    {
        r.exit_code = Some(0);
    }
    (r, cut)
}

fn web_search(input: &Value, text: &str, detail: &Value) -> WebSearchResult {
    let mut r = WebSearchResult {
        query: detail["query"]
            .as_str()
            .map_or_else(|| str_of(input, "query"), str::to_string),
        ..WebSearchResult::default()
    };
    let link = |l: &Value| Link {
        title: str_of(l, "title"),
        url: str_of(l, "url"),
    };
    if let Some(results) = detail["results"].as_array() {
        let mut said = Vec::new();
        for item in results {
            match item {
                Value::String(s) => said.push(clean_text(s).0),
                v => r
                    .links
                    .extend(v["content"].as_array().into_iter().flatten().map(link)),
            }
        }
        r.summary = said.join("\n\n").trim().to_string();
        return r;
    }
    // The text: a heading, a `Links: [...]` line per search, then words.
    let mut said = Vec::new();
    for line in text.lines() {
        if let Some(json) = line.strip_prefix("Links: ") {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(json) {
                r.links.extend(a.iter().map(link));
                continue;
            }
        }
        if line.starts_with("Web search results for query:") {
            continue;
        }
        said.push(line);
    }
    r.summary = said.join("\n").trim().to_string();
    r
}

fn mcp(tool: &str, text: &str) -> McpResult {
    let name = tool.strip_prefix("mcp__").unwrap_or(tool);
    let (server, tool) = name.split_once("__").unwrap_or(("", name));
    let trimmed = text.trim();
    let json = (trimmed.starts_with('{') || trimmed.starts_with('['))
        .then(|| serde_json::from_str::<Value>(trimmed).ok())
        .flatten()
        .and_then(|v| serde_json::to_string_pretty(&v).ok());
    McpResult {
        server: server.to_string(),
        tool: tool.to_string(),
        json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reminders_are_taken_out() {
        let (s, err) = clean_text("one\n\n<system-reminder>\nsecret\n</system-reminder>\n\ntwo");
        assert_eq!(s, "one\n\ntwo");
        assert!(!err);
        let (s, _) = clean_text("<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>");
        assert_eq!(s, "");
        let (s, _) = clean_text("kept\n<system-reminder>never closed");
        assert_eq!(s, "kept");
    }

    #[test]
    fn a_tool_use_error_is_a_flag_and_its_words() {
        let (s, err) = clean_text("<tool_use_error>Blocked: sleep 240</tool_use_error>");
        assert_eq!(s, "Blocked: sleep 240");
        assert!(err);
        let r = read_result("Read", &json!({}), "<tool_use_error>File does not exist.</tool_use_error>", false, &Value::Null);
        assert!(r.error);
        assert_eq!(r.kind, ResultKind::Plain);
    }

    #[test]
    fn escape_codes_and_carriage_returns_go() {
        let (s, _) = clean_text("\u{1b}[31mred\u{1b}[0m and \u{1b}]0;title\u{7}plain\r\nnext");
        assert_eq!(s, "red and plain\nnext");
        let (s, _) = clean_text("10%\r50%\r100%\ndone");
        assert_eq!(s, "100%\ndone");
    }

    #[test]
    fn a_long_text_keeps_the_end_that_matters() {
        let s = "a\n".repeat(10);
        assert_eq!(keep_start(&s, 5), ("a\na".into(), Cut::End));
        assert_eq!(keep_end(&s, 5), ("a\na\n".into(), Cut::Start));
        let big = "x\n".repeat(RESULT_CAP);
        let r = read_result("Bash", &json!({}), &big, false, &Value::Null);
        assert_eq!(r.cut, Cut::Start);
        assert!(r.whole_text().len() <= RESULT_CAP);
    }

    #[test]
    fn a_shell_result_keeps_its_output_once() {
        let detail = json!({"stdout": "out", "stderr": "err", "interrupted": false});
        let r = read_result("Bash", &json!({}), "out\nerr", false, &detail);
        assert!(r.text.is_empty());
        assert_eq!(r.whole_text(), "out\nerr");
        // Without an account the text is read into stdout, not kept beside it.
        let r = read_result("Bash", &json!({}), "Exit code 1\nboom", true, &Value::Null);
        assert!(r.text.is_empty());
        assert_eq!(r.whole_text(), "boom");
        // With nothing in the streams the text is all there is.
        let detail = json!({"stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "b1"});
        let r = read_result("Bash", &json!({}), "Running in the background", false, &detail);
        assert_eq!(&*r.text, "Running in the background");
        assert_eq!(r.whole_text(), "Running in the background");
    }

    #[test]
    fn a_shell_result_stays_under_the_cap_in_all() {
        let big = "x\n".repeat(RESULT_CAP);
        let detail = json!({"stdout": big, "stderr": big, "interrupted": false});
        let r = read_result("Bash", &json!({}), &format!("{big}\n{big}"), false, &detail);
        let ResultKind::Shell(s) = &r.kind else { panic!() };
        assert!(s.stdout.len() + s.stderr.len() <= RESULT_CAP);
        assert!(s.stderr.len() <= RESULT_CAP / 4);
        assert_eq!(r.cut, Cut::Start);
        assert!(r.text.is_empty());
    }

    #[test]
    fn a_cloned_result_shares_its_text() {
        let r = read_result("Read", &json!({}), "some text", false, &Value::Null);
        let c = r.clone();
        assert!(Arc::ptr_eq(&r.text, &c.text));
        assert_eq!(r, c);
    }

    #[test]
    fn a_read_from_its_account() {
        let detail = json!({"type": "text", "file": {"filePath": "C:\\a\\b.rs", "content": "fn a() {}\n\nfn b() {}", "numLines": 3, "startLine": 163, "totalLines": 688}});
        let r = read_result("Read", &json!({"file_path": "x"}), "163\tfn a() {}", false, &detail);
        let ResultKind::Read(read) = r.kind else { panic!("{:?}", r.kind) };
        assert_eq!(read.path, "C:\\a\\b.rs");
        assert_eq!(read.start, 163);
        assert_eq!(read.lines, vec!["fn a() {}", "", "fn b() {}"]);
        assert_eq!(read.total, Some(688));
        assert_eq!(read.range_label(), "lines 163 to 165");
    }

    #[test]
    fn a_read_from_its_text() {
        let text = "     5\u{2192}first\n     6\u{2192}\n     7\u{2192}  third\n\n<system-reminder>\nmalware note\n</system-reminder>\n";
        let r = read_result("Read", &json!({"file_path": "C:/x/y.rs"}), text, false, &Value::Null);
        let ResultKind::Read(read) = r.kind else { panic!("{:?}", r.kind) };
        assert_eq!(read.path, "C:/x/y.rs");
        assert_eq!(read.start, 5);
        assert_eq!(read.lines, vec!["first", "", "  third"]);
        let tabbed = numbered_lines("395\t\n396\tfn set() {").unwrap();
        assert_eq!((tabbed.start, tabbed.lines.len()), (395, 2));
        assert!(numbered_lines("not a read").is_none());
    }

    #[test]
    fn range_labels() {
        let r = |start, n, total| ReadResult {
            start,
            lines: vec![String::new(); n],
            total,
            ..ReadResult::default()
        };
        assert_eq!(r(1, 120, Some(120)).range_label(), "all 120 lines");
        assert_eq!(r(1, 1, Some(1)).range_label(), "its one line");
        assert_eq!(r(40, 1, Some(90)).range_label(), "line 40");
        assert_eq!(r(1, 0, Some(0)).range_label(), "empty");
        assert_eq!(r(1, 50, Some(90)).range_label(), "lines 1 to 50");
    }

    #[test]
    fn grep_files() {
        let detail = json!({"mode": "files_with_matches", "filenames": ["a.rs", "b.rs"], "numFiles": 2});
        let r = grep("Found 2 files\na.rs\nb.rs", &detail);
        assert_eq!(r.files.len(), 2);
        assert_eq!(r.files[1], FileHits { path: "b.rs".into(), count: None });
        let r = grep("Found 2 files\na.rs\nb.rs", &Value::Null);
        assert_eq!(r.mode, "files_with_matches");
        assert_eq!(r.files.len(), 2);
    }

    #[test]
    fn grep_counts() {
        let detail = json!({"mode": "count", "numFiles": 2, "filenames": [], "content": "crates\\a.rs:63\nC:\\b.rs:2", "numMatches": 65});
        let r = grep("", &detail);
        assert_eq!(r.matches, Some(65));
        assert_eq!(r.files[0], FileHits { path: "crates\\a.rs".into(), count: Some(63) });
        assert_eq!(r.files[1], FileHits { path: "C:\\b.rs".into(), count: Some(2) });
    }

    #[test]
    fn grep_content() {
        let content = "crates\\a.rs:409:  \"x\" => 1,\ncrates\\a.rs-410-  ctx\n--\nC:\\horadric-core\\b.rs:7:fn b() { let t = 3:4; }\nc-d.rs:12:y";
        let detail = json!({"mode": "content", "numFiles": 0, "filenames": [], "content": content, "numLines": 5, "appliedLimit": 20});
        let r = grep("", &detail);
        assert!(r.limited);
        assert_eq!(r.matches, Some(3));
        assert_eq!(r.lines[0], GrepLine { file: "crates\\a.rs".into(), line: Some(409), text: "  \"x\" => 1,".into(), context: false, gap: false });
        assert!(r.lines[1].context && r.lines[1].line == Some(410) && r.lines[1].text == "  ctx");
        assert!(r.lines[2].gap);
        assert_eq!(r.lines[3].file, "C:\\horadric-core\\b.rs");
        assert_eq!(r.lines[3].text, "fn b() { let t = 3:4; }");
        assert_eq!(r.lines[4].file, "c-d.rs");
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.files[0].count, Some(1));
        let one = grep_line("12:only one file");
        assert_eq!((one.file.as_str(), one.line, one.text.as_str()), ("", Some(12), "only one file"));
        let bare = grep_line("no numbers here");
        assert_eq!((bare.line, bare.text.as_str()), (None, "no numbers here"));
    }

    #[test]
    fn glob_files() {
        let detail = json!({"filenames": ["a.md", "b.md"], "numFiles": 2, "truncated": false, "totalMatches": 2});
        assert_eq!(glob("", &detail), GlobResult { files: vec!["a.md".into(), "b.md".into()], total: 2, truncated: false });
        assert_eq!(glob("No files found", &Value::Null).files.len(), 0);
        assert_eq!(glob("x.rs\ny.rs", &Value::Null).total, 2);
    }

    #[test]
    fn shell_streams_and_codes() {
        let input = json!({"command": "git log", "description": "Show history"});
        let detail = json!({"stdout": "\u{1b}[33mabc\u{1b}[0m one", "stderr": "\nShell cwd was reset", "interrupted": false, "isImage": false});
        let r = read_result("Bash", &input, "abc one\nShell cwd was reset", false, &detail);
        let ResultKind::Shell(s) = r.kind else { panic!() };
        assert_eq!((s.command.as_str(), s.description.as_str()), ("git log", "Show history"));
        assert_eq!(&*s.stdout, "abc one");
        assert_eq!(&*s.stderr, "Shell cwd was reset");
        assert_eq!(s.exit_code, Some(0));

        let text = "Exit code 128\nfatal: pathspec 'x' did not match any files";
        let r = read_result("Bash", &input, text, true, &json!(format!("Error: {text}")));
        assert!(r.error);
        let ResultKind::Shell(s) = r.kind else { panic!() };
        assert_eq!(s.exit_code, Some(128));
        assert_eq!(&*s.stdout, "fatal: pathspec 'x' did not match any files");

        let detail = json!({"stdout": "", "stderr": "", "interrupted": false, "returnCodeInterpretation": "No matches found"});
        let ResultKind::Shell(s) = read_result("Bash", &input, "", false, &detail).kind else { panic!() };
        assert_eq!((s.exit_code, s.interpretation.as_str()), (None, "No matches found"));

        let detail = json!({"stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "b8b", "timedOutAfterMs": 600000});
        let ResultKind::Shell(s) = read_result("PowerShell", &input, "moved", false, &detail).kind else { panic!() };
        assert_eq!((s.exit_code, s.background.as_deref(), s.timed_out_ms), (None, Some("b8b"), Some(600000)));
    }

    #[test]
    fn web_search_links_and_words() {
        let detail = json!({"query": "aocr.org", "results": [
            {"tool_use_id": "s1", "content": [{"title": "AOCR", "url": "https://aocr.org"}]},
            "It is a college of radiology."
        ]});
        let r = web_search(&json!({}), "", &detail);
        assert_eq!(r.query, "aocr.org");
        assert_eq!(r.links, vec![Link { title: "AOCR".into(), url: "https://aocr.org".into() }]);
        assert_eq!(r.summary, "It is a college of radiology.");

        let text = "Web search results for query: \"q\"\n\nLinks: [{\"title\":\"T\",\"url\":\"https://t\"}]\n\nWords here.";
        let r = web_search(&json!({"query": "q"}), text, &Value::Null);
        assert_eq!(r.links.len(), 1);
        assert_eq!(r.summary, "Words here.");
    }

    #[test]
    fn mcp_names_and_json() {
        let r = mcp("mcp__claude_ai_Slack__slack_send_message", "{\"a\":1,\"b\":[2]}");
        assert_eq!((r.server.as_str(), r.tool.as_str()), ("claude_ai_Slack", "slack_send_message"));
        assert_eq!(r.json.as_deref(), Some("{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}"));
        assert_eq!(mcp("mcp__x__y", "plain words").json, None);
        assert_eq!(mcp("mcp__x__y", "{broken").json, None);
    }
}
