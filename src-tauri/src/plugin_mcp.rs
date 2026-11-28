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
const MAX_SECRETS: usize = 8;
const MAX_SECRET_VALUE: usize = 4096;

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
    /// User-supplied secrets (API keys) passed to a stdio server as env.
    /// The manifest only declares them; values live in an owner-only file
    /// outside the plugin folder (`~/vicuna/config/plugins/<id>-mcp-secrets.json`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<PluginMcpSecret>,
}

/// One declared secret field (a key the user types in Configure).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginMcpSecret {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub required: bool,
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
        validate_secrets(s).map_err(e)?;
    }
    Ok(())
}

fn plain_text(v: &str, max: usize) -> bool {
    v.chars().count() <= max && !v.chars().any(char::is_control)
}

fn validate_secrets(s: &PluginMcpServer) -> Result<(), String> {
    if s.secrets.is_empty() {
        return Ok(());
    }
    if s.transport != "stdio" {
        return Err("secrets are passed as env, so only stdio servers can declare them (http servers sign in with OAuth)".into());
    }
    if s.secrets.len() > MAX_SECRETS {
        return Err(format!("at most {MAX_SECRETS} secrets"));
    }
    let mut seen = std::collections::HashSet::new();
    for x in &s.secrets {
        let k = x.key.as_str();
        if !valid_env_name(k) || forbidden_env(k) {
            return Err(format!("secret {k:?} is not allowed (UPPER_CASE env names; no PATH/HOME/loader variables)"));
        }
        if !seen.insert(k) {
            return Err(format!("secret {k:?} is declared twice"));
        }
        if s.env.contains_key(k) {
            return Err(format!("secret {k:?} collides with a static env variable"));
        }
        if x.label.trim().is_empty() || !plain_text(&x.label, 60) {
            return Err(format!("secret {k}: label must be 1-60 characters"));
        }
        if x.description.as_deref().is_some_and(|d| !plain_text(d, 200)) {
            return Err(format!("secret {k}: description must be at most 200 characters"));
        }
        if x.placeholder.as_deref().is_some_and(|d| !plain_text(d, 80)) {
            return Err(format!("secret {k}: placeholder must be at most 80 characters"));
        }
        if let Some(u) = &x.help_url {
            let ok = u.len() <= 500
                && reqwest::Url::parse(u).is_ok_and(|p| p.scheme() == "https" && p.host_str().is_some_and(|h| !h.is_empty()) && p.username().is_empty() && p.password().is_none());
            if !ok {
                return Err(format!("secret {k}: helpUrl must be an https:// URL"));
            }
        }
    }
    Ok(())
}

// ---------- user secrets (owner-only, never returned in plaintext) ----------

/// `server id -> KEY -> value` for one plugin.
pub type Secrets = BTreeMap<String, BTreeMap<String, String>>;

/// `~/vicuna/config/plugins` (`VICUNA_PLUGIN_SECRETS_DIR` overrides).
pub fn secrets_dir() -> Result<std::path::PathBuf, String> {
    if let Ok(p) = std::env::var("VICUNA_PLUGIN_SECRETS_DIR") {
        return Ok(p.into());
    }
    Ok(crate::opencode::home_dir().ok_or("no home folder")?.join("vicuna").join("config").join("plugins"))
}

pub fn secrets_path_in(dir: &Path, plugin_id: &str) -> std::path::PathBuf {
    dir.join(format!("{plugin_id}-mcp-secrets.json"))
}

pub fn read_secrets_in(dir: &Path, plugin_id: &str) -> Secrets {
    std::fs::read(secrets_path_in(dir, plugin_id)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn read_secrets(plugin_id: &str) -> Secrets {
    secrets_dir().map(|d| read_secrets_in(&d, plugin_id)).unwrap_or_default()
}

fn write_secrets_in(dir: &Path, plugin_id: &str, all: &Secrets) -> Result<(), String> {
    let path = secrets_path_in(dir, plugin_id);
    let all: Secrets = all.iter().filter(|(_, m)| !m.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
    if all.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{plugin_id}-mcp-secrets.{}.tmp", std::process::id()));
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        }
        f.write_all(&serde_json::to_vec_pretty(&all).unwrap()).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Update one server's secrets: `""` removes a key, omitted keys stay.
/// Only declared keys are accepted.
pub fn set_secrets_in(dir: &Path, plugin_id: &str, s: &PluginMcpServer, values: &serde_json::Map<String, Value>) -> Result<(), String> {
    let mut all = read_secrets_in(dir, plugin_id);
    let mine = all.entry(s.id.clone()).or_default();
    for (k, v) in values {
        if !s.secrets.iter().any(|x| &x.key == k) {
            return Err(format!("MCP server {} declares no secret {k:?}", s.id));
        }
        let v = v.as_str().ok_or_else(|| format!("secret {k}: value must be a string"))?.trim();
        if v.is_empty() {
            mine.remove(k);
        } else if v.chars().count() > MAX_SECRET_VALUE || v.chars().any(char::is_control) {
            return Err(format!("secret {k}: must be a single line of at most {MAX_SECRET_VALUE} characters"));
        } else {
            mine.insert(k.clone(), v.to_string());
        }
    }
    write_secrets_in(dir, plugin_id, &all)
}

pub fn clear_secrets_in(dir: &Path, plugin_id: &str, server: &str) -> Result<(), String> {
    let mut all = read_secrets_in(dir, plugin_id);
    all.remove(server);
    write_secrets_in(dir, plugin_id, &all)
}

/// Uninstall: forget every secret of the plugin.
pub fn remove_secrets_in(dir: &Path, plugin_id: &str) -> Result<(), String> {
    write_secrets_in(dir, plugin_id, &Secrets::new())
}

pub fn remove_secrets(plugin_id: &str) -> Result<(), String> {
    remove_secrets_in(&secrets_dir()?, plugin_id)
}

fn tail(v: &str) -> String {
    let c: Vec<char> = v.chars().collect();
    if c.len() <= 8 { String::new() } else { c[c.len() - 4..].iter().collect() }
}

/// Per-secret status for the UI: never the value, only the last 4 chars
/// (none for short values, which a tail would mostly reveal).
pub fn secret_status(s: &PluginMcpServer, stored: &Secrets) -> Vec<Value> {
    let mine = stored.get(&s.id);
    s.secrets.iter().map(|x| {
        let v = mine.and_then(|m| m.get(&x.key)).filter(|v| !v.is_empty());
        json!({
            "key": x.key, "label": x.label, "description": x.description, "helpUrl": x.help_url,
            "placeholder": x.placeholder, "required": x.required,
            "configured": v.is_some(), "tail": v.map(|v| tail(v)),
        })
    }).collect()
}

/// Declared + stored secrets for one server (only declared keys).
fn declared_values(s: &PluginMcpServer, stored: &Secrets) -> HashMap<String, String> {
    let Some(mine) = stored.get(&s.id) else { return HashMap::new() };
    s.secrets.iter().filter_map(|x| mine.get(&x.key).filter(|v| !v.is_empty()).map(|v| (x.key.clone(), v.clone()))).collect()
}

/// Merge stored secrets into a runtime config's env.
pub fn apply_secrets(cfg: &mut McpServerConfig, values: HashMap<String, String>) {
    if values.is_empty() || cfg.normalized_transport() != "stdio" {
        return;
    }
    cfg.env.get_or_insert_with(HashMap::new).extend(values);
}

#[cfg(test)]
thread_local! {
    /// Tests inject secret values per server id.
    pub static TEST_SECRETS: std::cell::RefCell<Option<HashMap<String, HashMap<String, String>>>> = const { std::cell::RefCell::new(None) };
}

/// The stored secrets for a plugin server id (empty for non-plugin ids).
/// Only consulted at connect/turn time, never for UI listings.
pub fn secrets_for(cfg: &McpServerConfig) -> HashMap<String, String> {
    #[cfg(test)]
    if let Some(v) = TEST_SECRETS.with(|t| t.borrow().clone()) {
        return v.get(&cfg.id).cloned().unwrap_or_default();
    }
    let Some(pid) = cfg.plugin_id.as_deref() else { return HashMap::new() };
    let Some((m, dir)) = crate::plugins::installed_manifest(pid) else { return HashMap::new() };
    let consent = read_consent(&dir);
    let stored = read_secrets(pid);
    m.mcp_servers.iter()
        .find(|s| server_id(pid, &s.id).eq_ignore_ascii_case(&cfg.id) && consented(&consent, s))
        .map(|s| declared_values(s, &stored))
        .unwrap_or_default()
}

/// Strip injected secret values from a config (defense in depth before any
/// UI-facing serialization).
pub fn redact(cfg: &mut McpServerConfig, values: &HashMap<String, String>) {
    if let Some(env) = cfg.env.as_mut() {
        env.retain(|k, _| !values.contains_key(k));
        if env.is_empty() {
            cfg.env = None;
        }
    }
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
        "secretStatus": secret_status(s, &read_secrets(plugin_id)),
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

    fn gem(secrets: Value) -> PluginMcpServer {
        srv(json!({"id": "gem", "type": "stdio", "command": "gem-mcp", "env": {"GEM_MODE": "ro"}, "secrets": secrets}))
    }

    #[test]
    fn secrets_validation() {
        let ok = gem(json!([
            {"key": "GEMINI_API_KEY", "label": "Gemini API key", "description": "From AI Studio",
             "helpUrl": "https://aistudio.google.com/apikey", "placeholder": "AIza…", "required": true},
            {"key": "GEM_ORG", "label": "Org"}
        ]));
        validate("demo", &[ok.clone()]).unwrap();
        assert!(!ok.secrets[1].required);
        let bad = [
            json!([{"key": "lower", "label": "x"}]),
            json!([{"key": "PATH", "label": "x"}]),
            json!([{"key": "LD_PRELOAD", "label": "x"}]),
            json!([{"key": "NODE_OPTIONS", "label": "x"}]),
            json!([{"key": "A_KEY", "label": "x"}, {"key": "A_KEY", "label": "y"}]),
            json!([{"key": "GEM_MODE", "label": "collides"}]),
            json!([{"key": "A_KEY", "label": ""}]),
            json!([{"key": "A_KEY", "label": "x".repeat(61)}]),
            json!([{"key": "A_KEY", "label": "x", "description": "d".repeat(201)}]),
            json!([{"key": "A_KEY", "label": "x", "placeholder": "p".repeat(81)}]),
            json!([{"key": "A_KEY", "label": "x", "helpUrl": "http://example.com"}]),
            json!([{"key": "A_KEY", "label": "x", "helpUrl": "javascript:alert(1)"}]),
            json!((0..9).map(|i| json!({"key": format!("K{i}"), "label": "x"})).collect::<Vec<_>>()),
        ];
        for b in bad {
            assert!(validate("demo", &[gem(b.clone())]).is_err(), "{b}");
        }
        // http servers sign in with OAuth, not env secrets
        let h = srv(json!({"id": "web", "type": "http", "url": "https://a.dev/mcp", "secrets": [{"key": "A_KEY", "label": "x"}]}));
        assert!(validate("demo", &[h]).unwrap_err().contains("stdio"));
        // unknown secret fields are refused
        assert!(serde_json::from_value::<PluginMcpServer>(json!({"id": "a", "type": "stdio", "command": "x", "secrets": [{"key": "A", "label": "x", "value": "baked"}]})).is_err());
    }

    #[test]
    fn secrets_persist_owner_only_masked_and_removed() {
        let dir = std::env::temp_dir().join(format!("vicuna-mcp-secrets-{}-{}", std::process::id(), crate::plugins::now_ms()));
        let s = gem(json!([{"key": "GEMINI_API_KEY", "label": "Key"}, {"key": "GEM_ORG", "label": "Org"}]));
        let m = |v: Value| v.as_object().unwrap().clone();
        set_secrets_in(&dir, "demo", &s, &m(json!({"GEMINI_API_KEY": "AIzaSy-abcdef-WXYZ", "GEM_ORG": "acme"}))).unwrap();
        let path = secrets_path_in(&dir, "demo");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // undeclared keys and multi-line values are refused
        assert!(set_secrets_in(&dir, "demo", &s, &m(json!({"OTHER": "x"}))).is_err());
        assert!(set_secrets_in(&dir, "demo", &s, &m(json!({"GEM_ORG": "a\nb"}))).is_err());
        let st = secret_status(&s, &read_secrets_in(&dir, "demo"));
        let text = serde_json::to_string(&st).unwrap();
        assert!(!text.contains("AIzaSy-abcdef") && !text.contains("acme"), "{text}");
        assert_eq!((st[0]["configured"].as_bool(), st[0]["tail"].as_str()), (Some(true), Some("WXYZ")));
        assert_eq!(st[1]["tail"].as_str(), Some(""), "short values show no tail");
        // omitted keys stay, "" removes
        set_secrets_in(&dir, "demo", &s, &m(json!({"GEM_ORG": ""}))).unwrap();
        let st = secret_status(&s, &read_secrets_in(&dir, "demo"));
        assert_eq!((st[0]["configured"].as_bool(), st[1]["configured"].as_bool()), (Some(true), Some(false)));
        assert!(st[1]["tail"].is_null());
        // injection merges only declared, stored keys into a stdio env
        let mut cfg = to_config("demo", "Demo", &s);
        apply_secrets(&mut cfg, declared_values(&s, &read_secrets_in(&dir, "demo")));
        let env = cfg.env.clone().unwrap();
        assert_eq!((env["GEMINI_API_KEY"].as_str(), env["GEM_MODE"].as_str()), ("AIzaSy-abcdef-WXYZ", "ro"));
        redact(&mut cfg, &declared_values(&s, &read_secrets_in(&dir, "demo")));
        assert!(!cfg.env.unwrap().contains_key("GEMINI_API_KEY"));
        clear_secrets_in(&dir, "demo", "gem").unwrap();
        assert!(!path.exists(), "an empty store leaves no file");
        set_secrets_in(&dir, "demo", &s, &m(json!({"GEM_ORG": "acme"}))).unwrap();
        remove_secrets_in(&dir, "demo").unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
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
