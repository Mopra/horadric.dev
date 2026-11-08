//! Writes Horadric's hook entries into Claude Code's user settings.
//!
//! Claude Code only reads hooks from settings files, so the entries have to
//! live in `~/.claude/settings.json`. They are inert for any `claude` not
//! started under Horadric: the session header comes out empty and the listener
//! drops the event. The installer only ever touches entries whose URL
//! contains `/horadric/`, so a user's own hooks are left exactly as they were.
//!
//! Horadric's Claude mod goes in beside them: a plugin whose TypeScript
//! runs inside the session and reports what no hook does, each subagent's
//! type and when it ends. It is written to `~/.claude/horadric-mod` and
//! named in the `CLAUDE_CODE_PLUGIN_DIRS` of the settings' `env`, the one
//! way to load a plugin from a folder without a marketplace. It does
//! nothing in a session Horadric did not start.
//!
//! An older Horadric also ran Grok Build, and gave it a hook file and an
//! MCP table in `~/.grok`. [`forget_grok`] takes them out again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{hook_url, HOOK_PATH, OWNER_ENV, SESSION_ENV};

/// Hook events Horadric needs. Everything that moves a session between
/// working, waiting and done.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
    // A hireling's start, and its end with its transcript and last words,
    // even where the mod is not loaded.
    "SubagentStart",
    "SubagentStop",
];

/// `~/.claude/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".claude").join("settings.json"))
}

/// The handler Claude Code runs. One shape for every event. It never
/// carries `onFailure`: set to `"block"`, a closed Horadric would stop
/// every prompt and tool call on the machine.
fn handler(port: u16) -> Value {
    json!({
        "type": "http",
        "url": hook_url(port),
        "headers": {
            "X-Horadric-Session": format!("${SESSION_ENV}"),
            "X-Horadric-Port": format!("${OWNER_ENV}")
        },
        "allowedEnvVars": [SESSION_ENV, OWNER_ENV],
        "timeout": 5
    })
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| u.contains(HOOK_PATH))
}

/// Adds or refreshes Horadric's hooks in a settings document. Ours are
/// taken out and written fresh, so an `onFailure` someone put on one is
/// dropped with it.
pub fn install_into(settings: &mut Value, port: u16) {
    remove_from(settings);
    let root = ensure_object(settings);
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = ensure_object(hooks);
    for event in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(groups) = groups {
            groups.push(json!({ "hooks": [handler(port)] }));
        }
    }
}

/// Removes every Horadric hook from a settings document, leaving the rest.
pub fn remove_from(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|h| !is_ours(h));
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
}

/// True when the document has a Horadric hook on every event we need,
/// none of them with an `onFailure` that `install_into` would strip.
pub fn is_installed(settings: &Value, port: u16) -> bool {
    let url = hook_url(port);
    EVENTS.iter().all(|event| {
        settings
            .pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    g.get("hooks").and_then(Value::as_array).is_some_and(|l| {
                        l.iter().any(|h| {
                            h.get("url").and_then(Value::as_str) == Some(&url)
                                && h.get("onFailure").is_none()
                        })
                    })
                })
            })
    })
}

fn ensure_object(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("just made it an object")
}

fn read(path: &Path) -> io::Result<Value> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(json!({})),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Write beside and rename, so a crash mid write cannot leave Claude Code
    // with half a settings file.
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Installs the hooks and the mod beside the user's settings file.
/// Idempotent.
pub fn install(path: &Path, port: u16) -> io::Result<()> {
    let mut settings = read(path)?;
    install_into(&mut settings, port);
    let dir = mod_dir(path);
    write_mod(&dir)?;
    add_mod(&mut settings, &dir);
    write(path, &settings)
}

/// Removes the hooks and the mod from beside the user's settings file.
/// Idempotent.
pub fn uninstall(path: &Path) -> io::Result<()> {
    let mut settings = read(path)?;
    remove_from(&mut settings);
    remove_mod(&mut settings);
    write(path, &settings)?;
    match fs::remove_dir_all(mod_dir(path)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The settings file of each town's own Claude home: a session there reads
/// its user settings from that home and never from `~/.claude`, so the
/// hooks and the mod are written into each as well.
pub fn town_settings() -> Vec<PathBuf> {
    crate::towns::homes()
        .into_iter()
        .map(|h| h.join("settings.json"))
        .collect()
}

/// [`install`] into every town home. Each failure is said, none stops the
/// rest: a town without hooks only has tiles that do not move.
pub fn install_towns(port: u16) -> Vec<String> {
    town_settings()
        .into_iter()
        .filter_map(|p| install(&p, port).err().map(|e| format!("{}: {e}", p.display())))
        .collect()
}

/// [`uninstall`] from every town home.
pub fn uninstall_towns() -> Vec<String> {
    town_settings()
        .into_iter()
        .filter_map(|p| uninstall(&p).err().map(|e| format!("{}: {e}", p.display())))
        .collect()
}

/// The folder the mod is written to, beside the settings file.
pub const MOD_DIR: &str = "horadric-mod";

/// The variable Claude Code loads plugin folders from, as `--plugin-dir`
/// does.
pub const PLUGIN_DIRS: &str = "CLAUDE_CODE_PLUGIN_DIRS";

/// The mod's files, by their path inside its folder.
pub const MOD_FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("../mod/.claude-plugin/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("../mod/hooks/hooks.json")),
    (
        "hooks/register.tsx",
        include_str!("../mod/hooks/register.tsx"),
    ),
    ("types/index.d.ts", include_str!("../mod/types/index.d.ts")),
];

const LIST_SEP: char = if cfg!(windows) { ';' } else { ':' };

pub fn mod_dir(settings: &Path) -> PathBuf {
    settings.parent().unwrap_or(Path::new(".")).join(MOD_DIR)
}

/// Writes the mod afresh, so a file an older build had and this one dropped
/// does not stay behind. Left alone when it has not changed: every running
/// session watches the folder and reloads the mod on a write.
fn write_mod(dir: &Path) -> io::Result<()> {
    let same = MOD_FILES
        .iter()
        .all(|(name, text)| fs::read_to_string(dir.join(name)).is_ok_and(|t| t == *text));
    if same {
        return Ok(());
    }
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    for (name, text) in MOD_FILES {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    Ok(())
}

fn is_mod_entry(entry: &str) -> bool {
    Path::new(entry.trim()).file_name() == Some(MOD_DIR.as_ref())
}

/// Names `dir` in the settings' plugin folders, after any of the user's own.
pub fn add_mod(settings: &mut Value, dir: &Path) {
    remove_mod(settings);
    let env = ensure_object(settings)
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    let env = ensure_object(env);
    let dir = dir.to_string_lossy();
    let list = match env.get(PLUGIN_DIRS).and_then(Value::as_str) {
        Some(theirs) if !theirs.trim().is_empty() => format!("{theirs}{LIST_SEP}{dir}"),
        _ => dir.into_owned(),
    };
    env.insert(PLUGIN_DIRS.into(), Value::String(list));
}

/// Takes the mod out of the settings' plugin folders, leaving the user's.
pub fn remove_mod(settings: &mut Value) {
    let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(list) = env.get(PLUGIN_DIRS).and_then(Value::as_str) else {
        return;
    };
    let rest: Vec<&str> = list
        .split(LIST_SEP)
        .filter(|e| !e.trim().is_empty() && !is_mod_entry(e))
        .collect();
    if rest.is_empty() {
        env.remove(PLUGIN_DIRS);
    } else {
        env.insert(
            PLUGIN_DIRS.into(),
            Value::String(rest.join(&LIST_SEP.to_string())),
        );
    }
    if env.is_empty() {
        settings.as_object_mut().map(|o| o.remove("env"));
    }
}

/// Reports whether the user's settings file has the hooks.
pub fn status(path: &Path, port: u16) -> io::Result<bool> {
    Ok(is_installed(&read(path)?, port))
}

/// Grok's home: `$GROK_HOME`, or `~/.grok`.
fn grok_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".grok"))
}

/// The header of the table an older Horadric gave Grok `horadric mcp` in.
const GROK_MCP_TABLE: &str = "[mcp_servers.horadric]";

/// Grok's `config` with Horadric's table taken out. Only that table goes:
/// it runs from its header to the next one, and a table under it, like
/// `[mcp_servers.horadric.env]`, goes with it.
fn without_grok_mcp(config: &str) -> String {
    let ours = |line: &str| {
        let line = line.trim();
        line == GROK_MCP_TABLE || line.starts_with("[mcp_servers.horadric.")
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in config.lines() {
        if line.trim_start().starts_with('[') {
            inside = ours(line);
        }
        if !inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Removes the hook file and MCP table an older Horadric put into Grok's
/// home, which would otherwise run `horadric hook grok`, a command that is
/// gone, at every Grok event. Idempotent, and nothing without a home.
pub fn forget_grok() -> Result<(), String> {
    let Some(home) = grok_home() else {
        return Ok(());
    };
    forget_grok_in(&home).map_err(|e| format!("{}: {e}", home.display()))
}

fn forget_grok_in(home: &Path) -> io::Result<()> {
    if let Err(e) = fs::remove_file(home.join("hooks").join("horadric.json")) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e);
        }
    }
    let path = home.join("config.toml");
    let config = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let text = without_grok_mcp(&config);
    if text == config {
        return Ok(());
    }
    // Beside and rename, as for Claude Code's settings.
    let tmp = path.with_extension("toml.horadric-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// What Claude Code's `/model` and `/effort` save into the user's settings
/// as they switch a session: typed in, as Horadric does, they switch this
/// session and also make the pick the default for every new one. Effort is
/// saved for the model in use, under `modelSettings`.
pub const SWITCHED: [&str; 3] = ["model", "effortLevel", "modelSettings"];

/// What `keys` hold in `settings`, None where one is missing.
pub fn pick(settings: &Value, keys: &[&str]) -> Vec<Option<Value>> {
    keys.iter().map(|k| settings.get(*k).cloned()).collect()
}

/// Puts `keys` back as [`pick`] found them. True when anything changed.
pub fn put_back(settings: &mut Value, keys: &[&str], was: &[Option<Value>]) -> bool {
    if pick(settings, keys) == was {
        return false;
    }
    let map = ensure_object(settings);
    for (k, v) in keys.iter().zip(was) {
        match v {
            Some(v) => map.insert(k.to_string(), v.clone()),
            None => map.remove(*k),
        };
    }
    true
}

/// What `keys` hold in the user's settings file now.
pub fn snapshot(path: &Path, keys: &[&str]) -> io::Result<Vec<Option<Value>>> {
    Ok(pick(&read(path)?, keys))
}

/// Puts `keys` in the user's settings file back as a [`snapshot`] had
/// them, writing only when something changed.
pub fn restore(path: &Path, keys: &[&str], was: &[Option<Value>]) -> io::Result<()> {
    let mut settings = read(path)?;
    if put_back(&mut settings, keys, was) {
        write(path, &settings)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_saved_as_the_default_is_put_back() {
        let mut s = json!({"model": "opus", "hooks": {}});
        let was = pick(&s, &SWITCHED);
        assert_eq!(was, [Some(json!("opus")), None, None]);
        assert!(!put_back(&mut s, &SWITCHED, &was));
        s["model"] = json!("claude-haiku-4-5-20251001");
        s["modelSettings"] = json!({"claude-haiku-4-5": {"effortLevel": "low"}});
        assert!(put_back(&mut s, &SWITCHED, &was));
        assert_eq!(s, json!({"model": "opus", "hooks": {}}));
    }

    #[test]
    fn the_mod_joins_the_users_plugin_folders_and_leaves_them_as_it_found_them() {
        let sep = LIST_SEP;
        let mine = "/plugins/mine";
        let dir = Path::new("home").join(".claude").join(MOD_DIR);
        let mut s = json!({ "env": { "FOO": "1", PLUGIN_DIRS: mine } });
        add_mod(&mut s, &dir);
        add_mod(&mut s, &dir);
        assert_eq!(
            s["env"][PLUGIN_DIRS],
            format!("{mine}{sep}{}", dir.to_string_lossy())
        );
        remove_mod(&mut s);
        assert_eq!(s, json!({ "env": { "FOO": "1", PLUGIN_DIRS: mine } }));

        let mut s = json!({ "model": "opus" });
        add_mod(&mut s, &dir);
        assert_eq!(s["env"][PLUGIN_DIRS], dir.to_string_lossy().as_ref());
        remove_mod(&mut s);
        assert_eq!(s, json!({ "model": "opus" }));
    }

    #[test]
    fn a_mod_folder_from_another_home_is_still_ours_to_remove() {
        let old = Path::new("old").join(MOD_DIR);
        let mut s = json!({ "env": { PLUGIN_DIRS: old.to_string_lossy() } });
        remove_mod(&mut s);
        assert_eq!(s, json!({}));
    }

    #[test]
    fn install_writes_the_mod_and_uninstall_takes_it_away() {
        let home = std::env::temp_dir().join(format!("horadric-mod-{}", std::process::id()));
        let settings = home.join(".claude").join("settings.json");
        let dir = mod_dir(&settings);
        fs::create_dir_all(dir.join("stale")).unwrap();
        install(&settings, 43117).unwrap();
        assert!(!dir.join("stale").exists());
        // Unchanged, it is not written again.
        fs::create_dir_all(dir.join("kept")).unwrap();
        install(&settings, 43117).unwrap();
        assert!(dir.join("kept").exists());
        for (name, text) in MOD_FILES {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), *text);
        }
        let s = read(&settings).unwrap();
        assert!(is_installed(&s, 43117));
        assert_eq!(s["env"][PLUGIN_DIRS], dir.to_string_lossy().as_ref());

        uninstall(&settings).unwrap();
        assert!(!dir.exists());
        assert_eq!(read(&settings).unwrap(), json!({}));
        uninstall(&settings).unwrap();
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn the_mod_names_the_module_it_ships() {
        let hooks = MOD_FILES
            .iter()
            .find(|(n, _)| *n == "hooks/hooks.json")
            .unwrap();
        let modules: Value = serde_json::from_str(hooks.1).unwrap();
        let module = modules["modules"][0].as_str().unwrap();
        let shipped = format!("hooks/{}", module.trim_start_matches("./"));
        assert!(MOD_FILES.iter().any(|(n, _)| *n == shipped));
    }

    #[test]
    fn install_is_idempotent_and_removable() {
        let mut s = json!({
            "model": "opus",
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine" } ] } ]
            }
        });
        install_into(&mut s, 43117);
        install_into(&mut s, 43117);
        assert!(is_installed(&s, 43117));
        // The user's own Stop hook is untouched and ours sits beside it.
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");

        remove_from(&mut s);
        assert!(!is_installed(&s, 43117));
        assert_eq!(
            s.pointer("/hooks/Stop").unwrap().as_array().unwrap().len(),
            1
        );
        assert!(s.pointer("/hooks/Notification").is_none());
        assert_eq!(s["model"], "opus");
    }

    #[test]
    fn a_prompt_waits_on_the_memory_inside_its_hooks_time() {
        let timeout = handler(43117)["timeout"].as_u64().unwrap();
        assert!(crate::listener::PROMPT_WAIT.as_secs() + 1 < timeout);
        assert!(EVENTS.contains(&"UserPromptSubmit"));
    }

    #[test]
    fn our_hooks_never_carry_on_failure() {
        let mut s = json!({
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine", "onFailure": "block" } ] } ]
            }
        });
        install_into(&mut s, 43117);
        let mut ours = 0;
        for groups in s["hooks"].as_object().unwrap().values() {
            for g in groups.as_array().unwrap() {
                for h in g["hooks"].as_array().unwrap().iter().filter(|h| is_ours(h)) {
                    assert!(h.get("onFailure").is_none());
                    ours += 1;
                }
            }
        }
        assert_eq!(ours, EVENTS.len());
        // A user's own hook may block if they chose that.
        assert_eq!(s["hooks"]["Stop"][0]["hooks"][0]["onFailure"], "block");

        s["hooks"]["Stop"][1]["hooks"][0]["onFailure"] = json!("block");
        assert!(!is_installed(&s, 43117));
        install_into(&mut s, 43117);
        assert!(is_installed(&s, 43117));
        assert!(s["hooks"]["Stop"][1]["hooks"][0].get("onFailure").is_none());
    }

    #[test]
    fn port_change_replaces_old_entries() {
        let mut s = json!({});
        install_into(&mut s, 1000);
        install_into(&mut s, 2000);
        assert!(!is_installed(&s, 1000));
        assert!(is_installed(&s, 2000));
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 1);
    }

    #[test]
    fn remove_on_empty_is_fine() {
        let mut s = json!({ "model": "x" });
        remove_from(&mut s);
        assert_eq!(s, json!({ "model": "x" }));
    }

    #[test]
    fn an_old_grok_hook_and_mcp_table_come_out() {
        let dir = std::env::temp_dir().join(format!("horadric-grok-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        forget_grok_in(&dir).unwrap();
        fs::create_dir_all(dir.join("hooks")).unwrap();
        fs::write(dir.join("hooks").join("horadric.json"), "{}").unwrap();
        let config = "[cli]\ninstaller = \"internal\"\n\n\
                      [mcp_servers.horadric]\ncommand = \"old\"\n\n\
                      [mcp_servers.horadric.env]\nA = \"1\"\n\n\
                      [mcp_servers.github]\ncommand = \"gh\"\n";
        fs::write(dir.join("config.toml"), config).unwrap();
        forget_grok_in(&dir).unwrap();
        forget_grok_in(&dir).unwrap();
        assert!(!dir.join("hooks").join("horadric.json").exists());
        assert_eq!(
            fs::read_to_string(dir.join("config.toml")).unwrap(),
            "[cli]\ninstaller = \"internal\"\n\n[mcp_servers.github]\ncommand = \"gh\"\n"
        );
        // A table whose name only starts the same is someone else's.
        let theirs = "[mcp_servers.horadric2]\ncommand = \"z\"\n";
        assert_eq!(without_grok_mcp(theirs), theirs);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn handler_carries_session_header() {
        let h = handler(43117);
        assert_eq!(h["type"], "http");
        assert_eq!(h["headers"]["X-Horadric-Session"], "$HORADRIC_SESSION");
        assert_eq!(h["allowedEnvVars"][0], "HORADRIC_SESSION");
    }

    #[test]
    fn handler_carries_owner_port_header() {
        let h = handler(43117);
        assert_eq!(h["headers"]["X-Horadric-Port"], "$HORADRIC_OWNER_PORT");
        assert_eq!(h["allowedEnvVars"][1], "HORADRIC_OWNER_PORT");
    }
}
