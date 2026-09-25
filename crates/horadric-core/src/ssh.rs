//! A project's servers: the hosts in its `.horadric/config.json`, and the
//! rules for the terminal that reaches one.
//!
//! Horadric does not speak SSH. It runs the `ssh` that ships with Windows,
//! which reads `~/.ssh/config` for users, ports and keys, so a host here is
//! anything `ssh` takes as a destination: an alias, `deploy@203.0.113.7`.

use serde_json::Value;

/// The hosts in a `config.json`, in its order, each once. The file comes
/// with the repository, so a name `ssh` would read as an option, or split
/// into more than one argument, is left out rather than passed on.
pub fn hosts(config: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(config) else {
        return Vec::new();
    };
    let Some(list) = v.get("hosts").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for h in list.iter().filter_map(Value::as_str).map(str::trim) {
        let usable = !h.is_empty()
            && !h.starts_with('-')
            && !h.chars().any(|c| c.is_whitespace() || c.is_control());
        if usable && !out.iter().any(|o| o == h) {
            out.push(h.to_string());
        }
    }
    out
}

/// What `ssh` is started with to open a shell on `host`. The `--` ends the
/// options, so the host is only ever a destination.
pub fn args(host: &str) -> Vec<String> {
    vec!["--".into(), host.into()]
}

/// `ssh` exits 255 when it fails itself (refused, dropped, a key it could
/// not use) and with the remote shell's code otherwise. Only its own
/// failure keeps the pane, so the error can be read and a click can
/// reconnect. A remote `exit` closes it, as a local shell's does.
pub fn keeps(code: u32) -> bool {
    code == 255
}

/// A name for the `n`th SSH terminal of a project, counting from zero.
pub fn name(n: usize) -> String {
    match n {
        0 => "SSH".to_string(),
        n => format!("SSH {}", n + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts_in_order_each_once() {
        let config =
            r#"{"tasks":{"mode":"auto"},"hosts":["myvps"," deploy@203.0.113.7 ","myvps"]}"#;
        assert_eq!(hosts(config), vec!["myvps", "deploy@203.0.113.7"]);
    }

    #[test]
    fn no_hosts_when_the_file_says_none_or_is_unreadable() {
        assert!(hosts("").is_empty());
        assert!(hosts("not json").is_empty());
        assert!(hosts(r#"{"tasks":{}}"#).is_empty());
        assert!(hosts(r#"{"hosts":"myvps"}"#).is_empty());
        assert_eq!(hosts(r#"{"hosts":[1,null,"a"]}"#), vec!["a"]);
    }

    #[test]
    fn a_host_that_would_be_an_option_or_several_words_is_left_out() {
        let config = r#"{"hosts":["-oProxyCommand=calc","my vps","a\tb","","ok"]}"#;
        assert_eq!(hosts(config), vec!["ok"]);
    }

    #[test]
    fn the_host_goes_after_the_end_of_options() {
        assert_eq!(args("myvps"), vec!["--", "myvps"]);
    }

    #[test]
    fn only_ssh_failing_keeps_the_pane() {
        assert!(keeps(255));
        assert!(!keeps(0));
        assert!(!keeps(1));
        assert!(!keeps(130));
    }

    #[test]
    fn ssh_terminals_are_numbered_from_the_second() {
        assert_eq!(name(0), "SSH");
        assert_eq!(name(2), "SSH 3");
    }
}
