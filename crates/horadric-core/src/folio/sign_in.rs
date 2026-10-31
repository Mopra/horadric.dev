//! Logging in to Claude from the Folio, without a terminal: the sign-in
//! card's state, and what `claude auth login`, `claude auth logout` and
//! `claude auth status --json` say, read from their output.
//!
//! What 2.1.314 does with its output on pipes (tried 2026-10-31 in an
//! empty `CLAUDE_CONFIG_DIR`): it listens on localhost and opens the
//! browser itself, then prints three things, the last with no line end:
//!
//! ```text
//! Opening browser to sign in…
//! If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&…
//! Paste code here if prompted >
//! ```
//!
//! The browser coming back to localhost finishes it by itself. The address
//! printed is the manual one: its page shows a code, `code#state`, which
//! goes in on stdin as a line. Then `Login successful.` and exit 0, or
//! `Login failed: …` on stderr and exit 1. A pasted line without its `#`
//! gets `Invalid code. …` on stderr and the login goes on waiting. Closing
//! stdin does not end it while stdin is not a terminal.

use serde_json::Value;

/// A turn failed for one of these: a new login can fix it.
pub fn is_auth_error(kind: &str) -> bool {
    matches!(kind, "authentication_failed" | "oauth_org_not_allowed")
}

/// Whether an `api_error`'s error, an API error the CLI serialised, is
/// the login being refused.
pub fn is_auth_api_error(error: &Value) -> bool {
    if error["status"].as_u64() == Some(401) {
        return true;
    }
    let text = error.to_string();
    text.contains("authentication_error") || text.contains("OAuth token")
}

/// Where a sign-in stands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SignInState {
    /// A turn failed for want of a login; no sign-in runs yet.
    Needed,
    /// `claude auth login` started and has not said where to go yet.
    Starting,
    /// The sign-in page waits in the browser.
    Waiting,
    /// A pasted code went to the CLI, which checks it.
    Checking,
    /// Logged in, as this email when the CLI said.
    Done(Option<String>),
    Failed(String),
    /// Closed by the human, or replaced by a sign-in started elsewhere.
    Cancelled,
}

/// The sign-in card, a block in the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignIn {
    /// The login run this card follows, 0 while none does.
    pub run: u64,
    pub state: SignInState,
    /// Why it is needed, in the API's words; empty when the human asked.
    pub why: String,
    /// The manual sign-in page, once the CLI printed it.
    pub url: Option<String>,
    /// The CLI said it takes a pasted code.
    pub asks_code: bool,
    /// The last code pasted was not whole.
    pub bad_code: bool,
    /// How many bad codes the CLI had said so far, so only a new one counts.
    bad_codes: usize,
}

impl SignIn {
    /// A card for a turn that failed for want of a login.
    pub fn needed(why: &str) -> SignIn {
        SignIn {
            run: 0,
            state: SignInState::Needed,
            why: why.trim().to_string(),
            url: None,
            asks_code: false,
            bad_code: false,
            bad_codes: 0,
        }
    }

    /// A card for a login run `run` that has just started.
    pub fn started(run: u64) -> SignIn {
        let mut s = SignIn::needed("");
        s.begin(run);
        s
    }

    /// Run `run` now serves this card, from its start.
    pub fn begin(&mut self, run: u64) {
        *self = SignIn {
            run,
            state: SignInState::Starting,
            why: std::mem::take(&mut self.why),
            ..SignIn::needed("")
        };
    }

    /// A login runs for it, or waits to.
    pub fn active(&self) -> bool {
        matches!(
            self.state,
            SignInState::Starting | SignInState::Waiting | SignInState::Checking
        )
    }

    /// A pasted code went to the CLI.
    pub fn code_sent(&mut self) {
        self.state = SignInState::Checking;
        self.bad_code = false;
    }

    /// Takes in what the run has said so far and, once it ended, its exit
    /// code. Returns whether the card changed.
    pub fn hear(&mut self, heard: &Heard, exit: Option<i32>) -> bool {
        if !self.active() {
            return false;
        }
        let before = self.clone();
        self.url = heard.url.clone().or(self.url.take());
        self.asks_code |= heard.asks_code;
        if heard.bad_codes > self.bad_codes {
            self.bad_codes = heard.bad_codes;
            self.bad_code = true;
            self.state = SignInState::Waiting;
        }
        if heard.succeeded {
            self.state = SignInState::Done(None);
        } else if let Some(why) = &heard.failure {
            self.state = SignInState::Failed(why.clone());
        } else if let Some(code) = exit {
            self.state = SignInState::Failed(match code {
                130 => "it ended before a code was pasted.".to_string(),
                _ => "it ended without logging in.".to_string(),
            });
        } else if self.state == SignInState::Starting && self.url.is_some() {
            self.state = SignInState::Waiting;
        }
        *self != before
    }
}

/// What a login run has said so far, read again from all of it each time,
/// so a line split across reads is never half heard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    pub url: Option<String>,
    pub asks_code: bool,
    pub succeeded: bool,
    /// What `Login failed:` said.
    pub failure: Option<String>,
    /// How many pasted codes it called invalid.
    pub bad_codes: usize,
}

impl Heard {
    pub fn read(out: &str, err: &str) -> Heard {
        let out = plain(out);
        let err = plain(err);
        let url = ["visit: ", "Sign in at: "]
            .iter()
            .find_map(|lead| address_after(&out, lead))
            .or_else(|| address_after(&out, ""));
        let failure = (err.lines())
            .find_map(|l| l.trim().strip_prefix("Login failed:"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Heard {
            url,
            asks_code: out.contains("Paste code here"),
            succeeded: out.contains("Login successful"),
            failure,
            bad_codes: err.matches("Invalid code").count(),
        }
    }
}

/// The first `https://` address after `lead`.
fn address_after(text: &str, lead: &str) -> Option<String> {
    let at = text.find(lead)? + lead.len();
    let rest = &text[at..];
    let start = rest.find("https://")?;
    let url: String = rest[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    (url.len() > "https://".len()).then_some(url)
}

/// `text` without terminal escapes: colours, and the hyperlinks the CLI
/// wraps an address in where it thinks the terminal shows them.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
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

/// What `claude auth status --json` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: bool,
    /// `claude.ai`, `api_key`, `none` and the like.
    pub method: String,
    pub email: Option<String>,
    /// `max`, `pro` and the like, for a subscription login.
    pub subscription: Option<String>,
}

impl AuthStatus {
    pub fn read(json: &str) -> Option<AuthStatus> {
        let v: Value = serde_json::from_str(json.trim()).ok()?;
        let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        Some(AuthStatus {
            logged_in: v["loggedIn"].as_bool()?,
            method: s("authMethod").unwrap_or_default(),
            email: s("email"),
            subscription: s("subscriptionType"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c&state=Im9f";

    fn printed() -> String {
        format!(
            "Opening browser to sign in\u{2026}\nIf the browser didn't open, visit: {URL}\nPaste code here if prompted > "
        )
    }

    #[test]
    fn the_address_and_the_prompt_are_heard() {
        let h = Heard::read(&printed(), "");
        assert_eq!(h.url.as_deref(), Some(URL));
        assert!(h.asks_code);
        assert!(!h.succeeded);
        let half = Heard::read("Opening browser to sign in\u{2026}\nIf the browser didn't open, visit: ", "");
        assert_eq!(half.url, None);
    }

    #[test]
    fn a_hyperlinked_address_reads_plain() {
        let out = format!("Sign in at: \x1b]8;;{URL}\x07{URL}\x1b]8;;\x07\n");
        assert_eq!(Heard::read(&out, "").url.as_deref(), Some(URL));
        assert_eq!(plain("\x1b[1mbold\x1b[0m"), "bold");
    }

    #[test]
    fn success_failure_and_bad_codes_are_heard() {
        let done = Heard::read(&format!("{}Login successful.\n", printed()), "");
        assert!(done.succeeded);
        let failed = Heard::read(&printed(), "Login failed: Request failed with status code 400\n");
        assert_eq!(
            failed.failure.as_deref(),
            Some("Request failed with status code 400")
        );
        let bad = "Invalid code. Please make sure the full code was copied.\n";
        assert_eq!(Heard::read("", &bad.repeat(2)).bad_codes, 2);
    }

    #[test]
    fn a_card_follows_its_run() {
        let mut card = SignIn::needed("OAuth token has expired \u{b7} Please run /login");
        assert!(!card.active());
        card.begin(3);
        assert_eq!((card.run, &card.state), (3, &SignInState::Starting));
        assert!(card.why.contains("expired"));
        assert!(card.hear(&Heard::read(&printed(), ""), None));
        assert_eq!(card.state, SignInState::Waiting);
        assert!(card.asks_code && card.url.is_some());
        card.code_sent();
        let bad = "Invalid code. Please make sure the full code was copied.\n";
        assert!(card.hear(&Heard::read(&printed(), bad), None));
        assert_eq!(card.state, SignInState::Waiting);
        assert!(card.bad_code);
        // The same bad code heard again is not a new one.
        card.code_sent();
        assert!(!card.hear(&Heard::read(&printed(), bad), None));
        assert_eq!(card.state, SignInState::Checking);
        let done = Heard::read(&format!("{}Login successful.\n", printed()), bad);
        assert!(card.hear(&done, Some(0)));
        assert_eq!(card.state, SignInState::Done(None));
        // Once over, nothing more moves it.
        assert!(!card.hear(&Heard::default(), Some(1)));
    }

    #[test]
    fn a_run_that_ends_without_a_word_failed() {
        let mut card = SignIn::started(1);
        assert!(card.hear(&Heard::default(), Some(130)));
        assert!(matches!(card.state, SignInState::Failed(ref s) if s.contains("before a code")));
    }

    #[test]
    fn auth_errors_are_told_apart() {
        assert!(is_auth_error("authentication_failed"));
        assert!(!is_auth_error("rate_limit"));
        let refused: Value = serde_json::json!({"status": 401, "error": {"type": "x"}});
        assert!(is_auth_api_error(&refused));
        let typed = serde_json::json!({"error": {"error": {"type": "authentication_error"}}});
        assert!(is_auth_api_error(&typed));
        assert!(!is_auth_api_error(&serde_json::json!({"status": 529})));
    }

    #[test]
    fn the_status_reads_logged_out_and_in() {
        let out = r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}"#;
        let s = AuthStatus::read(out).unwrap();
        assert!(!s.logged_in);
        assert_eq!((s.method.as_str(), s.email), ("none", None));
        let inn = r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "a@b.dk", "subscriptionType": "max"}"#;
        let s = AuthStatus::read(inn).unwrap();
        assert!(s.logged_in);
        assert_eq!(s.email.as_deref(), Some("a@b.dk"));
        assert_eq!(s.subscription.as_deref(), Some("max"));
        assert_eq!(AuthStatus::read("not json"), None);
    }
}
