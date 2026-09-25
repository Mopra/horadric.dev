//! A project's servers: the hosts in its `.horadric/config.json`, and the
//! rules for the terminal that reaches one.
//!
//! Horadric does not speak SSH. It runs the `ssh` that ships with Windows,
//! which reads `~/.ssh/config` for users, ports and keys, so a host here is
//! anything `ssh` takes as a destination: an alias, `deploy@203.0.113.7`.

use serde_json::{Map, Value};

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
        if usable(h) && !out.iter().any(|o| o == h) {
            out.push(h.to_string());
        }
    }
    out
}

/// Whether `ssh` would take `host` as one destination and nothing else.
fn usable(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && !host.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `config` with `host` added to the end of its hosts, everything else
/// kept. None when `host` is not usable or is there already. A file that
/// is not a JSON object is started afresh rather than lost in part.
pub fn with_host(config: &str, host: &str) -> Option<String> {
    let host = host.trim();
    if !usable(host) || hosts(config).iter().any(|h| h == host) {
        return None;
    }
    let mut root = match serde_json::from_str::<Value>(config) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let list = root
        .entry("hosts")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !list.is_array() {
        *list = Value::Array(Vec::new());
    }
    if let Some(l) = list.as_array_mut() {
        l.push(Value::String(host.into()));
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).ok()?;
    out.push('\n');
    Some(out)
}

/// The hosts an `~/.ssh/config` names, in its order, each once: every
/// `Host` pattern that is a plain name. A wildcard or a negation matches
/// many hosts and names none, so it is left out. `Match` lines are
/// conditions, not names, and the `Host` lines after them are read as
/// usual since a `Host` line ends a `Match` block.
pub fn config_hosts(ssh_config: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in ssh_config.lines() {
        let line = line.trim();
        let Some((keyword, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        let rest = rest.split('#').next().unwrap_or_default();
        for name in rest.split_whitespace().map(|n| n.trim_matches('"')) {
            let plain = !name.contains(['*', '?', '!']);
            if plain && usable(name) && !out.iter().any(|o| o == name) {
                out.push(name.to_string());
            }
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

/// What every agent in a project with `hosts` is told about them, none
/// without any. `ssh` is how its shell runs the Windows `ssh`: Git Bash
/// brings its own, which reads the same `~/.ssh` but can not reach the
/// Windows `ssh-agent`, so a key held there works only through this one.
/// `BatchMode` makes a host that wants a password, a passphrase or a new
/// host key fail at once, since nobody can answer a prompt in the agent's
/// shell.
pub fn system_prompt(hosts: &[String], ssh: &str) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }
    let list = hosts
        .iter()
        .map(|h| format!("`{h}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "This project has servers you can reach over SSH: {list}. Run a command on \
         one with `{ssh} -o BatchMode=yes <host> '<command>'` from your shell. Use \
         that `ssh` rather than the one on your PATH: it is the Windows one, which \
         reaches the keys in the Windows ssh-agent. Your shell can not answer a \
         prompt, so with BatchMode a host that asks for a password or a passphrase, \
         or whose host key is not known yet, fails at once. If that happens, say so \
         and ask the human to connect once from the project's SSH terminal instead \
         of working around it."
    ))
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
    fn a_host_is_added_at_the_end_and_the_rest_kept() {
        let config = r#"{"tasks":{"mode":"auto"},"hosts":["a"]}"#;
        let out = with_host(config, " b ").unwrap();
        assert_eq!(hosts(&out), vec!["a", "b"]);
        assert!(out.contains("\"mode\": \"auto\""));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn a_host_starts_the_list_in_a_new_or_broken_config() {
        assert_eq!(hosts(&with_host("", "a").unwrap()), vec!["a"]);
        assert_eq!(hosts(&with_host("not json", "a").unwrap()), vec!["a"]);
        assert_eq!(
            hosts(&with_host(r#"{"hosts":"x"}"#, "a").unwrap()),
            vec!["a"]
        );
    }

    #[test]
    fn a_host_already_there_or_unusable_is_not_added() {
        let config = r#"{"hosts":["a"]}"#;
        assert_eq!(with_host(config, "a"), None);
        assert_eq!(with_host(config, ""), None);
        assert_eq!(with_host(config, "-oProxyCommand=calc"), None);
        assert_eq!(with_host(config, "my vps"), None);
    }

    #[test]
    fn the_ssh_config_offers_its_plain_host_names() {
        let config = "\
# my servers
Host myvps
    HostName 203.0.113.7
    User deploy
host  web1 web2 # both
Host=db
Host *
    ServerAliveInterval 30
Host *.internal !bastion prod-? \"quoted\"
Match host myvps exec \"true\"
    ForwardAgent yes
Host after-match
HostName not-a-host
Host myvps
";
        assert_eq!(
            config_hosts(config),
            vec!["myvps", "web1", "web2", "db", "quoted", "after-match"]
        );
    }

    #[test]
    fn an_empty_ssh_config_offers_nothing() {
        assert!(config_hosts("").is_empty());
        assert!(config_hosts("Host\nHost -x\n").is_empty());
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
    fn the_prompt_names_every_host_and_the_ssh_to_run() {
        let hosts = vec!["myvps".to_string(), "deploy@203.0.113.7".to_string()];
        let p = system_prompt(&hosts, "C:/Windows/System32/OpenSSH/ssh.exe").unwrap();
        assert!(p.contains("`myvps`, `deploy@203.0.113.7`."));
        assert!(
            p.contains("`C:/Windows/System32/OpenSSH/ssh.exe -o BatchMode=yes <host> '<command>'`")
        );
        assert!(!p.contains('\n'));
    }

    #[test]
    fn no_prompt_without_hosts() {
        assert_eq!(system_prompt(&[], "ssh"), None);
    }

    #[test]
    fn ssh_terminals_are_numbered_from_the_second() {
        assert_eq!(name(0), "SSH");
        assert_eq!(name(2), "SSH 3");
    }
}
