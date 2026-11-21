//! Harness plugins: a plugin can add a Work harness (coding agent) that
//! speaks ACP (Agent Client Protocol), driven exactly like the built-in ACP
//! engines (opencode, agy, fx) in `acp.rs`.
//!
//! Manifest (inside `vicuna-plugin.json`):
//!
//! ```json
//! "harnesses": [{
//!   "id": "opencode-acp", "label": "OpenCode (plugin)",
//!   "acp": { "command": "opencode", "args": ["acp"] },
//!   "models": [{ "id": "opencode/beam-free", "label": "Beam (free)" }],
//!   "installHint": "Install opencode: https://opencode.ai/docs/#install",
//!   "installUrl": "https://opencode.ai/docs/#install"
//! }]
//! ```
//!
//! Safety rules (enforced here, on the server, never in a window):
//! - Plugins never ship binaries. `command` is a bare program name
//!   (`[A-Za-z0-9._+-]`, no `/` or `\`, no `..`, no leading `-`/`.`) that
//!   Vicuna resolves on the user's terminal PATH (+ version-manager bins),
//!   the same places it finds the built-in CLIs. Absolute and relative
//!   paths are refused: a path inside a plugin would point at plugin-shipped
//!   or attacker-chosen files, and a bare name means the user installed it.
//!   Detection = that resolution; nothing is executed before consent.
//! - `args` is a list passed straight to the process: there is no shell and
//!   no interpolation. Arguments with control characters or shell
//!   metacharacters (`` ` $ ; | & < > ``, quotes, backslash) are refused
//!   anyway, so a manifest can't look harmless while hiding a shell trick
//!   for some wrapper that re-parses its argv.
//! - A harness is disabled until the user consents to the exact command +
//!   args. Consent is stored next to `install.json` (`consent.json`) and
//!   checked on every use: if an upgrade changes the command or args, the
//!   stored consent no longer matches and the harness is disabled again
//!   until the user approves the new command.
//!
//! Engine ids are `plugin:<plugin-id>/<harness-id>`; in memory the
//! `<plugin-id>/<harness-id>` key is interned so `Engine` stays `Copy`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const ENGINE_PREFIX: &str = "plugin:";
pub const MAX_HARNESSES: usize = 4;
const MAX_ARGS: usize = 32;
const MAX_ARG_LEN: usize = 256;
const MAX_MODELS: usize = 64;
const CONSENT: &str = "consent.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpLaunch {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessModel {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Harness {
    pub id: String,
    pub label: String,
    pub acp: AcpLaunch,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<HarnessModel>,
    #[serde(default)]
    pub install_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_url: Option<String>,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A bare program name resolved on PATH (see the module docs for why).
pub fn valid_command(c: &str) -> Result<(), String> {
    if c.is_empty() || c.len() > 64 {
        return Err("command must be 1-64 characters".into());
    }
    if c.contains('/') || c.contains('\\') || c.contains(':') {
        return Err(format!("command {c:?} must be a bare program name found on PATH, not a path"));
    }
    if c.contains("..") || c.starts_with('.') || c.starts_with('-') {
        return Err(format!("command {c:?} may not start with '.' or '-' or contain '..'"));
    }
    if !c.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)) {
        return Err(format!("command {c:?} may only use letters, digits and . _ + -"));
    }
    Ok(())
}

pub fn valid_arg(a: &str) -> Result<(), String> {
    if a.len() > MAX_ARG_LEN {
        return Err(format!("argument longer than {MAX_ARG_LEN} characters"));
    }
    if let Some(ch) = a.chars().find(|ch| ch.is_control() || "`$;|&<>\"'\\".contains(*ch)) {
        return Err(format!("argument {a:?} contains a disallowed character {ch:?} (no shell metacharacters)"));
    }
    Ok(())
}

pub fn validate(list: &[Harness]) -> Result<(), String> {
    if list.len() > MAX_HARNESSES {
        return Err(format!("at most {MAX_HARNESSES} harnesses per plugin"));
    }
    let mut seen = std::collections::HashSet::new();
    for h in list {
        if !valid_id(&h.id) || !seen.insert(h.id.as_str()) {
            return Err(format!("harness {:?}: invalid or duplicate id (lowercase letters, digits, dashes)", h.id));
        }
        let l = h.label.trim();
        if l.is_empty() || l.chars().count() > 40 || l.chars().any(char::is_control) {
            return Err(format!("harness {}: label must be 1-40 characters", h.id));
        }
        valid_command(&h.acp.command).map_err(|e| format!("harness {}: {e}", h.id))?;
        if h.acp.args.len() > MAX_ARGS {
            return Err(format!("harness {}: at most {MAX_ARGS} arguments", h.id));
        }
        for a in &h.acp.args {
            valid_arg(a).map_err(|e| format!("harness {}: {e}", h.id))?;
        }
        if h.models.len() > MAX_MODELS {
            return Err(format!("harness {}: at most {MAX_MODELS} models", h.id));
        }
        for m in &h.models {
            if m.id.is_empty() || m.id.len() > 128 || m.id.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err(format!("harness {}: invalid model id {:?}", h.id, m.id));
            }
        }
        if h.install_hint.chars().count() > 300 || h.install_hint.chars().any(|c| c.is_control() && c != '\n') {
            return Err(format!("harness {}: installHint must be at most 300 characters", h.id));
        }
        if let Some(u) = &h.install_url {
            if !u.starts_with("https://") || u.len() > 300 || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("harness {}: installUrl must be an https URL", h.id));
            }
        }
    }
    Ok(())
}

// ---------- consent ----------

/// What the user approved for one harness: exactly this launch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub command: String,
    pub args: Vec<String>,
    pub granted_at: i64,
}

pub type Consent = BTreeMap<String, Grant>;

pub fn read_consent(plugin_dir: &Path) -> Consent {
    std::fs::read(plugin_dir.join(CONSENT)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_consent(plugin_dir: &Path, c: &Consent) -> Result<(), String> {
    std::fs::write(plugin_dir.join(CONSENT), serde_json::to_vec_pretty(c).unwrap()).map_err(|e| e.to_string())
}

pub fn consented(consent: &Consent, h: &Harness) -> bool {
    consent.get(&h.id).is_some_and(|g| g.command == h.acp.command && g.args == h.acp.args)
}

/// Harnesses (of the current manifest) that need the user's approval.
pub fn pending(consent: &Consent, list: &[Harness]) -> Vec<Harness> {
    list.iter().filter(|h| !consented(consent, h)).cloned().collect()
}

/// Grant consent. The window echoes the exact command + args it showed the
/// user; anything that doesn't match the installed manifest is refused, so a
/// plugin changed between the dialog and the click can't slip through.
pub fn grant(plugin_dir: &Path, list: &[Harness], approved: &[Value], now: i64) -> Result<Consent, String> {
    let mut consent = read_consent(plugin_dir);
    for a in approved {
        let hid = a.get("id").and_then(Value::as_str).ok_or("each approved harness needs an id")?;
        let h = list.iter().find(|h| h.id == hid).ok_or_else(|| format!("this plugin has no harness {hid:?}"))?;
        let cmd = a.get("command").and_then(Value::as_str).unwrap_or_default();
        let args: Vec<String> = a
            .get("args")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if cmd != h.acp.command || args != h.acp.args {
            return Err(format!("harness {hid}: the command changed since it was shown; review it again"));
        }
        consent.insert(hid.to_string(), Grant { command: cmd.to_string(), args, granted_at: now });
    }
    write_consent(plugin_dir, &consent)?;
    Ok(consent)
}

pub fn revoke(plugin_dir: &Path, hid: Option<&str>) -> Result<Consent, String> {
    let mut consent = read_consent(plugin_dir);
    match hid {
        Some(h) => {
            consent.remove(h);
        }
        None => consent.clear(),
    }
    write_consent(plugin_dir, &consent)?;
    Ok(consent)
}

/// One harness as the Plugins panel / consent dialog renders it.
pub fn describe(h: &Harness, consent: &Consent, plugin_id: &str) -> Value {
    json!({
        "id": h.id, "label": h.label, "engine": format!("{ENGINE_PREFIX}{plugin_id}/{}", h.id),
        "command": h.acp.command, "args": h.acp.args, "models": h.models,
        "installHint": h.install_hint, "installUrl": h.install_url,
        "enabled": consented(consent, h),
        "installed": find_command(&h.acp.command).is_some(),
    })
}

// ---------- engine registry ----------

/// Intern a string for the program's lifetime. Bounded: one entry per
/// distinct harness key / label ever seen.
pub fn intern(s: &str) -> &'static str {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> = std::sync::OnceLock::new();
    let mut g = SET.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(x) = g.get(s) {
        return x;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    g.insert(leaked);
    leaked
}

/// `plugin:<plugin>/<harness>` -> interned `<plugin>/<harness>` key.
pub fn parse_engine(s: &str) -> Option<&'static str> {
    let rest = s.trim().strip_prefix(ENGINE_PREFIX)?;
    let (p, h) = rest.split_once('/')?;
    (valid_id(p) && valid_id(h)).then(|| intern(rest))
}

pub fn split_key(key: &str) -> (&str, &str) {
    key.split_once('/').unwrap_or((key, ""))
}

/// The installed harness behind a key, with whether the user consented.
pub struct Resolved {
    pub plugin_id: String,
    pub plugin_name: String,
    pub harness: Harness,
    pub enabled: bool,
}

pub fn lookup(key: &str) -> Option<Resolved> {
    let (pid, hid) = split_key(key);
    let (m, dir) = crate::plugins::installed_manifest(pid)?;
    let h = m.harnesses.iter().find(|h| h.id == hid)?.clone();
    let enabled = consented(&read_consent(&dir), &h);
    Some(Resolved { plugin_id: pid.to_string(), plugin_name: m.name, harness: h, enabled })
}

/// Every installed harness key (enabled or not), sorted.
pub fn all_keys() -> Vec<&'static str> {
    let mut out = Vec::new();
    for (m, _) in crate::plugins::installed_manifests() {
        for h in &m.harnesses {
            out.push(intern(&format!("{}/{}", m.id, h.id)));
        }
    }
    out.sort();
    out
}

/// Resolve a bare command on the terminal PATH + version-manager bins
/// without running it (no `--version` probes before/without consent).
pub fn find_command(cmd: &str) -> Option<PathBuf> {
    valid_command(cmd).ok()?;
    let home = crate::opencode::home_dir();
    let dirs = crate::exe_resolve::version_manager_dirs(home.as_deref());
    crate::exe_resolve::candidates(&[cmd], &[crate::exe_resolve::login_shell_path(), std::env::var_os("PATH")], &dirs)
        .into_iter()
        .next()
}

/// Why a plugin harness can't run right now, or None when it can.
pub fn unavailable(key: &str) -> Option<String> {
    let Some(r) = lookup(key) else {
        return Some(format!(
            "The plugin harness {ENGINE_PREFIX}{key} is no longer installed (its plugin was uninstalled or no longer provides it). Pick another harness for this session."
        ));
    };
    if !r.enabled {
        return Some(format!(
            "The harness \"{}\" from plugin {} is not enabled: approve the command it runs in Settings → Plugins.",
            r.harness.label, r.plugin_name
        ));
    }
    if find_command(&r.harness.acp.command).is_none() {
        let hint = if r.harness.install_hint.trim().is_empty() {
            format!("Install `{}` so it is on your PATH.", r.harness.acp.command)
        } else {
            r.harness.install_hint.clone()
        };
        return Some(format!("{} needs the `{}` command, which was not found. {hint}", r.harness.label, r.harness.acp.command));
    }
    None
}

pub fn label(key: &str) -> &'static str {
    match lookup(key) {
        Some(r) => intern(r.harness.label.trim()),
        None => intern(&format!("{ENGINE_PREFIX}{key}")),
    }
}

/// The launch for a turn. Refuses unless the user consented to exactly
/// this command + args (re-checked here, at spawn time).
pub fn launch(key: &str, workspace: &str) -> Result<(PathBuf, tokio::process::Command), String> {
    if let Some(e) = unavailable(key) {
        return Err(e);
    }
    let r = lookup(key).ok_or("plugin harness disappeared")?;
    let bin = find_command(&r.harness.acp.command).ok_or("command not found")?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(&r.harness.acp.args);
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    if !workspace.is_empty() {
        cmd.current_dir(workspace);
    }
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    Ok((bin, cmd))
}

// ---------- saved model per harness ----------

fn models_path() -> Option<PathBuf> {
    Some(crate::opencode::home_dir()?.join("vicuna").join("config").join("plugin-harness-models.json"))
}

fn read_models() -> serde_json::Map<String, Value> {
    models_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn saved_model(key: &str) -> Option<String> {
    read_models().get(key).and_then(Value::as_str).map(str::to_string)
}

pub fn save_model(key: &str, model: Option<&str>) -> Result<(), String> {
    let path = models_path().ok_or("cannot locate home folder")?;
    let mut m = read_models();
    match model.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => m.insert(key.to_string(), json!(v)),
        None => m.remove(key),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&Value::Object(m)).unwrap()).map_err(|e| e.to_string())
}

pub fn engine_info(key: &str) -> crate::opencode::WorkEngineInfo {
    let r = lookup(key);
    let missing = unavailable(key);
    let path = r.as_ref().and_then(|r| find_command(&r.harness.acp.command));
    crate::opencode::WorkEngineInfo {
        id: format!("{ENGINE_PREFIX}{key}"),
        label: label(key).to_string(),
        installed: missing.is_none(),
        missing,
        engines: Vec::new(),
        path: path.map(|p| p.display().to_string()),
        version: r.as_ref().map(|r| format!("(plugin: {})", r.plugin_name)),
        install_url: r.as_ref().and_then(|r| r.harness.install_url.clone()).unwrap_or_default(),
        model: saved_model(key),
        config_path: None,
        others: Vec::new(),
        warning: None,
        ask_permission: false,
    }
}

/// Static models from the manifest (the picker falls back to whatever the
/// agent reports over ACP when the manifest lists none).
pub fn manifest_models(key: &str) -> Vec<crate::opencode::OpencodeModel> {
    lookup(key)
        .map(|r| {
            r.harness
                .models
                .iter()
                .map(|m| model_entry(&m.id, m.label.as_deref()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn model_entry(id: &str, label: Option<&str>) -> crate::opencode::OpencodeModel {
    crate::opencode::OpencodeModel {
        id: id.to_string(),
        provider: id.split_once('/').map(|(p, _)| p.to_string()).unwrap_or_default(),
        name: label.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(cmd: &str, args: &[&str]) -> Harness {
        Harness {
            id: "demo".into(),
            label: "Demo".into(),
            acp: AcpLaunch { command: cmd.into(), args: args.iter().map(|s| s.to_string()).collect() },
            models: vec![],
            install_hint: String::new(),
            install_url: None,
        }
    }

    #[test]
    fn commands_must_be_bare_names() {
        for ok in ["opencode", "gemini", "goose", "claude-code-acp", "node22", "my_agent.exe", "c++agent"] {
            assert!(valid_command(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/usr/bin/opencode", "./agent", "../agent", "bin/agent", "C:\\agent.exe", "C:agent", "-rf",
            ".hidden", "a..b", "agent;rm", "agent x", "$(id)", "`id`", "ag|ent",
        ] {
            assert!(valid_command(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn args_reject_shell_tricks() {
        assert!(validate(&[h("opencode", &["acp", "--port=0", "--model", "a/b:c"])]).is_ok());
        for bad in ["$(id)", "`id`", "a;b", "a|b", "a&b", "a>b", "x\ny", "'q'", "\"q\"", "a\\b"] {
            assert!(validate(&[h("opencode", &[bad])]).is_err(), "{bad:?}");
        }
        assert!(validate(&[h("opencode", &[&"x".repeat(300)])]).is_err());
        let many: Vec<&str> = std::iter::repeat_n("a", 40).collect();
        assert!(validate(&[h("opencode", &many)]).is_err());
    }

    #[test]
    fn manifest_shape_is_checked() {
        let mut x = h("opencode", &["acp"]);
        x.id = "Bad Id".into();
        assert!(validate(&[x]).is_err());
        let dup = vec![h("a", &[]), h("b", &[])];
        assert!(validate(&dup).is_err(), "duplicate ids");
        let mut u = h("opencode", &[]);
        u.install_url = Some("http://x".into());
        assert!(validate(&[u]).is_err());
        assert!(validate(&vec![h("a", &[]); 5]).is_err());
    }

    #[test]
    fn engine_ids_parse_and_intern() {
        let k = parse_engine("plugin:sample/opencode-acp").unwrap();
        assert_eq!(k, "sample/opencode-acp");
        assert!(std::ptr::eq(k, parse_engine(" plugin:sample/opencode-acp ").unwrap()));
        assert_eq!(parse_engine("plugin:../x"), None);
        assert_eq!(parse_engine("plugin:a"), None);
        assert_eq!(parse_engine("opencode"), None);
        assert_eq!(split_key(k), ("sample", "opencode-acp"));
    }

    #[test]
    fn consent_is_bound_to_the_exact_command() {
        let dir = std::env::temp_dir().join(format!("vicuna-consent-{}-{}", std::process::id(), crate::plugins::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = vec![h("opencode", &["acp"])];
        // no consent -> disabled, pending
        assert!(!consented(&read_consent(&dir), &v1[0]));
        assert_eq!(pending(&read_consent(&dir), &v1).len(), 1);
        // echo mismatch refused
        assert!(grant(&dir, &v1, &[json!({"id":"demo","command":"opencode","args":["acp","--evil"]})], 1).is_err());
        assert!(grant(&dir, &v1, &[json!({"id":"nope","command":"opencode","args":["acp"]})], 1).is_err());
        grant(&dir, &v1, &[json!({"id":"demo","command":"opencode","args":["acp"]})], 1).unwrap();
        assert!(consented(&read_consent(&dir), &v1[0]));
        // an upgrade that changes the command or the args needs fresh consent
        assert!(!consented(&read_consent(&dir), &h("opencode", &["acp", "--yolo"])));
        assert!(!consented(&read_consent(&dir), &h("gemini", &["acp"])));
        // unchanged command keeps it
        assert!(consented(&read_consent(&dir), &h("opencode", &["acp"])));
        revoke(&dir, Some("demo")).unwrap();
        assert!(!consented(&read_consent(&dir), &v1[0]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
