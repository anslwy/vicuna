// fx as a Work engine: discovery of the user's `fx` CLI, the saved model
// pick and the environment of the `fx acp` process. The engine itself is the
// shared ACP client in acp.rs (fx speaks standard ACP over stdio, like
// `opencode acp`), with the user's own fx login and ~/.fx/settings.json:
// nothing bundled, nothing in the user's fx config is rewritten.
//
// Model ids in Vicuna are `<fx provider>/<model>` (e.g. `codex/gpt-6-luna`,
// `ollama/deepseek-v4.1-flash:cloud`); a turn sets fx's `provider` then
// `model` session config options. Providers come from fx itself: its built-in
// ones (codex, gateway, grok) plus every provider configured under
// "providers" in ~/.fx/settings.json, e.g. a local Ollama:
//
//   "providers": { "ollama": { "base_url": "http://localhost:11434/v1",
//     "protocol": "openai-chat-completions",
//     "auth": { "type": "bearer", "env": "OLLAMA_API_KEY" } } }

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::process::Command;

use crate::opencode::{
    common_dirs, find_exe_in, home_dir, parse_version, read_model_from, write_model_to, WorkEngineInfo,
};

pub const CLI_INSTALL_URL: &str = "https://fx.sh/docs";
/// Exclusive override (absolute path), like VICUNA_OPENCODE_BIN.
pub const BIN_ENV: &str = "VICUNA_FX_BIN";
/// fx's own permission-mode override: per process, never written to the
/// user's settings (unlike the ACP `mode` config option, which fx persists).
const PERMISSION_ENV: &str = "FX_PERMISSION_MODE";

#[cfg(windows)]
const CLI_NAMES: &[&str] = &["fx.exe", "fx"];
#[cfg(not(windows))]
const CLI_NAMES: &[&str] = &["fx"];

// ---------- detection ----------

fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".fx").join("bin"));
    }
    dirs.extend(common_dirs(home));
    dirs
}

pub fn find_fx_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(CLI_NAMES, override_bin, path_var, &extra_dirs(home))
}

/// Like the user's terminal (login-shell PATH, process PATH, install
/// folders; newest `--version` when several exist), same as opencode.
pub fn resolve_fx() -> Option<crate::exe_resolve::Resolved> {
    crate::exe_resolve::resolve(CLI_NAMES, std::env::var(BIN_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
}

pub fn find_fx() -> Option<PathBuf> {
    resolve_fx().map(|r| r.path)
}

pub fn missing_message() -> String {
    format!("fx CLI not found, install it: {CLI_INSTALL_URL}")
}

fn describe(p: &Path) -> (Option<String>, String) {
    // `fx --version` prints "0.0.16"
    let v = crate::exe_resolve::version_of(p).and_then(|o| parse_version(&o));
    let label = match &v {
        Some(v) => format!("{} ({v})", p.display()),
        None => p.display().to_string(),
    };
    (v, label)
}

pub async fn engine_info() -> WorkEngineInfo {
    let (cli, version, other_labels) = tokio::task::spawn_blocking(|| {
        let found = resolve_fx();
        let version = found.as_ref().and_then(|f| describe(&f.path).0);
        let others: Vec<String> = found.as_ref().map(|f| f.others.iter().map(|o| describe(o).1).collect()).unwrap_or_default();
        (found.map(|f| f.path), version, others)
    })
    .await
    .unwrap_or((None, None, Vec::new()));
    let installed = cli.is_some();
    WorkEngineInfo {
        id: "fx".to_string(),
        label: "fx".to_string(),
        installed,
        missing: (!installed).then(missing_message),
        engines: Vec::new(),
        path: cli.map(|c| c.display().to_string()),
        version,
        install_url: CLI_INSTALL_URL.to_string(),
        model: saved_model(),
        config_path: prefs_path().map(|p| p.display().to_string()),
        others: other_labels,
        warning: None,
        ask_permission: false,
    }
}

// ---------- process ----------

/// `provider/model` -> (provider, model). Gateway models carry their own
/// slash (`gateway/openai/gpt-6-sol`), so only the first one splits.
pub fn split_model(id: &str) -> Option<(&str, &str)> {
    let (p, m) = id.trim().split_once('/')?;
    (!p.is_empty() && !m.is_empty()).then_some((p, m))
}

/// Configured providers whose auth env var is unset although they point at
/// a loopback server (a local Ollama needs no real key, but fx refuses to
/// start the provider without the variable): set a placeholder for those.
/// Remote providers keep failing with fx's own "check the auth environment
/// variable" error, which names the variable.
pub fn loopback_auth_env(settings: &Value, is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(providers) = settings.get("providers").and_then(Value::as_object) else {
        return out;
    };
    for p in providers.values() {
        let Some(env) = p.pointer("/auth/env").and_then(Value::as_str).filter(|e| !e.is_empty()) else { continue };
        let url = p.get("base_url").and_then(Value::as_str).unwrap_or("");
        let host = url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(url)
            .split(['/', ':'])
            .next()
            .unwrap_or("");
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "[") || host.starts_with("127.");
        if loopback && !is_set(env) && !out.iter().any(|(k, _)| k == env) {
            out.push((env.to_string(), "ollama".to_string()));
        }
    }
    out
}

fn fx_settings() -> Option<Value> {
    let p = home_dir()?.join(".fx").join("settings.json");
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Environment for every fx process Vicuna starts.
pub fn process_env(ask: bool) -> Vec<(String, String)> {
    let mut env = vec![("NO_COLOR".to_string(), "1".to_string()), ("FX_AUTO_UPGRADE".to_string(), "0".to_string())];
    // Vicuna's "ask" approval mode; "auto" keeps the user's own fx mode
    // (default: fx's automatic safety review) and Vicuna approves whatever
    // fx still asks about.
    if ask {
        env.push((PERMISSION_ENV.to_string(), "ask".to_string()));
    }
    if let Some(s) = fx_settings() {
        env.extend(loopback_auth_env(&s, |k| std::env::var_os(k).is_some_and(|v| !v.is_empty())));
    }
    env
}

pub fn command(bin: &Path, ask: bool) -> Command {
    let mut cmd = Command::new(bin);
    for (k, v) in process_env(ask) {
        cmd.env(k, v);
    }
    // the terminal's PATH, so stdio MCP servers fx spawns resolve like in a
    // shell (the same login-shell fix as opencode)
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    if let Some(h) = home_dir() {
        cmd.current_dir(h);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

// ---------- persisted model choice ----------

pub fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("fx-work.json"))
}

pub fn saved_model() -> Option<String> {
    read_model_from(&prefs_path()?)
}

pub fn save_model(model: Option<&str>) -> Result<(), String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_model_to(&path, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("vicuna-fx-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    #[test]
    fn finds_cli_on_path_and_in_local_bin() {
        use std::os::unix::fs::PermissionsExt as _;
        let fake = |dir: &Path| {
            let p = dir.join("fx");
            std::fs::write(&p, "#!/bin/sh\necho 0.0.16\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let path_dir = tmp("path");
        let home = tmp("home");
        let cli = fake(&path_dir);
        assert_eq!(find_fx_in(None, Some(path_dir.clone().into_os_string()), Some(&home)), Some(cli));
        // ~/.local/bin (fx's installer), which a GUI PATH misses
        let local = home.join(".local").join("bin");
        std::fs::create_dir_all(&local).unwrap();
        let native = fake(&local);
        assert_eq!(find_fx_in(None, Some(OsString::from("/nonexistent")), Some(&home)), Some(native));
        assert_eq!(find_fx_in(Some("/nonexistent/fx"), Some(path_dir.into_os_string()), Some(&home)), None);
        assert!(missing_message().contains(CLI_INSTALL_URL));
    }

    #[test]
    fn version_and_model_ids_parse() {
        assert_eq!(parse_version("0.0.16\n").as_deref(), Some("0.0.16"));
        assert_eq!(split_model("codex/gpt-6-luna"), Some(("codex", "gpt-6-luna")));
        assert_eq!(split_model("ollama/deepseek-v4.1-flash:cloud"), Some(("ollama", "deepseek-v4.1-flash:cloud")));
        assert_eq!(split_model("gateway/openai/gpt-6-sol"), Some(("gateway", "openai/gpt-6-sol")));
        assert_eq!(split_model("gpt-6-luna"), None);
        assert_eq!(split_model("/x"), None);
    }

    #[test]
    fn loopback_providers_get_a_placeholder_key() {
        let s = json!({ "providers": {
            "ollama": { "base_url": "http://localhost:11434/v1", "auth": { "type": "bearer", "env": "OLLAMA_API_KEY" } },
            "lan": { "base_url": "http://127.0.0.1:8080/v1", "auth": { "type": "bearer", "env": "LAN_KEY" } },
            "remote": { "base_url": "https://api.example.com/v1", "auth": { "type": "bearer", "env": "REMOTE_KEY" } }
        }});
        let mut env = loopback_auth_env(&s, |_| false);
        env.sort();
        assert_eq!(env, vec![("LAN_KEY".into(), "ollama".into()), ("OLLAMA_API_KEY".into(), "ollama".into())]);
        // a key the user exported wins
        assert!(loopback_auth_env(&s, |k| k == "OLLAMA_API_KEY").iter().all(|(k, _)| k != "OLLAMA_API_KEY"));
        assert!(loopback_auth_env(&json!({}), |_| false).is_empty());
    }

    #[test]
    fn ask_mode_sets_fx_permission_env_only_when_asked() {
        assert!(process_env(true).iter().any(|(k, v)| k == PERMISSION_ENV && v == "ask"));
        assert!(!process_env(false).iter().any(|(k, _)| k == PERMISSION_ENV));
    }
}
