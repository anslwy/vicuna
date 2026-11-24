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
//! Optional `icon: "agent.svg"` on each harness (or top-level `icon` fallback)
//! ships a relative SVG asset. The installer stores it under `files/`; detection
//! exposes a validated, at-most-32-KiB SVG as a base64 data URI. Invalid paths,
//! missing/unsafe files and symlink escapes use the puzzle icon. Icons render
//! only in image elements; see `plugin_icon.rs` for the supported SVG subset.
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
use crate::cli_accounts::Registry as AccountRegistry;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub acp: AcpLaunch,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<HarnessModel>,
    #[serde(default)]
    pub install_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_url: Option<String>,
    /// Optional subscription-usage command + a declarative map of its JSON
    /// output (see `UsageSpec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageSpec>,
    /// Named accounts: `"muse"` or a declarative `AccountSpec` (folder
    /// names only; see `validate_accounts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<AccountProvider>,
}

/// Named-account support for a harness. Either a fixed Vicuna adapter by
/// name (`"muse"`, published before the declarative form) or a declarative
/// `AccountSpec` from the manifest. Both run through the same code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum AccountProvider {
    Named(NamedAccounts),
    Custom(AccountSpec),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NamedAccounts {
    Muse,
}

/// Declarative account separation. A manifest picks only folder NAMES:
/// every `env` value becomes a path inside that account's own folder, so a
/// plugin can never point a variable at arbitrary text or paths. Variable
/// names are limited to folder-style names (`valid_account_env`), `clear`
/// can only remove variables, `login` follows the `acp` command rules (and
/// is part of consent), and `signedIn` only reads a file in the account
/// folder (JSON, comments allowed; signed in = the pointer has a value).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSpec {
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clear: Vec<String>,
    pub login: Vec<AcpLaunch>,
    pub signed_in: SignedInCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignedInCheck {
    pub file: String,
    pub pointer: String,
}

const MAX_ACCOUNT_ENV: usize = 8;
const MAX_ACCOUNT_CLEAR: usize = 16;
const MAX_ACCOUNT_LOGIN: usize = 3;

fn muse_spec() -> AccountSpec {
    let env = [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")];
    AccountSpec {
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        clear: vec!["META_API_KEY".into()],
        login: vec![AcpLaunch { command: "muse".into(), args: vec!["login".into()] }],
        // Muse writes the Meta provider entry on login and removes its
        // credentials on logout.
        signed_in: SignedInCheck { file: "config/muse/auth.json".into(), pointer: "/providers/meta".into() },
    }
}

impl AccountProvider {
    pub fn spec(&self) -> std::borrow::Cow<'_, AccountSpec> {
        match self {
            Self::Named(NamedAccounts::Muse) => std::borrow::Cow::Owned(muse_spec()),
            Self::Custom(s) => std::borrow::Cow::Borrowed(s),
        }
    }

    pub fn environment(&self, dir: &Path) -> Vec<(String, PathBuf)> {
        self.spec().env.iter().map(|(k, v)| (k.clone(), dir.join(v))).collect()
    }

    /// Apply this account's environment to a child process: drop the
    /// credential overrides first, then point the folders at the account.
    pub fn configure(&self, dir: &Path, cmd: &mut impl EnvTarget) {
        for key in &self.spec().clear {
            cmd.remove(key);
        }
        for (key, path) in self.environment(dir) {
            cmd.set(&key, &path);
        }
    }

    pub fn sign_in_command(&self, dir: &Path) -> String {
        let spec = self.spec();
        let vars = self.environment(dir);
        let line = |l: &AcpLaunch| std::iter::once(l.command.clone()).chain(l.args.iter().cloned()).collect::<Vec<_>>().join(" ");
        if cfg!(windows) {
            let clear = spec.clear.iter().map(|k| format!("$env:{k} = $null; ")).collect::<String>();
            let assignments = vars.iter().map(|(key, path)| {
                format!("$env:{key} = '{}'; ", path.to_string_lossy().replace('\'', "''"))
            }).collect::<String>();
            format!("{clear}{assignments}{}", spec.login.iter().map(line).collect::<Vec<_>>().join("; "))
        } else {
            let mut prefix = vec!["env".to_string()];
            prefix.extend(spec.clear.iter().map(|k| format!("-u {k}")));
            prefix.extend(vars.iter().map(|(key, path)| format!("{key}={}", crate::agy_accounts::shell_quote(&path.to_string_lossy()))));
            let prefix = prefix.join(" ");
            spec.login.iter().map(|l| format!("{prefix} {}", line(l))).collect::<Vec<_>>().join(" && ")
        }
    }

    /// Reads one small file in the account folder; never runs anything.
    pub fn signed_in(&self, dir: &Path) -> bool {
        let check = &self.spec().signed_in;
        let Ok(text) = std::fs::read_to_string(dir.join(&check.file)) else { return false };
        if text.len() > 1 << 20 {
            return false;
        }
        serde_json::from_str::<Value>(&strip_json_comments(&text)).ok()
            .and_then(|v| v.pointer(&check.pointer).cloned())
            .is_some_and(|v| match v {
                Value::Null | Value::Bool(false) => false,
                Value::String(s) => !s.trim().is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            })
    }
}

/// Environment edits shared by std and tokio commands.
pub trait EnvTarget {
    fn remove(&mut self, key: &str);
    fn set(&mut self, key: &str, value: &Path);
}

impl EnvTarget for std::process::Command {
    fn remove(&mut self, key: &str) { self.env_remove(key); }
    fn set(&mut self, key: &str, value: &Path) { self.env(key, value); }
}

impl EnvTarget for tokio::process::Command {
    fn remove(&mut self, key: &str) { self.env_remove(key); }
    fn set(&mut self, key: &str, value: &Path) { self.env(key, value); }
}

/// Drop `//` and `/* */` comments outside strings (CLIs like Copilot keep
/// state in JSON with comments).
fn strip_json_comments(s: &str) -> String {
    let (b, mut out, mut i, mut in_str) = (s.as_bytes(), Vec::with_capacity(s.len()), 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            out.push(c);
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' { i += 1; }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') { i += 1; }
            i += 2;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn valid_env_name(k: &str) -> bool {
    (1..=64).contains(&k.len())
        && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !k.as_bytes()[0].is_ascii_digit()
}

/// Folder-style variables only. Anything that loads code or changes how
/// programs are found (PATH, LD_*/DYLD_*, NODE_OPTIONS, PYTHONPATH, …) has
/// no folder-style name, and HOME itself is refused (an empty HOME breaks
/// the shell tools the agent runs).
fn valid_account_env(k: &str) -> bool {
    const SUFFIXES: [&str; 5] = ["_HOME", "_CONFIG_DIR", "_DATA_DIR", "_STATE_DIR", "_CACHE_DIR"];
    valid_env_name(k) && k != "HOME" && !k.starts_with("LD_") && !k.starts_with("DYLD_")
        && SUFFIXES.iter().any(|s| k.len() > s.len() && k.ends_with(s))
}

fn valid_segment(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s != "." && s != ".."
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn validate_accounts(a: &AccountProvider) -> Result<(), String> {
    let AccountProvider::Custom(s) = a else { return Ok(()) };
    if s.env.is_empty() || s.env.len() > MAX_ACCOUNT_ENV {
        return Err(format!("accounts.env needs 1-{MAX_ACCOUNT_ENV} variables"));
    }
    for (k, v) in &s.env {
        if !valid_account_env(k) {
            return Err(format!("accounts.env: {k:?} is not allowed (folder variables only: *_HOME, *_CONFIG_DIR, *_DATA_DIR, *_STATE_DIR, *_CACHE_DIR, XDG_*_HOME; never HOME, PATH or loader variables)"));
        }
        if !valid_segment(v) {
            return Err(format!("accounts.env.{k}: {v:?} must be a plain folder name (letters, digits, . _ -)"));
        }
    }
    if s.clear.len() > MAX_ACCOUNT_CLEAR || s.clear.iter().any(|k| !valid_env_name(k)) {
        return Err(format!("accounts.clear: at most {MAX_ACCOUNT_CLEAR} variable names (A-Z, 0-9, _)"));
    }
    if s.login.is_empty() || s.login.len() > MAX_ACCOUNT_LOGIN {
        return Err(format!("accounts.login needs 1-{MAX_ACCOUNT_LOGIN} commands"));
    }
    for l in &s.login {
        valid_command(&l.command).map_err(|e| format!("accounts.login: {e}"))?;
        if l.args.len() > MAX_ARGS {
            return Err(format!("accounts.login: at most {MAX_ARGS} arguments"));
        }
        for x in &l.args {
            valid_arg(x).map_err(|e| format!("accounts.login: {e}"))?;
        }
    }
    let segs: Vec<&str> = s.signed_in.file.split('/').collect();
    if segs.len() > 4 || !segs.iter().all(|x| valid_segment(x)) {
        return Err("accounts.signedIn.file must be a relative path inside the account folder (up to 4 plain folder/file names)".into());
    }
    let p = &s.signed_in.pointer;
    if !p.starts_with('/') || p.len() > 200 || p.chars().any(char::is_control) {
        return Err("accounts.signedIn.pointer must be a JSON pointer like /users/0".into());
    }
    Ok(())
}

/// `usage`: a command printing JSON (same rules as `acp`: bare program on
/// PATH, args list, no shell) and a map from JSON pointers (RFC 6901, e.g.
/// `/allowance/weekly/usedPercent`) to the Usage view's fields. No plugin
/// code runs: Vicuna reads the output itself (JSON only, capped at
/// `USAGE_MAX_BYTES` / `USAGE_TIMEOUT_SECS`).
///
/// ```json
/// "usage": {
///   "command": "muse", "args": ["usage", "--json"],
///   "map": {
///     "plan": "/allowance/tier",
///     "note": "/hint",
///     "meters": [
///       { "label": "Weekly", "usedPercent": "/allowance/weekly/usedPercent", "resetsAt": "/allowance/weekly/resetsAtMs" },
///       { "each": "/limits", "labelPath": "/name", "used": "/used", "limit": "/max", "resetsAt": "/resetsAt" }
///     ]
///   }
/// }
/// ```
///
/// Per meter: `label` (fixed text) or `labelPath`; one of `usedPercent`,
/// `remainingPercent` or `used` + `limit`; optional `resetsAt` (ISO 8601 or
/// epoch seconds/milliseconds) and `group`. With `each`, the meter repeats
/// for every element of that array and its other paths are relative to the
/// element. A meter whose value is missing is skipped; no meters at all
/// reports usage as unknown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub map: UsageMap,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub meters: Vec<UsageMeterSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageMeterSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_percent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

pub const USAGE_MAX_BYTES: usize = 256 * 1024;
pub const USAGE_TIMEOUT_SECS: u64 = 15;
const MAX_METERS: usize = 16;

impl UsageSpec {
    pub fn launch(&self) -> AcpLaunch {
        AcpLaunch { command: self.command.clone(), args: self.args.clone() }
    }
}

fn valid_pointer(p: &str) -> bool {
    (p.is_empty() || p.starts_with('/')) && p.len() <= 200 && !p.chars().any(char::is_control)
}

fn valid_text(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().count() <= 40 && !t.chars().any(char::is_control)
}

fn validate_usage(u: &UsageSpec) -> Result<(), String> {
    valid_command(&u.command).map_err(|e| format!("usage: {e}"))?;
    if u.args.len() > MAX_ARGS {
        return Err(format!("usage: at most {MAX_ARGS} arguments"));
    }
    for a in &u.args {
        valid_arg(a).map_err(|e| format!("usage: {e}"))?;
    }
    let m = &u.map;
    for p in [&m.plan, &m.note].into_iter().flatten() {
        if !valid_pointer(p) {
            return Err(format!("usage: {p:?} is not a JSON pointer (\"/a/b\")"));
        }
    }
    if m.meters.is_empty() || m.meters.len() > MAX_METERS {
        return Err(format!("usage: map.meters needs 1-{MAX_METERS} entries"));
    }
    for (i, x) in m.meters.iter().enumerate() {
        let paths = [&x.each, &x.label_path, &x.used_percent, &x.remaining_percent, &x.used, &x.limit, &x.resets_at];
        if let Some(bad) = paths.into_iter().flatten().find(|p| !valid_pointer(p)) {
            return Err(format!("usage: meter {i}: {bad:?} is not a JSON pointer (\"/a/b\")"));
        }
        for t in [&x.label, &x.group].into_iter().flatten() {
            if !valid_text(t) {
                return Err(format!("usage: meter {i}: labels must be 1-40 characters"));
            }
        }
        if x.label.is_none() && x.label_path.is_none() {
            return Err(format!("usage: meter {i} needs a label or labelPath"));
        }
        let ways = x.used_percent.is_some() as u8 + x.remaining_percent.is_some() as u8 + (x.used.is_some() || x.limit.is_some()) as u8;
        if ways != 1 || x.used.is_some() != x.limit.is_some() {
            return Err(format!("usage: meter {i} needs exactly one of usedPercent, remainingPercent or used + limit"));
        }
    }
    Ok(())
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
        if let Some(u) = &h.usage {
            validate_usage(u).map_err(|e| format!("harness {}: {e}", h.id))?;
        }
        if let Some(a) = &h.accounts {
            validate_accounts(a).map_err(|e| format!("harness {}: {e}", h.id))?;
        }
        if h.accounts == Some(AccountProvider::Named(NamedAccounts::Muse))
            && (h.acp.command != "muse" || h.acp.args.first().map(String::as_str) != Some("acp")
                || h.usage.as_ref().is_some_and(|u| u.command != "muse" || u.args != ["usage", "--json"]))
        {
            return Err(format!("harness {}: Muse accounts require muse acp and, if provided, muse usage --json", h.id));
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
    /// The usage command approved with it (None = the harness had none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AcpLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<AccountProvider>,
}

pub type Consent = BTreeMap<String, Grant>;

pub fn read_consent(plugin_dir: &Path) -> Consent {
    std::fs::read(plugin_dir.join(CONSENT)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_consent(plugin_dir: &Path, c: &Consent) -> Result<(), String> {
    std::fs::write(plugin_dir.join(CONSENT), serde_json::to_vec_pretty(c).unwrap()).map_err(|e| e.to_string())
}

pub fn consented(consent: &Consent, h: &Harness) -> bool {
    consent.get(&h.id).is_some_and(|g| {
        g.command == h.acp.command && g.args == h.acp.args && g.usage == h.usage.as_ref().map(UsageSpec::launch) && g.accounts == h.accounts
    })
}

fn launch_of(v: Option<&Value>) -> Option<AcpLaunch> {
    let v = v.filter(|v| !v.is_null())?;
    Some(AcpLaunch {
        command: v.get("command").and_then(Value::as_str).unwrap_or_default().to_string(),
        args: v
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    })
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
        let usage = launch_of(a.get("usage"));
        let accounts: Option<AccountProvider> = serde_json::from_value(a.get("accounts").cloned().unwrap_or(Value::Null))
            .map_err(|_| format!("harness {hid}: invalid account provider"))?;
        if accounts != h.accounts || cmd != h.acp.command || args != h.acp.args || usage != h.usage.as_ref().map(UsageSpec::launch) {
            return Err(format!("harness {hid}: the command changed since it was shown; review it again"));
        }
        consent.insert(hid.to_string(), Grant { command: cmd.to_string(), args, granted_at: now, usage, accounts });
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
        "accounts": h.accounts,
        "installed": find_command(&h.acp.command).is_some(),
        "usage": h.usage.as_ref().map(|u| json!({"command": u.command, "args": u.args})),
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
    let (base, account) = split_account(rest);
    let (p, h) = base.split_once('/')?;
    (valid_id(p) && valid_id(h) && account.is_none_or(crate::agy_accounts::valid_id))
        .then(|| intern(rest))
}

pub fn split_account(key: &str) -> (&str, Option<&str>) {
    key.split_once('@').map_or((key, None), |(base, id)| (base, Some(id)))
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
    let (pid, hid) = split_key(split_account(key).0);
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
            let key = intern(&format!("{}/{}", m.id, h.id));
            out.push(key);
            if h.accounts.is_some() {
                out.extend(AccountRegistry::Plugin(key).list().iter()
                    .map(|account| intern(&format!("{key}@{}", account.id))));
            }
        }
    }
    out.sort();
    out
}

pub fn account_bases() -> Vec<String> {
    all_keys().into_iter()
        .filter(|key| split_account(key).1.is_none())
        .filter(|key| account_provider(key).is_ok())
        .map(|key| format!("{ENGINE_PREFIX}{key}"))
        .collect()
}

pub fn account_provider(key: &str) -> Result<AccountProvider, String> {
    lookup(key).filter(|r| r.enabled).and_then(|r| r.harness.accounts)
        .ok_or_else(|| "Enable an installed harness that supports extra accounts in Settings → Plugins.".into())
}

pub fn account_info(key: &str) -> Option<crate::agy_accounts::AccountInfo> {
    let (base, id) = split_account(key);
    let provider = lookup(base)?.harness.accounts?;
    crate::cli_accounts::plugin_info(intern(base), id?, provider)
}

fn account_profile(key: &str, r: &Resolved) -> Result<Option<(AccountProvider, PathBuf)>, String> {
    let (base, id) = split_account(key);
    let Some(id) = id else { return Ok(None) };
    let provider = r.harness.accounts.clone().ok_or("This plugin harness no longer supports extra accounts.")?;
    let dir = AccountRegistry::Plugin(intern(base)).checked_profile(id)?;
    Ok(Some((provider, dir)))
}

fn sign_in_hint(profile: Option<&(AccountProvider, PathBuf)>) -> Option<String> {
    let (provider, dir) = profile?;
    (!provider.signed_in(dir)).then(|| format!(
        "To sign in to this account, run `{}` in a terminal on this machine.",
        provider.sign_in_command(dir)
    ))
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
    if let Err(err) = account_profile(key, &r) {
        return Some(err);
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
        Some(r) => {
            let (base, id) = split_account(key);
            match id {
                Some(id) => {
                    let name = AccountRegistry::Plugin(intern(base)).find(id)
                        .map(|a| a.label).unwrap_or_else(|| id.to_string());
                    intern(&format!("{} · {name}", r.harness.label.trim()))
                }
                None => intern(r.harness.label.trim()),
            }
        }
        None => intern(&format!("{ENGINE_PREFIX}{key}")),
    }
}

/// The launch for a turn. Refuses unless the user consented to exactly
/// this command + args (re-checked here, at spawn time).
pub fn launch(key: &str, workspace: &str) -> Result<(PathBuf, tokio::process::Command), String> {
    let r = lookup(key).ok_or_else(|| format!("The plugin harness {ENGINE_PREFIX}{key} is no longer installed."))?;
    if !r.enabled {
        return Err("This harness is not enabled. Approve its commands in Settings → Plugins.".into());
    }
    let profile = account_profile(key, &r)?;
    if let Some(hint) = sign_in_hint(profile.as_ref()) {
        return Err(hint);
    }
    let bin = find_command(&r.harness.acp.command).ok_or("command not found")?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(&r.harness.acp.args);
    if let Some((provider, dir)) = profile {
        provider.configure(&dir, &mut cmd);
    }
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

// ---------- usage command ----------

/// Whether this harness reports usage (it has a usage map and is enabled).
pub fn reports_usage(key: &str) -> bool {
    lookup(key).is_some_and(|r| r.enabled && r.harness.usage.is_some() && account_profile(key, &r).is_ok())
}

/// Run the consented usage command and return (map, parsed JSON output).
/// Refuses unless the user approved exactly this usage command.
pub fn run_usage(key: &str) -> Result<(UsageMap, Value), String> {
    let r = lookup(key).ok_or("this plugin harness is no longer installed")?;
    let u = r.harness.usage.clone().ok_or_else(|| format!("{} does not report usage.", r.harness.label))?;
    if !r.enabled {
        return Err(format!(
            "\"{}\" is not enabled: approve its commands in Settings → Plugins before Vicuna reads its usage.",
            r.harness.label
        ));
    }
    let bin = find_command(&u.command).ok_or_else(|| format!("the usage command `{}` was not found on PATH", u.command))?;
    let profile = account_profile(key, &r)?;
    if let Some(hint) = sign_in_hint(profile.as_ref()) {
        return Err(hint);
    }
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&u.args);
    if let Some((provider, dir)) = profile {
        provider.configure(&dir, &mut cmd);
    }
    let out = run_capped(cmd, &bin, USAGE_MAX_BYTES, std::time::Duration::from_secs(USAGE_TIMEOUT_SECS))?;
    let v: Value = serde_json::from_slice(&out).map_err(|_| format!("`{}` did not print JSON", u.command))?;
    Ok((u.map, v))
}

/// Spawn `bin args` (no shell, cwd = home), read at most `max` bytes of
/// stdout and kill it after `timeout`.
fn run_capped(mut cmd: std::process::Command, bin: &Path, max: usize, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    if let Some(h) = crate::opencode::home_dir() {
        cmd.current_dir(h);
    }
    cmd.env("NO_COLOR", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = (&mut stdout).take(max as u64 + 1).read_to_end(&mut buf);
        let _ = tx.send(r.map(|_| buf));
    });
    let got = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    match got {
        Ok(Ok(buf)) if buf.len() > max => Err(format!("usage output is larger than {} KB", max / 1024)),
        Ok(Ok(buf)) => Ok(buf),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("the usage command took longer than {}s", timeout.as_secs())),
    }
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
        config_path: account_info(key).map(|a| a.home),
        others: Vec::new(),
        warning: sign_in_hint(r.as_ref().and_then(|r| account_profile(key, r).ok().flatten()).as_ref()),
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
            icon: None,
            acp: AcpLaunch { command: cmd.into(), args: args.iter().map(|s| s.to_string()).collect() },
            models: vec![],
            install_hint: String::new(),
            install_url: None,
            usage: None,
            accounts: None,
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

    fn usage_spec(cmd: &str, args: &[&str]) -> UsageSpec {
        serde_json::from_value(json!({
            "command": cmd, "args": args,
            "map": {"meters": [{"label": "Weekly", "usedPercent": "/w/used"}]}
        }))
        .unwrap()
    }

    #[test]
    fn usage_command_and_map_are_validated() {
        let mut x = h("muse", &["acp"]);
        x.usage = Some(usage_spec("muse", &["usage", "--json"]));
        assert!(validate(&[x.clone()]).is_ok());
        for (cmd, args) in [("/bin/muse", vec!["usage"]), ("muse", vec!["usage;rm"]), ("muse", vec!["$(id)"]), ("../muse", vec![])] {
            x.usage = Some(usage_spec(cmd, &args));
            assert!(validate(&[x.clone()]).is_err(), "{cmd} {args:?}");
        }
        let bad_maps = [
            json!({"meters": []}),
            json!({"meters": [{"label": "W", "usedPercent": "w/used"}]}),
            json!({"meters": [{"usedPercent": "/a"}]}),
            json!({"meters": [{"label": "W"}]}),
            json!({"meters": [{"label": "W", "usedPercent": "/a", "remainingPercent": "/b"}]}),
            json!({"meters": [{"label": "W", "used": "/a"}]}),
            json!({"plan": "plan", "meters": [{"label": "W", "usedPercent": "/a"}]}),
        ];
        for m in bad_maps {
            x.usage = Some(serde_json::from_value(json!({"command": "muse", "args": ["usage"], "map": m.clone()})).unwrap());
            assert!(validate(&[x.clone()]).is_err(), "{m}");
        }
    }

    #[test]
    fn usage_command_is_part_of_consent() {
        let dir = std::env::temp_dir().join(format!("vicuna-uconsent-{}-{}", std::process::id(), crate::plugins::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut x = h("muse", &["acp"]);
        x.usage = Some(usage_spec("muse", &["usage", "--json"]));
        let list = vec![x.clone()];
        // approving only the acp command (dialog without the usage line) is refused
        assert!(grant(&dir, &list, &[json!({"id":"demo","command":"muse","args":["acp"]})], 1).is_err());
        assert!(grant(&dir, &list, &[json!({"id":"demo","command":"muse","args":["acp"],"usage":{"command":"muse","args":["usage"]}})], 1).is_err());
        grant(&dir, &list, &[json!({"id":"demo","command":"muse","args":["acp"],"usage":{"command":"muse","args":["usage","--json"]}})], 1).unwrap();
        assert!(consented(&read_consent(&dir), &x));
        // a changed usage command needs fresh consent
        let mut y = x.clone();
        y.usage = Some(usage_spec("muse", &["usage", "--all"]));
        assert!(!consented(&read_consent(&dir), &y));
        // an upgrade adding usage to a harness approved without it, too
        let mut z = x.clone();
        z.usage = None;
        assert!(!consented(&read_consent(&dir), &z));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabling_plugin_accounts_requires_fresh_consent() {
        let dir = std::env::temp_dir().join(format!("vicuna-profile-consent-{}-{}", std::process::id(), crate::plugins::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut harness = h("muse", &["acp"]);
        let old = json!({"id": "demo", "command": "muse", "args": ["acp"]});
        grant(&dir, &[harness.clone()], &[old.clone()], 1).unwrap();

        harness.accounts = Some(AccountProvider::Named(NamedAccounts::Muse));
        validate(&[harness.clone()]).unwrap();
        assert!(!consented(&read_consent(&dir), &harness));
        assert!(grant(&dir, &[harness.clone()], &[old], 2).is_err());
        let approved = json!({"id": "demo", "command": "muse", "args": ["acp"], "accounts": "muse"});
        let consent = grant(&dir, &[harness.clone()], &[approved], 2).unwrap();
        assert!(consented(&consent, &harness));

        harness.acp.command = "other-agent".into();
        assert!(validate(&[harness]).is_err());
        assert!(parse_engine("plugin:muse/muse@work").is_some());
        assert!(parse_engine("plugin:muse/muse@../work").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn copilot_spec() -> AccountProvider {
        serde_json::from_value(json!({
            "env": {"COPILOT_HOME": "copilot", "GH_CONFIG_DIR": "gh"},
            "clear": ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
            "login": [{"command": "copilot", "args": ["login"]}, {"command": "gh", "args": ["auth", "login", "--web"]}],
            "signedIn": {"file": "copilot/config.json", "pointer": "/loggedInUsers/0"}
        })).unwrap()
    }

    #[test]
    fn declarative_accounts_are_validated() {
        let mut x = h("copilot", &["--acp", "--stdio"]);
        x.accounts = Some(copilot_spec());
        validate(&[x.clone()]).unwrap();
        assert_eq!(serde_json::from_value::<AccountProvider>(json!("muse")).unwrap(), AccountProvider::Named(NamedAccounts::Muse));
        assert!(serde_json::from_value::<AccountProvider>(json!("copilot")).is_err());

        let bad = |patch: Value| {
            let mut v = serde_json::to_value(copilot_spec()).unwrap();
            for (k, val) in patch.as_object().unwrap() { v[k] = val.clone(); }
            match serde_json::from_value::<AccountProvider>(v) {
                Err(_) => true,
                Ok(a) => { let mut y = x.clone(); y.accounts = Some(a); validate(&[y]).is_err() }
            }
        };
        // Variables that load code, change lookup or aren't folder-style.
        for name in ["PATH", "HOME", "NODE_OPTIONS", "PYTHONPATH", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "LD_CONFIG_DIR", "PYTHONHOME", "_HOME", "gh_config_dir", "X-HOME"] {
            assert!(bad(json!({"env": {name: "x"}})), "{name}");
        }
        // Values are folder names only.
        for v in ["..", ".", "a/b", "/abs", "~", "$(id)", "a b", ""] {
            assert!(bad(json!({"env": {"COPILOT_HOME": v}})), "{v:?}");
        }
        assert!(bad(json!({"env": {}})));
        assert!(bad(json!({"login": []})));
        assert!(bad(json!({"login": [{"command": "/bin/sh", "args": []}]})));
        assert!(bad(json!({"login": [{"command": "gh", "args": ["auth;rm -rf ~"]}]})));
        assert!(bad(json!({"signedIn": {"file": "../x.json", "pointer": "/a"}})));
        assert!(bad(json!({"signedIn": {"file": "/etc/passwd", "pointer": "/a"}})));
        assert!(bad(json!({"signedIn": {"file": "a.json", "pointer": "a"}})));
        assert!(bad(json!({"clear": ["GH TOKEN"]})));
        assert!(bad(json!({"shell": "x"})), "unknown fields are refused");
        assert!(!bad(json!({"env": {"XDG_CONFIG_HOME": "c", "FOO_CACHE_DIR": "k"}})));
    }

    #[test]
    fn declarative_accounts_isolate_and_detect_login() {
        let a = copilot_spec();
        let dir = Path::new("/tmp/acct one");
        assert_eq!(a.environment(dir), vec![("COPILOT_HOME".to_string(), dir.join("copilot")), ("GH_CONFIG_DIR".to_string(), dir.join("gh"))]);
        if !cfg!(windows) {
            let cmd = a.sign_in_command(dir);
            assert!(cmd.starts_with("env -u COPILOT_GITHUB_TOKEN -u GH_TOKEN -u GITHUB_TOKEN COPILOT_HOME='/tmp/acct one/copilot'"), "{cmd}");
            assert!(cmd.contains(" copilot login && env ") && cmd.ends_with(" gh auth login --web"), "{cmd}");
            let mut c = std::process::Command::new("true");
            c.env("GH_TOKEN", "leak");
            a.configure(dir, &mut c);
            let envs: Vec<_> = c.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
            assert!(envs.contains(&("GH_TOKEN".into(), None)));
            assert!(envs.contains(&("GH_CONFIG_DIR".into(), Some("/tmp/acct one/gh".into()))));
            let muse = AccountProvider::Named(NamedAccounts::Muse).sign_in_command(dir);
            assert!(muse.starts_with("env -u META_API_KEY XDG_CACHE_HOME='/tmp/acct one/cache'") && muse.ends_with(" muse login"), "{muse}");
        }

        let tmp = std::env::temp_dir().join(format!("vicuna-decl-acct-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("copilot")).unwrap();
        assert!(!a.signed_in(&tmp));
        let cfg = tmp.join("copilot/config.json");
        std::fs::write(&cfg, "// written by the CLI\n{ \"note\": \"http://x // not a comment\", /* c */ \"loggedInUsers\": [] }").unwrap();
        assert!(!a.signed_in(&tmp));
        std::fs::write(&cfg, "// written by the CLI\n{\n  \"loggedInUsers\": [ { \"login\": \"b\" } ] // tail\n}").unwrap();
        assert!(a.signed_in(&tmp));
        let muse = AccountProvider::Named(NamedAccounts::Muse);
        std::fs::create_dir_all(tmp.join("config/muse")).unwrap();
        std::fs::write(tmp.join("config/muse/auth.json"), "{\"providers\":{\"meta\":{}}}").unwrap();
        assert!(!muse.signed_in(&tmp));
        std::fs::write(tmp.join("config/muse/auth.json"), "{\"providers\":{\"meta\":{\"k\":1}}}").unwrap();
        assert!(muse.signed_in(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_capped_enforces_size_and_time() {
        let sh = std::path::Path::new("/bin/sh");
        if !sh.exists() {
            return;
        }
        fn test_command(sh: &Path, args: &[String]) -> std::process::Command {
            let mut cmd = std::process::Command::new(sh);
            cmd.args(args);
            cmd
        }
        let a = |s: &str| vec!["-c".to_string(), s.to_string()];
        assert_eq!(run_capped(test_command(sh, &a("printf '{\"a\":1}'")), sh, 1024, std::time::Duration::from_secs(5)).unwrap(), br#"{"a":1}"#);
        assert!(run_capped(test_command(sh, &a("head -c 5000 /dev/zero")), sh, 1024, std::time::Duration::from_secs(5)).unwrap_err().contains("larger"));
        let t = std::time::Instant::now();
        assert!(run_capped(test_command(sh, &a("sleep 30")), sh, 1024, std::time::Duration::from_millis(300)).unwrap_err().contains("longer"));
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }
}
