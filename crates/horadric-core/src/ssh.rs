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
