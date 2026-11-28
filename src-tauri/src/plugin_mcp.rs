//! Plugin MCP servers: a plugin can declare MCP servers in its manifest
//! (`mcpServers` in `vicuna-plugin.json`) that join the MCP dialog beside the
//! user's own `vicuna.json` servers, the same way the built-in `browser` row
//! does.
//!
//! ```json
//! "mcpServers": [
//!   {"id": "search", "label": "Search", "type": "http", "url": "https://mcp.example.com/mcp"},
//!   {"id": "files", "type": "stdio", "command": "files-mcp", "args": ["--stdio"], "env": {"FILES_MODE": "ro"}}
//! ]
//! ```
//!
//! Safety mirrors `plugin_harness.rs`:
//! - `http` servers need an `https://` URL (or plain `http://` to loopback);
//!   they use the normal MCP OAuth flow in `mcp.rs` (tokens keyed by the
//!   server id, never stored in the manifest). Plugins can't set headers.
//! - `stdio` servers name a bare program found on PATH (no paths, no shell)
//!   with arguments free of shell metacharacters; optional `env` may not set
//!   loader/path variables (`PATH`, `LD_*`, `DYLD_*`, `NODE_OPTIONS`, …).
//! - Nothing runs or connects until the user consents to the exact
//!   transport + command + args + env + url (`mcp-consent.json` next to
//!   `install.json`, carried over by an upgrade; a changed launch no longer
//!   matches and needs fresh consent). Consented servers still start
//!   disabled (`pluginMcpEnabled` in vicuna.json), so installing a plugin
//!   never spends harness tool budget on its own.
//! - The server id is `<plugin-id>-<server-id>`: lowercase letters, digits
//!   and dashes only, valid as a Claude/Codex/ACP MCP server name.

use crate::mcp::McpServerConfig;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

const CONSENT: &str = "mcp-consent.json";
const MAX_SERVERS: usize = 8;
const MAX_ARGS: usize = 32;
const MAX_ENV: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginMcpServer {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// `http` | `stdio`.
    #[serde(rename = "type")]
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
}

/// Exactly what the user approves (and what is compared on every load).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl PluginMcpServer {
    pub fn launch(&self) -> Launch {
        Launch {
            transport: self.transport.clone(),
            url: self.url.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
        }
    }
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && !s.starts_with('-') && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The MCP server id a plugin server gets everywhere (dialog, sessions,
/// auth.json, harness configs).
pub fn server_id(plugin_id: &str, server: &str) -> String {
    format!("{plugin_id}-{server}")
}

/// Variables that change how programs load code or find binaries. A plugin
/// may not set them (the command must run as the user's own install would).
fn forbidden_env(k: &str) -> bool {
    const DENY: [&str; 16] = [
        "PATH", "HOME", "SHELL", "IFS", "ENV", "BASH_ENV", "NODE_OPTIONS", "NODE_PATH", "PYTHONPATH",
        "PYTHONSTARTUP", "PYTHONHOME", "PERL5LIB", "PERL5OPT", "RUBYOPT", "RUBYLIB", "LUA_PATH",
    ];
    DENY.contains(&k)
        || ["LD_", "DYLD_", "GIT_", "NPM_CONFIG_", "JAVA_TOOL", "_JAVA_"].iter().any(|p| k.starts_with(p))
        || k.ends_with("_OPTIONS") && k.starts_with("JDK_")
}

fn valid_env_name(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64
        && k.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Secrets never belong in a public manifest; values that look like one are
/// refused so a plugin can't ship (or a user can't approve) a baked-in token.
fn looks_secret(k: &str) -> bool {
    ["TOKEN", "SECRET", "PASSWORD", "API_KEY", "APIKEY", "CREDENTIAL", "AUTH"].iter().any(|w| k.contains(w))
}

fn valid_url(u: &str) -> Result<(), String> {
    if u.len() > 500 || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("url must be a single line of at most 500 characters".into());
    }
    let parsed = reqwest::Url::parse(u).map_err(|_| format!("url {u:?} is not a valid URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("url may not carry credentials".into());
    }
    let host = parsed.host_str().unwrap_or_default();
    match parsed.scheme() {
        "https" if !host.is_empty() => Ok(()),
        "http" if matches!(host, "127.0.0.1" | "localhost" | "[::1]") => Ok(()),
        _ => Err(format!("url {u:?} must be https:// (plain http:// only for 127.0.0.1/localhost)")),
    }
}

pub fn validate(plugin_id: &str, list: &[PluginMcpServer]) -> Result<(), String> {
    if list.len() > MAX_SERVERS {
        return Err(format!("at most {MAX_SERVERS} MCP servers per plugin"));
    }
    let mut seen = std::collections::HashSet::new();
    for s in list {
        if !valid_id(&s.id) || !seen.insert(s.id.as_str()) {
            return Err(format!("mcpServers {:?}: invalid or duplicate id (lowercase letters, digits, dashes)", s.id));
        }
        let sid = server_id(plugin_id, &s.id);
        if sid.len() > 64 || crate::mcp::is_fixed(&sid) || sid.eq_ignore_ascii_case(crate::mcp::BROWSER_ID) {
            return Err(format!("mcpServers {}: server id {sid:?} is reserved or too long", s.id));
        }
        let e = |m: String| format!("mcpServers {}: {m}", s.id);
        if s.label.chars().count() > 40 || s.label.chars().any(char::is_control) {
            return Err(e("label must be at most 40 characters".into()));
        }
        if s.description.chars().count() > 300 || s.description.chars().any(|c| c.is_control() && c != '\n') {
            return Err(e("description must be at most 300 characters".into()));
        }
        if s.install_hint.as_deref().is_some_and(|h| h.chars().count() > 300 || h.chars().any(|c| c.is_control() && c != '\n')) {
            return Err(e("installHint must be at most 300 characters".into()));
        }
        match s.transport.as_str() {
            "http" => {
                if s.command.is_some() || !s.args.is_empty() || !s.env.is_empty() {
                    return Err(e("http servers take only url (no command, args or env)".into()));
                }
                valid_url(s.url.as_deref().ok_or_else(|| e("http servers need a url".into()))?).map_err(e)?;
            }
            "stdio" => {
                if s.url.is_some() {
                    return Err(e("stdio servers take a command, not a url".into()));
                }
                let cmd = s.command.as_deref().ok_or_else(|| e("stdio servers need a command".into()))?;
                crate::plugin_harness::valid_command(cmd).map_err(e)?;
                if s.args.len() > MAX_ARGS {
                    return Err(e(format!("at most {MAX_ARGS} arguments")));
                }
                for a in &s.args {
                    crate::plugin_harness::valid_arg(a).map_err(e)?;
                }
                if s.env.len() > MAX_ENV {
                    return Err(e(format!("at most {MAX_ENV} env variables")));
                }
                for (k, v) in &s.env {
                    if !valid_env_name(k) || forbidden_env(k) {
                        return Err(e(format!("env {k:?} is not allowed (UPPER_CASE names; no PATH/HOME/loader variables)")));
                    }
                    if looks_secret(k) {
                        return Err(e(format!("env {k:?} looks like a secret; plugins can't ship credentials")));
                    }
                    crate::plugin_harness::valid_arg(v).map_err(|m| e(format!("env {k}: {m}")))?;
                }
            }
            other => return Err(e(format!("type {other:?} must be \"http\" or \"stdio\""))),
        }
    }
    Ok(())
}

// ---------- consent ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    #[serde(flatten)]
    pub launch: Launch,
    pub granted_at: i64,
}

pub type Consent = BTreeMap<String, Grant>;

pub fn read_consent(plugin_dir: &Path) -> Consent {
    std::fs::read(plugin_dir.join(CONSENT)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_consent(plugin_dir: &Path, c: &Consent) -> Result<(), String> {
    std::fs::write(plugin_dir.join(CONSENT), serde_json::to_vec_pretty(c).unwrap()).map_err(|e| e.to_string())
}

pub const CONSENT_FILE: &str = CONSENT;

pub fn consented(consent: &Consent, s: &PluginMcpServer) -> bool {
    consent.get(&s.id).is_some_and(|g| g.launch == s.launch())
}

pub fn pending<'a>(consent: &Consent, list: &'a [PluginMcpServer]) -> Vec<&'a PluginMcpServer> {
    list.iter().filter(|s| !consented(consent, s)).collect()
}

/// Grant consent. The window echoes the exact launch it showed; anything
/// that differs from the installed manifest is refused.
pub fn grant(plugin_dir: &Path, list: &[PluginMcpServer], approved: &[Value], now: i64) -> Result<Consent, String> {
    let mut consent = read_consent(plugin_dir);
    for a in approved {
        let id = a.get("id").and_then(Value::as_str).ok_or("each approved MCP server needs an id")?;
        let s = list.iter().find(|s| s.id == id).ok_or_else(|| format!("this plugin has no MCP server {id:?}"))?;
        let shown = Launch {
            transport: a.get("type").and_then(Value::as_str).unwrap_or_default().to_string(),
            url: a.get("url").and_then(Value::as_str).map(str::to_string),
            command: a.get("command").and_then(Value::as_str).map(str::to_string),
            args: a.get("args").and_then(Value::as_array)
                .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            env: a.get("env").and_then(Value::as_object)
                .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
                .unwrap_or_default(),
        };
        if shown != s.launch() {
            return Err(format!("MCP server {id}: its launch changed since it was shown; review it again"));
        }
        consent.insert(id.to_string(), Grant { launch: shown, granted_at: now });
    }
    write_consent(plugin_dir, &consent)?;
    Ok(consent)
}

pub fn revoke(plugin_dir: &Path, id: Option<&str>) -> Result<Consent, String> {
    let mut consent = read_consent(plugin_dir);
    match id {
        Some(id) => {
            consent.remove(id);
        }
        None => consent.clear(),
    }
    write_consent(plugin_dir, &consent)?;
    Ok(consent)
}

/// One server as the Plugins panel / consent dialog renders it.
pub fn describe(s: &PluginMcpServer, consent: &Consent, plugin_id: &str) -> Value {
    json!({
        "id": s.id, "serverId": server_id(plugin_id, &s.id),
        "label": if s.label.trim().is_empty() { s.id.clone() } else { s.label.clone() },
        "description": s.description, "type": s.transport, "url": s.url,
        "command": s.command, "args": s.args, "env": s.env, "installHint": s.install_hint,
        "enabled": consented(consent, s),
        "installed": s.command.as_deref().is_none_or(|c| crate::plugin_harness::find_command(c).is_some()),
    })
}

// ---------- the MCP dialog's view ----------

fn to_config(plugin_id: &str, plugin_name: &str, s: &PluginMcpServer) -> McpServerConfig {
    let id = server_id(plugin_id, &s.id);
    let command = s.command.as_deref().map(|c| {
        // engines like fx need an absolute command; resolve like a terminal
        let bin = crate::plugin_harness::find_command(c).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| c.to_string());
        std::iter::once(bin).chain(s.args.iter().cloned()).collect::<Vec<_>>()
    });
    McpServerConfig {
        id: id.clone(),
        name: id,
        transport: s.transport.clone(),
        command,
        env: (!s.env.is_empty()).then(|| s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()),
        url: s.url.clone(),
        headers: None,
        oauth: None,
        enabled: false,
        plugin_id: Some(plugin_id.to_string()),
        plugin_name: Some(plugin_name.to_string()),
    }
}

#[cfg(test)]
thread_local! {
    /// Tests inject the installed plugin servers instead of reading
    /// `~/vicuna/plugins` (`(config, consented)`).
    pub static TEST_SERVERS: std::cell::RefCell<Option<Vec<(McpServerConfig, bool)>>> = const { std::cell::RefCell::new(None) };
}

/// Every installed plugin MCP server with whether the user consented.
pub fn all_servers() -> Vec<(McpServerConfig, bool)> {
    #[cfg(test)]
    if let Some(v) = TEST_SERVERS.with(|t| t.borrow().clone()) {
        return v;
    }
    let mut out = Vec::new();
    for (m, dir) in crate::plugins::installed_manifests() {
        if m.mcp_servers.is_empty() {
            continue;
        }
        let consent = read_consent(&dir);
        for s in &m.mcp_servers {
            out.push((to_config(&m.id, &m.name, s), consented(&consent, s)));
        }
    }
    out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    out
}

/// Consented plugin servers (enabled flag still false; `mcp.rs` applies
/// `pluginMcpEnabled`).
pub fn consented_servers() -> Vec<McpServerConfig> {
    all_servers().into_iter().filter(|(_, ok)| *ok).map(|(c, _)| c).collect()
}

/// The plugin owning a server id (consented or not), by display name.
pub fn owner(id: &str) -> Option<String> {
    all_servers().into_iter().find(|(c, _)| c.id.eq_ignore_ascii_case(id.trim()))
        .map(|(c, _)| c.plugin_name.unwrap_or_default())
}

/// Server ids one plugin contributes (for disconnect on uninstall/revoke).
pub fn ids_for(m: &crate::plugins::Manifest) -> Vec<String> {
    m.mcp_servers.iter().map(|s| server_id(&m.id, &s.id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(v: Value) -> PluginMcpServer {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn manifest_validation() {
        let ok = [
            json!({"id": "search", "label": "Search", "type": "http", "url": "https://mcp.example.com/mcp"}),
            json!({"id": "local", "type": "http", "url": "http://127.0.0.1:8123/mcp"}),
            json!({"id": "files", "type": "stdio", "command": "files-mcp", "args": ["--stdio", "--root=."], "env": {"FILES_MODE": "ro"}}),
        ];
        validate("demo", &ok.iter().cloned().map(srv).collect::<Vec<_>>()).unwrap();
        let bad = [
            json!({"id": "a", "type": "http", "url": "http://example.com/mcp"}),
            json!({"id": "a", "type": "http", "url": "ftp://example.com"}),
            json!({"id": "a", "type": "http", "url": "https://user:pw@example.com"}),
            json!({"id": "a", "type": "http"}),
            json!({"id": "a", "type": "http", "url": "https://x.dev", "command": "x"}),
            json!({"id": "a", "type": "stdio", "command": "/usr/bin/x"}),
            json!({"id": "a", "type": "stdio", "command": "../x"}),
            json!({"id": "a", "type": "stdio", "command": "x", "args": ["a; rm -rf ~"]}),
            json!({"id": "a", "type": "stdio", "command": "x", "args": ["$(id)"]}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"LD_PRELOAD": "/x.so"}}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"PATH": "/evil"}}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"NODE_OPTIONS": "--require=x"}}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"DYLD_INSERT_LIBRARIES": "x"}}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"GITHUB_TOKEN": "ghp_x"}}),
            json!({"id": "a", "type": "stdio", "command": "x", "env": {"lower": "x"}}),
            json!({"id": "a", "type": "stdio", "url": "https://x.dev", "command": "x"}),
            json!({"id": "a", "type": "sse", "url": "https://x.dev"}),
            json!({"id": "A_b", "type": "http", "url": "https://x.dev"}),
            json!({"id": "a/b", "type": "http", "url": "https://x.dev"}),
        ];
        for b in bad {
            assert!(validate("demo", &[srv(b.clone())]).is_err(), "{b}");
        }
        let dup = srv(json!({"id": "a", "type": "http", "url": "https://x.dev"}));
        assert!(validate("demo", &[dup.clone(), dup]).unwrap_err().contains("duplicate"));
        // unknown fields (e.g. headers with a token) are refused at parse time
        assert!(serde_json::from_value::<PluginMcpServer>(json!({"id": "a", "type": "http", "url": "https://x.dev", "headers": {"Authorization": "x"}})).is_err());
        assert_eq!(server_id("demo", "search"), "demo-search");
    }

    #[test]
    fn consent_round_trip_and_invalidation() {
        let tmp = std::env::temp_dir().join(format!("vicuna-mcp-consent-{}-{}", std::process::id(), crate::plugins::now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        struct Dir(std::path::PathBuf);
        impl Dir { fn path(&self) -> &Path { &self.0 } }
        impl Drop for Dir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let dir = Dir(tmp);
        let s = srv(json!({"id": "files", "type": "stdio", "command": "files-mcp", "args": ["--stdio"]}));
        let list = vec![s.clone()];
        assert!(!consented(&read_consent(dir.path()), &s));
        assert_eq!(pending(&read_consent(dir.path()), &list).len(), 1);
        // a mismatched echo is refused
        assert!(grant(dir.path(), &list, &[json!({"id": "files", "type": "stdio", "command": "files-mcp", "args": ["--other"]})], 1).is_err());
        assert!(grant(dir.path(), &list, &[json!({"id": "nope", "type": "stdio"})], 1).is_err());
        grant(dir.path(), &list, &[json!({"id": "files", "type": "stdio", "command": "files-mcp", "args": ["--stdio"], "env": {}})], 1).unwrap();
        assert!(consented(&read_consent(dir.path()), &s));
        // unchanged after an upgrade -> still consented; changed args/url -> not
        let mut changed = s.clone();
        changed.args.push("--write".into());
        assert!(!consented(&read_consent(dir.path()), &changed));
        let mut env = s.clone();
        env.env.insert("FILES_MODE".into(), "rw".into());
        assert!(!consented(&read_consent(dir.path()), &env));
        revoke(dir.path(), Some("files")).unwrap();
        assert!(!consented(&read_consent(dir.path()), &s));

        let h = srv(json!({"id": "web", "type": "http", "url": "https://a.dev/mcp"}));
        grant(dir.path(), std::slice::from_ref(&h), &[json!({"id": "web", "type": "http", "url": "https://a.dev/mcp"})], 2).unwrap();
        let mut moved = h.clone();
        moved.url = Some("https://b.dev/mcp".into());
        assert!(consented(&read_consent(dir.path()), &h) && !consented(&read_consent(dir.path()), &moved));
        revoke(dir.path(), None).unwrap();
        assert!(read_consent(dir.path()).is_empty());
    }

    #[test]
    fn config_is_plugin_tagged_and_disabled() {
        let c = to_config("demo", "Demo", &srv(json!({"id": "web", "type": "http", "url": "https://a.dev/mcp"})));
        assert_eq!((c.id.as_str(), c.enabled, c.plugin_id.as_deref(), c.plugin_name.as_deref()), ("demo-web", false, Some("demo"), Some("Demo")));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!((v["pluginId"].as_str(), v["pluginName"].as_str()), (Some("demo"), Some("Demo")));
        let s = to_config("demo", "Demo", &srv(json!({"id": "f", "type": "stdio", "command": "definitely-not-installed-xyz", "args": ["a"]})));
        assert_eq!(s.command.unwrap(), vec!["definitely-not-installed-xyz".to_string(), "a".into()]);
    }
}
