//! Telegram bridge: talk to any Work session from a phone, each with its own
//! bot token.
//!
//! One long-poll loop per configured & enabled session (`getUpdates`, no
//! webhook, so it works on a tailnet-only box). Each bot is public, so nothing
//! reaches its session until a Telegram user proves they can see Vicuna: the
//! app shows a pairing code and only the user who sends it is accepted (one
//! paired user per bot). Tokens live in a 0600 `~/vicuna/config/telegram.json`
//! and never go back to a window.
//!
//! Text/photos/files/voice from the paired user go to the bound Work session
//! (queued into its running turn, else a new server-started turn). Every turn
//! of that session, whoever started it (the phone, a window, a worker report),
//! is mirrored: a progress message edited while it runs, then the final reply.
//! Agent questions and permission cards become inline-keyboard buttons; a plain
//! text reply answers a free-form question.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use super::persist;
use super::turns::{self, live_turn};
use super::BridgeState;

const MAX_MSG: usize = 3900;

/// Placeholder key used while migrating a legacy single-bot `telegram.json`
/// before a lead session ID is resolved from the DB.
const LEGACY_LEAD_KEY: &str = "__legacy_lead__";

// ---------- config ----------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_username: String,
    /// the paired Telegram user (id) and the private chat to reply in
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub chat_id: Option<i64>,
    #[serde(default)]
    pub user_name: String,
    /// shown in the app until someone pairs
    #[serde(default)]
    pub pair_code: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    #[serde(default)]
    pub bridges: HashMap<String, BridgeConfig>,
}

fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VICUNA_TELEGRAM_CONFIG") {
        return Some(PathBuf::from(p));
    }
    Some(crate::opencode::home_dir()?.join("vicuna").join("config").join("telegram.json"))
}

pub fn load_store() -> Store {
    let Some(raw) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Store::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return Store::default();
    };
    if v.get("bridges").is_some() {
        return serde_json::from_value(v).unwrap_or_default();
    }
    // Legacy flat single-bot config: stage under LEGACY_LEAD_KEY until migrated.
    if let Ok(c) = serde_json::from_value::<BridgeConfig>(v) {
        if !c.token.is_empty() {
            let mut bridges = HashMap::new();
            bridges.insert(LEGACY_LEAD_KEY.to_string(), c);
            return Store { bridges };
        }
    }
    Store::default()
}

fn save_store(s: &Store) -> Result<(), String> {
    let p = config_path().ok_or("no home directory")?;
    if s.bridges.is_empty() {
        let _ = std::fs::remove_file(&p);
        bump_all();
        return Ok(());
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s).unwrap_or_default()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
    bump_all();
    Ok(())
}

/// If a legacy flat config was loaded under `LEGACY_LEAD_KEY`, attach it to
/// the current lead session in the DB so existing setups keep working.
async fn maybe_migrate_legacy(st: &BridgeState) {
    let mut s = load_store();
    if !s.bridges.contains_key(LEGACY_LEAD_KEY) {
        return;
    }
    let Some(lead) = lead_id(st).await else { return };
    if let Some(cfg) = s.bridges.remove(LEGACY_LEAD_KEY) {
        s.bridges.entry(lead.clone()).or_insert(cfg);
        let _ = save_store(&s);
        bump_session(&lead);
    }
}

fn new_pair_code() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

fn token_tail(t: &str) -> String {
    let n = t.chars().count();
    if n <= 4 { "••••".into() } else { format!("••••{}", t.chars().skip(n - 4).collect::<String>()) }
}

/// Telegram bot tokens look like `123456789:AA…` (35 chars after the colon).
pub fn valid_token_shape(t: &str) -> bool {
    match t.split_once(':') {
        Some((id, rest)) => {
            !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && rest.len() >= 30
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
        None => false,
    }
}

/// Is `text` a pairing attempt with `code` (`/start 123456`, `123456`)?
pub fn is_pair_attempt(text: &str, code: &str) -> bool {
    if code.is_empty() {
        return false;
    }
    let t = text.trim();
    let t = t.strip_prefix("/start").map(str::trim).unwrap_or(t);
    t == code
}

// ---------- runtime state ----------

#[derive(Default, Clone)]
struct BridgeRt {
    generation: u64,
    last_error: String,
    polling: bool,
    /// the model list last offered by /model for this bot: (engine, model ids)
    model_menu: Option<(String, Vec<String>)>,
}

#[derive(Default)]
struct Runtime {
    /// bumped on every config change so the supervisor reconciles pollers
    store_gen: u64,
    bridges: HashMap<String, BridgeRt>,
    /// turns being mirrored (turn id)
    following: HashSet<String>,
}

fn rt() -> &'static Mutex<Runtime> {
    static R: OnceLock<Mutex<Runtime>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn bump_all() {
    if let Ok(mut r) = rt().lock() {
        r.store_gen += 1;
    }
}

fn bump_session(sid: &str) {
    if let Ok(mut r) = rt().lock() {
        r.store_gen += 1;
        r.bridges.entry(sid.to_string()).or_default().generation += 1;
    }
}

fn set_error(sid: &str, e: impl Into<String>) {
    if let Ok(mut r) = rt().lock() {
        r.bridges.entry(sid.to_string()).or_default().last_error = e.into();
    }
}

fn set_polling(sid: &str, polling: bool, clear_err: bool) {
    if let Ok(mut r) = rt().lock() {
        let b = r.bridges.entry(sid.to_string()).or_default();
        b.polling = polling;
        if clear_err {
            b.last_error.clear();
        }
    }
}

/// Remove a deleted session's Telegram bridge and stop its poller.
pub fn drop_for(sid: &str) {
    let mut s = load_store();
    if s.bridges.remove(sid).is_some() {
        let _ = save_store(&s);
    }
    if let Ok(mut r) = rt().lock() {
        r.store_gen += 1;
        r.bridges.remove(sid);
    }
}

// ---------- Bot API ----------

fn api_base() -> String {
    std::env::var("VICUNA_TELEGRAM_API").unwrap_or_else(|_| "https://api.telegram.org".into())
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap_or_default())
}

async fn call(token: &str, method: &str, body: Value) -> Result<Value, String> {
    let url = format!("{}/bot{}/{}", api_base(), token, method);
    let resp = client().post(&url).json(&body).send().await.map_err(|e| {
        // never echo the URL: it carries the token
        format!("telegram {method}: {}", e.without_url())
    })?;
    let v: Value = resp.json().await.map_err(|e| format!("telegram {method}: {}", e.without_url()))?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(format!(
            "telegram {method}: {}",
            v.get("description").and_then(Value::as_str).unwrap_or("request failed")
        ))
    }
}

async fn send_text(token: &str, chat: i64, text: &str, keyboard: Option<Value>) -> Option<i64> {
    let mut body = json!({"chat_id": chat, "text": text, "disable_web_page_preview": true});
    if let Some(k) = keyboard {
        body["reply_markup"] = k;
    }
    call(token, "sendMessage", body).await.ok()?.get("message_id").and_then(Value::as_i64)
}

async fn edit_text(token: &str, chat: i64, msg: i64, text: &str) {
    let _ = call(token, "editMessageText", json!({"chat_id": chat, "message_id": msg, "text": text, "disable_web_page_preview": true})).await;
}

/// Split at line breaks into Telegram-sized pieces.
pub fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if cur.len() + line.len() > MAX_MSG && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line.len() > MAX_MSG {
            let mut s = line;
            while s.len() > MAX_MSG {
                let mut cut = MAX_MSG;
                while !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push(s[..cut].to_string());
                s = &s[cut..];
            }
            cur.push_str(s);
        } else {
            cur.push_str(line);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Reply text for the phone: tool cards become `▸ tool: summary` lines,
/// other harness HTML goes away.
pub fn phone_text(content: &str) -> String {
    let mut out = String::new();
    let mut rest = content;
    const DET: &str = "<details class=\"tool-result\">";
    while let Some(i) = rest.find(DET) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let summary = tail
            .find("<summary>")
            .and_then(|a| tail[a + 9..].find("</summary>").map(|b| tail[a + 9..a + 9 + b].to_string()))
            .unwrap_or_default();
        let summary = strip_tags(&summary);
        if !summary.trim().is_empty() {
            // re-escaped: the final strip_tags pass decodes entities once
            let line = tool_line(summary.trim()).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            out.push_str(&format!("\n▸ {line}\n"));
        }
        match tail.find("</details>") {
            Some(e) => rest = &tail[e + "</details>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let out = fold_tool_runs(&out);
    let out = persist::strip_harness_markup(&out);
    let out = strip_tags(&out);
    let mut s = String::new();
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s.trim().to_string()
}

/// `mcp__vicuna-mcp__manage_sessions — {"action":"archive","session_id":"1db0…"}`
/// → `manage_sessions: archive · 1db06ccf`. The JSON is often cut off, so
/// key/value pairs are scanned rather than parsed.
pub fn tool_line(summary: &str) -> String {
    let (name, detail) = match summary.split_once(" — ") {
        Some((n, d)) => (n.trim(), d.trim()),
        None => (summary.trim(), ""),
    };
    let name = match name.strip_prefix("mcp__") {
        Some(r) => r.split_once("__").map(|(_, t)| t).unwrap_or(r),
        None => name,
    };
    if !detail.starts_with('{') {
        return if detail.is_empty() { name.to_string() } else { format!("{name}: {}", clip(detail, 120)) };
    }
    let vals: Vec<String> = json_pairs(detail)
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "caller_session_id" | "description" | "timeout" | "timeout_ms"))
        .map(|(_, v)| clip(&short_ids(&v), 60))
        .filter(|v| !v.is_empty())
        .collect();
    if vals.is_empty() { name.to_string() } else { format!("{name}: {}", clip(&vals.join(" · "), 120)) }
}

/// (key, value) pairs of a possibly truncated flat JSON object; string
/// values unescaped, others kept as written.
fn json_pairs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 1;
    let read_str = |i: &mut usize| -> String {
        let mut v = String::new();
        *i += 1;
        while *i < c.len() && c[*i] != '"' {
            if c[*i] == '\\' && *i + 1 < c.len() {
                *i += 1;
                v.push(match c[*i] { 'n' => ' ', 't' => ' ', x => x });
            } else {
                v.push(c[*i]);
            }
            *i += 1;
        }
        *i += 1;
        v
    };
    while i < c.len() {
        while i < c.len() && c[i] != '"' {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let k = read_str(&mut i);
        while i < c.len() && (c[i] == ':' || c[i] == ' ') {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let v = if c[i] == '"' {
            read_str(&mut i)
        } else {
            // number/bool/nested: up to the next top-level comma
            let start = i;
            let mut depth = 0i32;
            while i < c.len() && !(depth == 0 && (c[i] == ',' || c[i] == '}')) {
                match c[i] { '{' | '[' => depth += 1, '}' | ']' => depth -= 1, _ => {} }
                i += 1;
            }
            c[start..i].iter().collect::<String>().trim().to_string()
        };
        out.push((k, v));
        while i < c.len() && c[i] != ',' {
            i += 1;
        }
        i += 1;
    }
    out
}

/// UUID-ish ids → their first 8 characters.
fn short_ids(v: &str) -> String {
    v.split(' ').map(|w| {
        let hexish = w.len() >= 13 && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && w.matches('-').count() >= 1;
        if hexish { w.chars().take(8).collect() } else { w.to_string() }
    }).collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

/// Consecutive `▸ tool: a` / `▸ tool: b` lines of one tool → `▸ tool ×2: a, b`.
pub fn fold_tool_runs(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(String, Vec<String>)> = None;
    let flush = |run: &mut Option<(String, Vec<String>)>, out: &mut Vec<String>| {
        if let Some((name, ds)) = run.take() {
            let ds: Vec<String> = ds.into_iter().filter(|d| !d.is_empty()).collect();
            let n = ds.len().max(1);
            let line = match (n, ds.is_empty()) {
                (_, true) => format!("▸ {name}"),
                (1, _) => format!("▸ {name}: {}", ds[0]),
                _ => {
                    // shared leading words (e.g. the action) said once
                    let first: Vec<&str> = ds[0].split(" · ").collect();
                    let common = (0..first.len())
                        .take_while(|&k| ds.iter().all(|d| d.split(" · ").nth(k) == Some(first[k])))
                        .count();
                    let head = first[..common].join(" · ");
                    let tails: Vec<String> = ds.iter().map(|d| d.split(" · ").skip(common).collect::<Vec<_>>().join(" · ")).filter(|t| !t.is_empty()).collect();
                    let body = match (head.is_empty(), tails.is_empty()) {
                        (true, _) => tails.join(", "),
                        (false, true) => head,
                        (false, false) => format!("{head} · {}", tails.join(", ")),
                    };
                    format!("▸ {name} ×{n}: {}", clip(&body, 300))
                }
            };
            out.push(line);
        }
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("▸ ") {
            let (name, d) = match rest.split_once(": ") {
                Some((n, d)) => (n.to_string(), d.to_string()),
                None => (rest.to_string(), String::new()),
            };
            match &mut run {
                Some((n, ds)) if *n == name => ds.push(d),
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((name, vec![d]));
                }
            }
        } else if line.trim().is_empty() && run.is_some() {
            continue; // blank lines between cards don't break a run
        } else {
            flush(&mut run, &mut out);
            out.push(line.to_string());
        }
    }
    flush(&mut run, &mut out);
    out.join("\n")
}

fn strip_tags(s: &str) -> String {
    remove_tags(s).replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// Tags out, entities kept (decoded once, by `strip_tags`).
fn remove_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let b = s.as_bytes();
    for (i, ch) in s.char_indices() {
        match ch {
            // only real tags (`<div`, `</span`), not `a < b`
            '<' if !in_tag && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'/') => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

// ---------- API actions ----------

async fn resolve_session_id(st: &BridgeState, args: &Value) -> Result<String, String> {
    if let Some(s) = args.get("sessionId").or_else(|| args.get("session_id")).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    lead_id(st).await.ok_or_else(|| "missing sessionId".into())
}

async fn session_title(st: &BridgeState, sid: &str) -> String {
    let Some(db) = st.db.as_ref() else { return "This session".into() };
    crate::server_store::load_chat(db, sid)
        .await
        .ok()
        .flatten()
        .and_then(|c| c.get("title").and_then(Value::as_str).map(str::to_string))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "This session".into())
}

pub async fn action(st: &Arc<BridgeState>, cmd: &str, args: &Value) -> Result<Value, String> {
    maybe_migrate_legacy(st).await;
    match cmd {
        "telegram_list" => Ok(status_all()),
        "telegram_status" => {
            if let Some(sid) = args.get("sessionId").or_else(|| args.get("session_id")).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                Ok(status_for(sid))
            } else if let Some(lead) = lead_id(st).await {
                Ok(status_for(&lead))
            } else {
                Ok(status_from_cfg("", None))
            }
        }
        "telegram_save" => {
            let sid = resolve_session_id(st, args).await?;
            let token = args.get("token").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if !valid_token_shape(&token) {
                return Err("That doesn't look like a bot token (123456789:AA…). Copy it from @BotFather.".into());
            }
            let mut s = load_store();
            // Reject duplicate tokens across sessions: Telegram only allows one poller per bot token.
            if let Some((other_sid, other_cfg)) = s.bridges.iter().find(|(k, v)| k.as_str() != sid && v.token == token) {
                let other_title = session_title(st, other_sid).await;
                let bot_label = if other_cfg.bot_username.is_empty() {
                    "This bot".to_string()
                } else {
                    format!("This bot (@{})", other_cfg.bot_username)
                };
                return Err(format!(
                    "{bot_label} is already connected to \"{other_title}\". Remove it there first or create a new bot with @BotFather."
                ));
            }
            let me = call(&token, "getMe", json!({})).await?;
            let c = s.bridges.entry(sid.clone()).or_default();
            if c.token != token {
                // a new bot: the old pairing belonged to the old bot's chat
                c.user_id = None;
                c.chat_id = None;
                c.user_name.clear();
            }
            c.token = token;
            c.enabled = true;
            c.bot_username = me.get("username").and_then(Value::as_str).unwrap_or("").to_string();
            if c.user_id.is_none() {
                c.pair_code = new_pair_code();
            }
            save_store(&s)?;
            bump_session(&sid);
            set_error(&sid, "");
            Ok(status_for(&sid))
        }
        "telegram_set" => {
            let sid = resolve_session_id(st, args).await?;
            let mut s = load_store();
            let c = s.bridges.get_mut(&sid).ok_or("no bot token saved for this session")?;
            if c.token.is_empty() {
                return Err("no bot token saved for this session".into());
            }
            if let Some(e) = args.get("enabled").and_then(Value::as_bool) {
                c.enabled = e;
            }
            save_store(&s)?;
            bump_session(&sid);
            Ok(status_for(&sid))
        }
        "telegram_unpair" => {
            let sid = resolve_session_id(st, args).await?;
            let mut s = load_store();
            let c = s.bridges.get_mut(&sid).ok_or("no bot token saved for this session")?;
            c.user_id = None;
            c.chat_id = None;
            c.user_name.clear();
            c.pair_code = new_pair_code();
            save_store(&s)?;
            bump_session(&sid);
            Ok(status_for(&sid))
        }
        "telegram_remove" => {
            let sid = resolve_session_id(st, args).await?;
            drop_for(&sid);
            Ok(status_for(&sid))
        }
        "whisper_status" => Ok(crate::whisper::status()),
        "whisper_install" => crate::whisper::start_install(args.get("model").and_then(Value::as_str).unwrap_or("tiny")),
        "whisper_uninstall" => crate::whisper::uninstall(args.get("model").and_then(Value::as_str).unwrap_or("tiny")),
        _ => Err(format!("unknown telegram action {cmd}")),
    }
}

fn status_from_cfg(sid: &str, c: Option<&BridgeConfig>) -> Value {
    let empty = BridgeConfig::default();
    let c = c.unwrap_or(&empty);
    let (err, polling) = rt()
        .lock()
        .ok()
        .and_then(|r| r.bridges.get(sid).map(|b| (b.last_error.clone(), b.polling)))
        .unwrap_or_default();
    json!({
        "sessionId": sid,
        "configured": !c.token.is_empty(),
        "enabled": c.enabled,
        "botUsername": c.bot_username,
        "tokenTail": if c.token.is_empty() { String::new() } else { token_tail(&c.token) },
        "paired": c.user_id.map(|id| json!({"id": id, "name": c.user_name})),
        "pairCode": if c.user_id.is_none() { c.pair_code.clone() } else { String::new() },
        "polling": polling && c.enabled && !c.token.is_empty(),
        "lastError": err,
    })
}

pub fn status_for(sid: &str) -> Value {
    let s = load_store();
    status_from_cfg(sid, s.bridges.get(sid))
}

pub fn status_all() -> Value {
    let s = load_store();
    let mut map = serde_json::Map::new();
    for (sid, cfg) in &s.bridges {
        if sid == LEGACY_LEAD_KEY || cfg.token.is_empty() {
            continue;
        }
        map.insert(sid.clone(), status_from_cfg(sid, Some(cfg)));
    }
    json!({ "bridges": map })
}

// ---------- the supervisor & per-session loops ----------

pub fn spawn(st: Arc<BridgeState>) {
    tokio::spawn(async move {
        let mut active: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        loop {
            maybe_migrate_legacy(&st).await;
            let s = load_store();
            let store_gen = rt().lock().map(|r| r.store_gen).unwrap_or(0);

            // Stop pollers for removed or disabled bridges
            let to_stop: Vec<String> = active
                .keys()
                .filter(|sid| {
                    s.bridges
                        .get(sid.as_str())
                        .map(|c| c.token.is_empty() || !c.enabled)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();
            for sid in to_stop {
                if let Some(h) = active.remove(&sid) {
                    h.abort();
                }
                set_polling(&sid, false, false);
            }

            // Spawn pollers for enabled bridges
            for (sid, cfg) in &s.bridges {
                if sid == LEGACY_LEAD_KEY || cfg.token.is_empty() || !cfg.enabled {
                    continue;
                }
                let needs_spawn = match active.get(sid) {
                    Some(h) => h.is_finished(),
                    None => true,
                };
                if needs_spawn {
                    let h = tokio::spawn(poll_bridge(st.clone(), sid.clone()));
                    active.insert(sid.clone(), h);
                }
            }

            // Wait for a config change or periodic check
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if rt().lock().map(|r| r.store_gen).unwrap_or(0) != store_gen {
                    break;
                }
            }
        }
    });
}

async fn poll_bridge(st: Arc<BridgeState>, sid: String) {
    let mut offset: i64 = 0;
    let mut last_token = String::new();
    loop {
        let Some(c) = load_store().bridges.remove(&sid) else {
            set_polling(&sid, false, false);
            return;
        };
        if c.token.is_empty() || !c.enabled {
            set_polling(&sid, false, false);
            return;
        }
        if c.token != last_token {
            offset = 0;
            last_token = c.token.clone();
            register_commands(&sid, &c.token).await;
        }
        // Mirror turns nobody started from Telegram (window, worker report, schedule)
        if let Some(chat) = c.chat_id {
            watch_session(&st, &sid, &c.token, chat).await;
        }
        let generation = rt().lock().ok().and_then(|r| r.bridges.get(&sid).map(|b| b.generation)).unwrap_or(0);
        let req = call(&c.token, "getUpdates", json!({"offset": offset, "timeout": 20, "allowed_updates": ["message", "callback_query"]}));
        // A config change (new token, disable, remove) cancels the long poll
        let changed = async {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let cur_gen = rt().lock().ok().and_then(|r| r.bridges.get(&sid).map(|b| b.generation)).unwrap_or(0);
                if cur_gen != generation {
                    break;
                }
                if c.chat_id.is_some() && session_has_unfollowed_turn(&st, &sid) {
                    break;
                }
            }
        };
        let res = tokio::select! {
            r = req => Some(r),
            _ = changed => None,
        };
        let Some(res) = res else { continue };
        match res {
            Ok(updates) => {
                set_polling(&sid, true, true);
                for u in updates.as_array().cloned().unwrap_or_default() {
                    if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                        offset = offset.max(id + 1);
                    }
                    if let Err(e) = handle_update(&st, &sid, &u).await {
                        set_error(&sid, e);
                    }
                }
            }
            Err(e) => {
                set_polling(&sid, false, false);
                set_error(&sid, e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn lead_id(st: &BridgeState) -> Option<String> {
    use sqlx::Row;
    let db = st.db.as_ref()?;
    let rows = sqlx::query("SELECT id, extra FROM chats WHERE extra LIKE '%\"role\"%' ORDER BY updated_at DESC")
        .fetch_all(db).await.ok()?;
    rows.iter().find_map(|r| {
        let extra: Option<String> = r.try_get("extra").ok();
        let chat = json!({"extra": extra});
        turns::parse_extra(&chat).lead.then(|| r.try_get::<String, _>("id").ok()).flatten()
    })
}

fn session_has_unfollowed_turn(st: &BridgeState, sid: &str) -> bool {
    match live_turn(st, sid) {
        Some((tid, _)) => !rt().lock().map(|r| r.following.contains(&tid)).unwrap_or(true),
        None => false,
    }
}

async fn watch_session(st: &Arc<BridgeState>, sid: &str, token: &str, chat: i64) {
    if let Some((tid, _)) = live_turn(st, sid) {
        follow(st.clone(), token.to_string(), chat, sid.to_string(), tid);
    }
}

async fn handle_update(st: &Arc<BridgeState>, sid: &str, u: &Value) -> Result<(), String> {
    let mut s = load_store();
    let Some(c) = s.bridges.get_mut(sid) else { return Ok(()) };
    let token = c.token.clone();
    if let Some(cb) = u.get("callback_query") {
        let from = cb.pointer("/from/id").and_then(Value::as_i64);
        let _ = call(&token, "answerCallbackQuery", json!({"callback_query_id": cb.get("id")})).await;
        if from.is_none() || from != c.user_id {
            return Ok(());
        }
        let data = cb.get("data").and_then(Value::as_str).unwrap_or("");
        let chat = c.chat_id.unwrap_or_default();
        let msg_id = cb.pointer("/message/message_id").and_then(Value::as_i64);
        if data == "sn" {
            if let (Some(m), Some(t)) = (msg_id, cb.pointer("/message/text").and_then(Value::as_str)) {
                // drop the button so it can't be tapped twice
                let _ = call(&token, "editMessageText", json!({"chat_id": chat, "message_id": m, "text": t})).await;
            }
            send_now_reply(st, sid, &token, chat).await;
            return Ok(());
        }
        if let Some(e) = data.strip_prefix("e:") {
            let out = pick_engine(st, sid, e).await;
            reply_or_edit(&token, chat, msg_id, out).await;
            return Ok(());
        }
        if let Some(i) = data.strip_prefix("m:") {
            let out = pick_model(st, sid, i.parse().unwrap_or(usize::MAX)).await;
            reply_or_edit(&token, chat, msg_id, (out, None)).await;
            return Ok(());
        }
        if let Some((ask_sid, idx)) = data.strip_prefix("a:").and_then(|d| d.rsplit_once(':')) {
            let idx: usize = idx.parse().unwrap_or(usize::MAX);
            match answer_ask(st, ask_sid, Answer::Choice(idx)).await {
                Ok(shown) => {
                    if let (Some(m), Some(t)) = (cb.pointer("/message/message_id").and_then(Value::as_i64), cb.pointer("/message/text").and_then(Value::as_str)) {
                        edit_text(&token, chat, m, &format!("{t}\n\n→ {shown}")).await;
                    }
                }
                Err(e) => {
                    send_text(&token, chat, &format!("⚠️ {e}"), None).await;
                }
            }
        }
        return Ok(());
    }
    let Some(msg) = u.get("message") else { return Ok(()) };
    let from = msg.pointer("/from/id").and_then(Value::as_i64);
    let chat = msg.pointer("/chat/id").and_then(Value::as_i64).unwrap_or_default();
    let private = msg.pointer("/chat/type").and_then(Value::as_str) == Some("private");
    let text = msg.get("text").or_else(|| msg.get("caption")).and_then(Value::as_str).unwrap_or("").to_string();

    if c.user_id.is_none() {
        if private && is_pair_attempt(&text, &c.pair_code) {
            let f = msg.get("from").cloned().unwrap_or(Value::Null);
            let name = f.get("username").and_then(Value::as_str).map(|u| format!("@{u}"))
                .or_else(|| f.get("first_name").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| "Telegram user".into());
            c.user_id = from;
            c.chat_id = Some(chat);
            c.user_name = name;
            c.pair_code.clear();
            save_store(&s)?;
            bump_session(sid);
            let title = session_title(st, sid).await;
            send_text(&token, chat, &format!("✅ Paired with Vicuna. Messages you send here go to \"{title}\".\n\n/status · /model · /send_now · /stop · /help"), None).await;
        } else if private {
            send_text(&token, chat, "This bot is private. Open Vicuna → right-click the session → Telegram, and send the pairing code shown there.", None).await;
        }
        return Ok(());
    }
    if from != c.user_id {
        return Ok(()); // strangers are ignored once paired
    }
    let t = text.trim();
    let title = session_title(st, sid).await;
    if t == "/help" || t == "/start" {
        send_text(&token, chat, &format!("Send any message to talk to \"{title}\". Photos, files and voice memos work too.\n/status: what this session is doing\n/model: switch harness and model (/model claude haiku)\n/send_now: interrupt the running turn and send your queued messages now\n/stop: stop the running turn"), None).await;
        return Ok(());
    }
    // Verify the bound session still exists
    let Some(db) = st.db.as_ref() else {
        return Err("no server db".into());
    };
    let Some(ch) = crate::server_store::load_chat(db, sid).await? else {
        send_text(&token, chat, "⚠️ The session bound to this bot no longer exists in Vicuna.", None).await;
        return Ok(());
    };
    if t == "/model" || t.starts_with("/model ") || t.starts_with("/model@") {
        let rest: Vec<&str> = t.split_whitespace().skip(1).collect();
        let (text, kb) = match rest.as_slice() {
            [] => model_overview(st, sid, &ch).await,
            [e] => pick_engine(st, sid, e).await,
            [e, m, ..] => match set_session_model(st, sid, e, m).await {
                Ok(s) => (s, None),
                Err(e) => (format!("⚠️ {e}"), None),
            },
        };
        send_text(&token, chat, &text, kb).await;
        return Ok(());
    }
    if t == "/status" {
        let state = if live_turn(st, sid).is_some() { "working" } else { "idle" };
        let (engine, model) = session_engine_model(&ch);
        send_text(&token, chat, &format!("\"{title}\" is {state} · {engine}/{model}"), None).await;
        return Ok(());
    }
    if t == "/send_now" || t.starts_with("/send_now@") {
        send_now_reply(st, sid, &token, chat).await;
        return Ok(());
    }
    if t == "/stop" {
        match live_turn(st, sid) {
            Some((tid, _)) => {
                if let Some(t) = st.agent.get_turn(&tid) { crate::work_agent::agent_stop(&t); }
                send_text(&token, chat, &format!("⏹ Stopping \"{title}\"…"), None).await;
            }
            None => {
                send_text(&token, chat, &format!("\"{title}\" is idle."), None).await;
            }
        }
        return Ok(());
    }
    // a text reply answers a waiting free-form question
    if !t.is_empty() && pending_ask_for(st, sid).is_some() {
        match answer_ask(st, sid, Answer::Text(t.to_string())).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                send_text(&token, chat, &format!("⚠️ {e}"), None).await;
                return Ok(());
            }
        }
    }
    let mut text_out = t.to_string();
    // voice memos: transcribe on the server (if a Whisper model is installed),
    // show the transcript on the phone first, then send it to the session as text
    if let Some(v) = msg.get("voice") {
        let fid = v.get("file_id").and_then(Value::as_str).unwrap_or("").to_string();
        if crate::whisper::installed_model().is_some() {
            let outcome = voice_to_text(&token, chat, &fid, |bytes, tx| async move {
                crate::whisper::transcribe_ogg_partial(bytes, move |p| {
                    let _ = tx.send(p.to_string());
                })
                .await
            })
            .await;
            match outcome {
                VoiceOutcome::Text(tx) => {
                    text_out = if text_out.is_empty() { tx } else { format!("{text_out}\n\n{tx}") };
                }
                VoiceOutcome::Empty => return Ok(()),
                VoiceOutcome::Failed => {} // the audio file goes to the session below
            }
            if !text_out.is_empty() && text_out != t {
                // transcript ready: a waiting free-form question takes it as the answer
                if pending_ask_for(st, sid).is_some() {
                    if let Err(e) = answer_ask(st, sid, Answer::Text(text_out.clone())).await {
                        send_text(&token, chat, &format!("⚠️ {e}"), None).await;
                    }
                    return Ok(());
                }
            }
        } else {
            send_text(&token, chat, "🎙 Got your voice memo. No transcription model is installed on the server, so it goes to this session as an audio file.", None).await;
        }
    }
    let transcribed = text_out != t;
    let mut images = Vec::new();
    let mut staged: Vec<String> = Vec::new();
    for (kind, name) in attachment_files(msg, transcribed) {
        let fid = kind;
        match download_file(&token, &fid, MAX_FILE).await {
            Ok(bytes) if is_image_name(&name) && bytes.len() <= 10 * 1024 * 1024 => {
                images.push(json!(format!("data:{};base64,{}", image_mime(&name), base64::engine::general_purpose::STANDARD.encode(&bytes))));
            }
            Ok(bytes) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                match crate::upload_stage_file_logic(sid.to_string(), name.clone(), b64).await {
                    Ok(p) => staged.push(p),
                    Err(e) => {
                        send_text(&token, chat, &format!("⚠️ Couldn't save {name}: {e}"), None).await;
                    }
                }
            }
            Err(e) => {
                send_text(&token, chat, &format!("⚠️ Couldn't fetch {name}: {e}"), None).await;
            }
        }
    }
    if !staged.is_empty() {
        text_out.push_str(&upload_note(&staged));
    }
    let t = text_out.trim().to_string();
    let t = t.as_str();
    if let Some(photos) = msg.get("photo").and_then(Value::as_array) {
        if let Some(fid) = photos.last().and_then(|p| p.get("file_id")).and_then(Value::as_str) {
            match download_file(&token, fid, 10 * 1024 * 1024).await {
                Ok(b) => images.push(json!(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&b)))),
                Err(e) => {
                    send_text(&token, chat, &format!("⚠️ Couldn't fetch the photo: {e}"), None).await;
                }
            }
        }
    }
    if t.is_empty() && images.is_empty() {
        return Ok(());
    }
    let mut m = json!({"role": "user", "content": t});
    if !images.is_empty() {
        m["images"] = json!(images);
    }
    let queued = live_turn(st, sid).is_some();
    if let Err(e) = turns::deliver(st, sid, m).await {
        send_text(&token, chat, &format!("⚠️ {e}"), None).await;
        return Ok(());
    }
    if queued {
        let kb = json!({"inline_keyboard": [[{"text": "⚡ Send now", "callback_data": "sn"}]]});
        send_text(&token, chat, "📨 Queued for the running turn. Tap Send now (or /send_now) to interrupt it and send right away.", Some(kb)).await;
    }
    if let Some((tid, _)) = live_turn(st, sid) {
        follow(st.clone(), token, chat, sid.to_string(), tid);
    }
    Ok(())
}

async fn send_now_reply(st: &Arc<BridgeState>, sid: &str, token: &str, chat: i64) {
    if live_turn(st, sid).is_none() {
        send_text(token, chat, "This session is idle, nothing is waiting.", None).await;
        return;
    }
    send_text(token, chat, "⚡ Interrupting the running turn…", None).await;
    // the stop + restart can take a while: don't hold up the update loop
    let (st, sid, token) = (st.clone(), sid.to_string(), token.to_string());
    tokio::spawn(async move {
        match turns::send_now(&st, &sid).await {
            Ok(0) => {
                send_text(&token, chat, "Nothing is queued: your messages already reached the session.", None).await;
            }
            Ok(_) => {
                if let Some((tid, _)) = live_turn(&st, &sid) {
                    follow(st.clone(), token, chat, sid, tid);
                }
            }
            Err(e) => {
                send_text(&token, chat, &format!("⚠️ {e}"), None).await;
            }
        }
    });
}

// ---------- /model ----------

const COMMANDS: &[(&str, &str)] = &[
    ("status", "What this session is doing, harness and model"),
    ("model", "Switch this session's harness and model"),
    ("send_now", "Interrupt the running turn and send queued messages now"),
    ("stop", "Stop the running turn"),
    ("help", "How this bot works"),
];

async fn register_commands(sid: &str, token: &str) {
    let cmds: Vec<Value> = COMMANDS.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
    if let Err(e) = call(token, "setMyCommands", json!({"commands": cmds})).await {
        set_error(sid, e);
    }
}

async fn reply_or_edit(token: &str, chat: i64, msg: Option<i64>, (text, kb): (String, Option<Value>)) {
    match msg {
        Some(m) => {
            let mut body = json!({"chat_id": chat, "message_id": m, "text": text});
            if let Some(k) = kb {
                body["reply_markup"] = k;
            }
            let _ = call(token, "editMessageText", body).await;
        }
        None => {
            send_text(token, chat, &text, kb).await;
        }
    }
}

/// (harness, model) a session runs with: its own pick, else the server's.
fn session_engine_model(chat: &Value) -> (String, String) {
    let x = turns::parse_extra(chat);
    let Ok((engine, model)) = crate::work_engine::session_engine(x.engine.as_deref(), x.work_model.as_deref()) else {
        return (format!("{} (not installed)", x.engine.unwrap_or_default()), "default".into());
    };
    (engine.id().to_string(), model.filter(|m| !m.is_empty()).unwrap_or_else(|| "default".into()))
}

async fn model_overview(_st: &Arc<BridgeState>, _sid: &str, ch: &Value) -> (String, Option<Value>) {
    let (engine, model) = session_engine_model(ch);
    let info = serde_json::to_value(crate::work_engine::engines_info().await).unwrap_or(Value::Null);
    let rows: Vec<Value> = info["engines"].as_array().into_iter().flatten()
        .filter(|e| e["installed"] == json!(true))
        .map(|e| {
            let id = e["id"].as_str().unwrap_or("");
            let mark = if id == engine { "✓ " } else { "" };
            json!([{"text": format!("{mark}{}", e["label"].as_str().unwrap_or(id)), "callback_data": format!("e:{id}")}])
        })
        .collect();
    (format!("This session runs {engine}/{model}.\nPick a harness:"), (!rows.is_empty()).then(|| json!({"inline_keyboard": rows})))
}

async fn model_ids(st: &Arc<BridgeState>, engine: crate::work_engine::Engine) -> Result<Vec<String>, String> {
    let v = serde_json::to_value(crate::work_engine::list_models(engine, &st.acp).await?).unwrap_or(Value::Null);
    let list = v.get("models").and_then(Value::as_array).or_else(|| v.as_array()).cloned().unwrap_or_default();
    Ok(list.iter().filter_map(|m| match m {
        Value::String(s) => Some(s.clone()),
        o => o.get("id").or_else(|| o.get("value")).and_then(Value::as_str).map(str::to_string),
    }).filter(|s| !s.is_empty()).collect())
}

/// Offer the models of a harness as buttons.
async fn pick_engine(st: &Arc<BridgeState>, sid: &str, e: &str) -> (String, Option<Value>) {
    let Some(engine) = crate::work_engine::Engine::parse(e) else { return (format!("⚠️ Unknown harness \"{e}\"."), None) };
    if !engine.is_present() {
        return (format!("⚠️ {}", engine.missing_message()), None);
    }
    let ids = match model_ids(st, engine).await {
        Ok(v) => v,
        Err(err) => return (format!("⚠️ Couldn't list {} models: {err}", engine.id()), None),
    };
    let shown: Vec<String> = ids.into_iter().take(40).collect();
    let mut rows: Vec<Value> = vec![json!([{"text": "Default", "callback_data": "m:0"}])];
    for (i, m) in shown.iter().enumerate() {
        rows.push(json!([{"text": m.chars().take(60).collect::<String>(), "callback_data": format!("m:{}", i + 1)}]));
    }
    let mut ids = vec!["default".to_string()];
    ids.extend(shown);
    if let Ok(mut r) = rt().lock() {
        r.bridges.entry(sid.to_string()).or_default().model_menu = Some((engine.id().to_string(), ids));
    }
    (format!("{} models:", engine.id()), Some(json!({"inline_keyboard": rows})))
}

async fn pick_model(st: &Arc<BridgeState>, sid: &str, i: usize) -> String {
    let pick = rt().lock().ok().and_then(|r| r.bridges.get(sid).and_then(|b| b.model_menu.clone()));
    let Some((engine, ids)) = pick else { return "⚠️ That menu expired, send /model again.".into() };
    let Some(model) = ids.get(i).cloned() else { return "⚠️ Unknown model.".into() };
    match set_session_model(st, sid, &engine, &model).await {
        Ok(s) => s,
        Err(e) => format!("⚠️ {e}"),
    }
}

async fn set_session_model(st: &Arc<BridgeState>, sid: &str, engine: &str, model: &str) -> Result<String, String> {
    turns::cli_session(st, "update", &json!({"id": sid, "engine": engine, "model": model})).await?;
    let note = if live_turn(st, sid).is_some() { " (from the next turn; one is running now)" } else { "" };
    Ok(format!("✅ Now runs {engine}/{model}{note}."))
}

/// Minimum gap between edits of the voice placeholder (Telegram rate limits).
const PARTIAL_EDIT_EVERY: Duration = Duration::from_millis(1500);

#[derive(Debug, PartialEq)]
pub enum VoiceOutcome {
    Text(String),
    /// no speech: nothing goes to the session
    Empty,
    /// download/transcription failed: send the audio file instead
    Failed,
}

/// Acknowledge a voice memo at once, keep "typing…" going while it is
/// transcribed, stream partial text into the placeholder, then edit it into
/// the transcript (or a clear error). `transcribe` gets the audio bytes and
/// a sender for partial text.
pub async fn voice_to_text<F, Fut>(token: &str, chat: i64, file_id: &str, transcribe: F) -> VoiceOutcome
where
    F: FnOnce(Vec<u8>, tokio::sync::mpsc::UnboundedSender<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, String>>,
{
    let placeholder = send_text(token, chat, "🎙 Got it, transcribing…", None).await;
    let show = |text: String| async move {
        match placeholder {
            Some(m) => edit_text(token, chat, m, &text).await,
            None => {
                send_text(token, chat, &text, None).await;
            }
        }
    };
    let typing = {
        let (token, stop) = (token.to_string(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let s2 = stop.clone();
        tokio::spawn(async move {
            // a chat action lasts ~5 s: refresh it until the transcript is in
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = call(&token, "sendChatAction", json!({"chat_id": chat, "action": "typing"})).await;
                for _ in 0..8 {
                    if s2.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });
        stop
    };
    let res = match download_file(token, file_id, MAX_FILE).await {
        Err(e) => Err(e),
        Ok(bytes) => {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let fut = transcribe(bytes, tx);
            tokio::pin!(fut);
            let mut last = std::time::Instant::now();
            let mut open = true;
            loop {
                tokio::select! {
                    r = &mut fut => break r,
                    p = rx.recv(), if open => match p {
                        Some(p) if placeholder.is_some() && last.elapsed() >= PARTIAL_EDIT_EVERY && !p.trim().is_empty() => {
                            edit_text(token, chat, placeholder.unwrap_or_default(), &format!("🎙 {}…", p.trim())).await;
                            last = std::time::Instant::now();
                        }
                        Some(_) => {}
                        None => open = false,
                    },
                }
            }
        }
    };
    typing.store(true, std::sync::atomic::Ordering::Relaxed);
    match res {
        Ok(Some(tx)) if !tx.trim().is_empty() => {
            show(format!("🎙 {}", tx.trim())).await;
            VoiceOutcome::Text(tx.trim().to_string())
        }
        Ok(_) => {
            show("🎙 No speech recognized in that voice memo.".into()).await;
            VoiceOutcome::Empty
        }
        Err(e) => {
            show(format!("⚠️ Couldn't transcribe the voice memo ({e}). Sending the audio file instead.")).await;
            VoiceOutcome::Failed
        }
    }
}

/// Telegram's getFile serves files up to 20 MB to bots.
const MAX_FILE: usize = 20 * 1024 * 1024;

async fn download_file(token: &str, file_id: &str, cap: usize) -> Result<Vec<u8>, String> {
    if file_id.is_empty() {
        return Err("no file id".into());
    }
    let f = call(token, "getFile", json!({"file_id": file_id})).await?;
    if f.get("file_size").and_then(Value::as_u64).is_some_and(|n| n as usize > cap) {
        return Err(format!("file is over {} MB", cap / 1024 / 1024));
    }
    let path = f.get("file_path").and_then(Value::as_str).ok_or("file is too big for a bot to download (20 MB limit)")?;
    let url = format!("{}/file/bot{}/{}", api_base(), token, path);
    let bytes = client().get(&url).send().await.map_err(|e| e.without_url().to_string())?
        .bytes().await.map_err(|e| e.without_url().to_string())?;
    if bytes.len() > cap {
        return Err(format!("file is over {} MB", cap / 1024 / 1024));
    }
    Ok(bytes.to_vec())
}

/// (file_id, file name) of every non-photo attachment in a message. A voice
/// memo that was already transcribed is left out.
pub fn attachment_files(msg: &Value, voice_transcribed: bool) -> Vec<(String, String)> {
    let uid = |v: &Value| v.get("file_unique_id").and_then(Value::as_str).unwrap_or("file").to_string();
    let mut out = Vec::new();
    for (key, default_ext) in [("document", ""), ("audio", "mp3"), ("video", "mp4"), ("video_note", "mp4"), ("voice", "ogg"), ("animation", "mp4")] {
        if key == "voice" && voice_transcribed {
            continue;
        }
        let Some(v) = msg.get(key) else { continue };
        let Some(fid) = v.get("file_id").and_then(Value::as_str) else { continue };
        let name = v.get("file_name").and_then(Value::as_str).map(str::to_string)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| {
                let ext = v.get("mime_type").and_then(Value::as_str)
                    .and_then(|m| m.rsplit('/').next())
                    .map(|e| if e == "mpeg" { "mp3" } else { e })
                    .filter(|_| default_ext.is_empty())
                    .unwrap_or(if default_ext.is_empty() { "bin" } else { default_ext });
                format!("{key}-{}.{ext}", uid(v))
            });
        out.push((fid.to_string(), sanitize_name(&name)));
    }
    out
}

fn sanitize_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or(n).trim();
    let s: String = base.chars().map(|c| if c.is_control() { '_' } else { c }).collect();
    if s.is_empty() || s == "." || s == ".." { "file".into() } else { s.chars().take(200).collect() }
}

fn is_image_name(n: &str) -> bool {
    let l = n.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif"].iter().any(|e| l.ends_with(e))
}

fn image_mime(n: &str) -> &'static str {
    let l = n.to_ascii_lowercase();
    if l.ends_with(".png") { "image/png" } else if l.ends_with(".webp") { "image/webp" } else if l.ends_with(".gif") { "image/gif" } else { "image/jpeg" }
}

/// Same note the composer appends for attached files (agents read them from there).
pub fn upload_note(paths: &[String]) -> String {
    let dir = std::path::Path::new(&paths[0]).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let names: Vec<String> = paths.iter()
        .map(|p| format!(" - {}", std::path::Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()))
        .collect();
    format!("\n\n[Attached files saved to {dir} (use the read_upload tool with the file NAME to read them):\n{}]", names.join("\n"))
}

// ---------- ask cards ----------

enum Answer {
    Choice(usize),
    Text(String),
}

fn recorder_of(st: &BridgeState, session: &str) -> Option<Arc<Mutex<turns::RecState>>> {
    let recs: Vec<_> = st.recorders.lock().ok()?.values().cloned().collect();
    recs.into_iter().find(|r| r.lock().map(|g| g.rec.chat_id == session && !g.finishing).unwrap_or(false))
}

fn pending_ask_for(st: &BridgeState, session: &str) -> Option<Value> {
    recorder_of(st, session)?.lock().ok()?.rec.pending_ask.clone()
}

/// Live reply text + waiting ask card of a session's running turn.
fn live_view(st: &BridgeState, session: &str) -> Option<(String, Option<Value>)> {
    let r = recorder_of(st, session)?;
    let g = r.lock().ok()?;
    let text = g.rec.messages.get(g.rec.cur).and_then(|m| m.get("content")).and_then(Value::as_str).unwrap_or("").to_string();
    Some((text, g.rec.pending_ask.clone()))
}

async fn answer_ask(st: &Arc<BridgeState>, session: &str, a: Answer) -> Result<String, String> {
    let ask = pending_ask_for(st, session).ok_or("That question was already answered.")?;
    let ask_id = ask.get("ask_id").and_then(Value::as_str).unwrap_or("").to_string();
    let options: Vec<String> = ask.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let multi = ask.get("multi").and_then(Value::as_bool).unwrap_or(false);
    let text = match a {
        Answer::Choice(i) => options.get(i).cloned().ok_or("unknown choice")?,
        Answer::Text(t) => t,
    };
    let (tid, turn) = live_turn(st, session).ok_or("That turn already ended.")?;
    let wire = if multi { json!([text]).to_string() } else { text.clone() };
    turns::record_answer(st, &turn, &tid, &ask_id, wire, Some(&text), false)?;
    Ok(text)
}

fn ask_keyboard(session: &str, options: &[String]) -> Option<Value> {
    if options.is_empty() {
        return None;
    }
    let rows: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(i, o)| json!([{"text": o.chars().take(60).collect::<String>(), "callback_data": format!("a:{session}:{i}")}]))
        .collect();
    Some(json!({"inline_keyboard": rows}))
}

// ---------- mirroring a session turn ----------

fn follow(st: Arc<BridgeState>, token: String, chat: i64, session: String, turn_id: String) {
    {
        let Ok(mut r) = rt().lock() else { return };
        if !r.following.insert(turn_id.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let progress = send_text(&token, chat, "⏳ Working…", None).await;
        let mut shown = String::new();
        let mut last_edit = std::time::Instant::now();
        let mut asked: Option<String> = None;
        loop {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let live = live_turn(&st, &session).map(|(t, _)| t);
            if live.as_deref() != Some(turn_id.as_str()) {
                break;
            }
            if let Some((text, ask)) = live_view(&st, &session) {
                if let Some(a) = ask {
                    let id = a.get("ask_id").and_then(Value::as_str).unwrap_or("").to_string();
                    if asked.as_deref() != Some(id.as_str()) {
                        let q = a.get("question").and_then(Value::as_str).unwrap_or("This session has a question");
                        let opts: Vec<String> = a.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                        let hint = if id.starts_with("ask-") { "\n\n(Tap a choice or reply with text.)" } else { "" };
                        send_text(&token, chat, &format!("❓ {q}{hint}"), ask_keyboard(&session, &opts)).await;
                        asked = Some(id);
                    }
                }
                let view = phone_text(&text);
                if let Some(m) = progress {
                    if view != shown && last_edit.elapsed() >= Duration::from_secs(3) && !view.is_empty() {
                        let tail: String = {
                            let n = view.chars().count();
                            view.chars().skip(n.saturating_sub(MAX_MSG - 40)).collect()
                        };
                        edit_text(&token, chat, m, &format!("⏳ {tail}")).await;
                        shown = view;
                        last_edit = std::time::Instant::now();
                    }
                }
            }
        }
        // the final write lands after `done`; give it a moment
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let reply = match st.db.as_ref() {
            Some(db) => crate::server_store::load_chat(db, &session).await.ok().flatten(),
            None => None,
        }
        .and_then(|c| {
            c.get("messages")?.as_array()?.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("assistant")).cloned()
        })
        .map(|m| phone_text(m.get("content").and_then(Value::as_str).unwrap_or("")))
        .unwrap_or_default();
        let reply = if reply.is_empty() { "(no reply text)".to_string() } else { reply };
        let parts = chunks(&reply);
        let mut it = parts.iter();
        if let (Some(m), Some(first)) = (progress, it.next()) {
            edit_text(&token, chat, m, first).await;
        } else if let Some(first) = parts.first() {
            send_text(&token, chat, first, None).await;
        }
        for p in it {
            send_text(&token, chat, p, None).await;
        }
        if let Ok(mut r) = rt().lock() {
            r.following.remove(&turn_id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape_and_pairing() {
        assert!(valid_token_shape("123456789:AAHxhuW3JCGIl5uO99DJS5S1e8zcTWkJkdo"));
        assert!(!valid_token_shape("nope"));
        assert!(!valid_token_shape("12a:AAHxhuW3JCGIl5uO99DJS5S1e8zcTWkJkdo"));
        assert!(is_pair_attempt("/start 123456", "123456"));
        assert!(is_pair_attempt(" 123456 ", "123456"));
        assert!(!is_pair_attempt("/start 12345", "123456"));
        assert!(!is_pair_attempt("", ""));
        assert_eq!(token_tail("123:abcdefgh"), "••••efgh");
    }

    #[test]
    fn phone_text_flattens_tool_cards() {
        let c = "Looking.\n\n<details class=\"tool-result\"><summary>Bash — ls &lt;dir&gt;</summary><pre><code>a\nb</code></pre></details>\n\nDone: 2 files.";
        let t = phone_text(c);
        assert!(t.contains("▸ Bash: ls <dir>"), "{t}");
        assert!(t.contains("Done: 2 files."));
        assert!(!t.contains("<pre>") && !t.contains("a\nb"));
        assert_eq!(phone_text("a < b"), "a < b");
    }

    #[test]
    fn attachments_name_files_and_skip_transcribed_voice() {
        let msg = json!({
            "document": {"file_id": "D", "file_unique_id": "u1", "file_name": "../report.pdf"},
            "voice": {"file_id": "V", "file_unique_id": "u2", "mime_type": "audio/ogg"},
            "audio": {"file_id": "A", "file_unique_id": "u3", "mime_type": "audio/mpeg"},
        });
        let all = attachment_files(&msg, false);
        assert_eq!(all, vec![("D".into(), "report.pdf".into()), ("A".into(), "audio-u3.mp3".into()), ("V".into(), "voice-u2.ogg".into())]);
        assert!(attachment_files(&msg, true).iter().all(|(id, _)| id != "V"));
        let note = upload_note(&["/h/vicuna/uploads/s/report.pdf".into()]);
        assert!(note.contains("[Attached files saved to /h/vicuna/uploads/s (use the read_upload tool") && note.contains(" - report.pdf]"));
    }

    #[test]
    fn tool_lines_are_compact_and_folded() {
        let a = r#"mcp__vicuna-mcp__manage_sessions — {"action":"archive","caller_session_id":"c19ce8f0-02c5-49e6-9744-10bf9d321fa5","session_id":"1db06ccf-38a9-4c64-9b35-13d"#;
        assert_eq!(tool_line(a), "manage_sessions: archive · 1db06ccf");
        assert_eq!(tool_line("Bash — ls -la"), "Bash: ls -la");
        let b = a.replace("1db06ccf", "fbf7a937");
        let t = fold_tool_runs(&format!("Archiving.\n▸ {}\n\n▸ {}\nDone.", tool_line(a), tool_line(&b)));
        assert_eq!(t, "Archiving.\n▸ manage_sessions ×2: archive · 1db06ccf, fbf7a937\nDone.");
    }

    #[test]
    fn legacy_flat_config_loads_for_migration_and_multi_session_store_roundtrips() {
        let dir = std::env::temp_dir().join(format!("vicuna-tg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg_path = dir.join("telegram.json");
        unsafe { std::env::set_var("VICUNA_TELEGRAM_CONFIG", &cfg_path) };

        // 1. Legacy single-bot flat JSON loads under LEGACY_LEAD_KEY
        std::fs::write(&cfg_path, r#"{"token":"111:AAHxhuW3JCGIl5uO99DJS5S1e8zcTWkJkdo","enabled":true,"botUsername":"old_bot","userId":7,"chatId":7,"userName":"@me"}"#).unwrap();
        let s = load_store();
        assert_eq!(s.bridges.len(), 1);
        assert_eq!(s.bridges[LEGACY_LEAD_KEY].bot_username, "old_bot");

        // 2. Multi-session store saves and reloads independently
        let mut s2 = Store::default();
        s2.bridges.insert("sess-a".into(), BridgeConfig {
            token: "111:AAHxhuW3JCGIl5uO99DJS5S1e8zcTWkJkdo".into(),
            enabled: true,
            bot_username: "bot_a".into(),
            user_id: Some(10),
            chat_id: Some(10),
            user_name: "@alice".into(),
            pair_code: String::new(),
        });
        s2.bridges.insert("sess-b".into(), BridgeConfig {
            token: "222:BBHxhuW3JCGIl5uO99DJS5S1e8zcTWkJkdo".into(),
            enabled: false,
            bot_username: "bot_b".into(),
            user_id: None,
            chat_id: None,
            user_name: String::new(),
            pair_code: "654321".into(),
        });
        save_store(&s2).unwrap();
        let loaded = load_store();
        assert_eq!(loaded.bridges.len(), 2);
        assert_eq!(loaded.bridges["sess-a"].bot_username, "bot_a");
        assert_eq!(loaded.bridges["sess-b"].pair_code, "654321");

        // 3. status_all returns both, drop_for removes only sess-a
        let all = status_all();
        assert_eq!(all["bridges"]["sess-a"]["botUsername"], "bot_a");
        assert_eq!(all["bridges"]["sess-b"]["botUsername"], "bot_b");
        drop_for("sess-a");
        let after = load_store();
        assert!(!after.bridges.contains_key("sess-a"));
        assert!(after.bridges.contains_key("sess-b"));

        unsafe { std::env::remove_var("VICUNA_TELEGRAM_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn voice_placeholder_is_edited_into_the_transcript() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            // fake Bot API: records "path body" and answers every call the same way
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let log = Arc::new(Mutex::new(Vec::<String>::new()));
            let log2 = log.clone();
            tokio::spawn(async move {
                loop {
                    let (mut c, _) = l.accept().await.unwrap();
                    let log = log2.clone();
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        loop {
                            let n = c.read(&mut tmp).await.unwrap_or(0);
                            if n == 0 { break; }
                            buf.extend_from_slice(&tmp[..n]);
                            let s = String::from_utf8_lossy(&buf).to_string();
                            if let Some(h) = s.find("\r\n\r\n") {
                                let len = s[..h].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                                if buf.len() >= h + 4 + len { break; }
                            }
                        }
                        let s = String::from_utf8_lossy(&buf).to_string();
                        let path = s.split_whitespace().nth(1).unwrap_or("").to_string();
                        let body = s.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                        let method = path.rsplit('/').next().unwrap_or("").to_string();
                        log.lock().unwrap().push(format!("{method} {body}"));
                        let resp = r#"{"ok":true,"result":{"message_id":42,"file_path":"voice/f.oga"}}"#;
                        let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}", resp.len()).as_bytes()).await;
                    });
                }
            });
            unsafe { std::env::set_var("VICUNA_TELEGRAM_API", format!("http://127.0.0.1:{port}")) };

            // success with one streamed partial
            let out = voice_to_text("1:tok", 7, "F", |_bytes, tx| async move {
                tokio::time::sleep(Duration::from_millis(1600)).await;
                let _ = tx.send("hello".into());
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(Some(" hello world ".into()))
            }).await;
            assert_eq!(out, VoiceOutcome::Text("hello world".into()));
            let calls = std::mem::take(&mut *log.lock().unwrap());
            assert!(calls[0].starts_with("sendMessage") && calls[0].contains("transcribing"), "{calls:?}");
            let edits: Vec<&String> = calls.iter().filter(|c| c.starts_with("editMessageText")).collect();
            assert_eq!(edits.len(), 2, "{calls:?}");
            assert!(edits[0].contains("🎙 hello…") && edits[0].contains("\"message_id\":42"), "{calls:?}");
            assert!(edits[1].contains("🎙 hello world\""), "{calls:?}");
            assert!(calls.iter().any(|c| c.starts_with("sendChatAction") && c.contains("typing")), "{calls:?}");
            assert!(calls.iter().any(|c| c.starts_with("getFile")), "{calls:?}");
            assert_eq!(calls.iter().filter(|c| c.starts_with("sendMessage")).count(), 1, "transcript edits the placeholder, no new message");

            // failure: the placeholder becomes a clear error
            let out = voice_to_text("1:tok", 7, "F", |_b, _tx| async move { Err("model broke".to_string()) }).await;
            assert_eq!(out, VoiceOutcome::Failed);
            let calls = std::mem::take(&mut *log.lock().unwrap());
            let last = calls.iter().rev().find(|c| c.starts_with("editMessageText")).expect("edited");
            assert!(last.contains("Couldn't transcribe") && last.contains("model broke"), "{calls:?}");

            // no speech
            let out = voice_to_text("1:tok", 7, "F", |_b, _tx| async move { Ok(Some("  ".into())) }).await;
            assert_eq!(out, VoiceOutcome::Empty);
            unsafe { std::env::remove_var("VICUNA_TELEGRAM_API") };
        });
    }

    #[test]
    fn chunks_split_long_text() {
        let long = "x".repeat(MAX_MSG * 2 + 5);
        let p = chunks(&long);
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|s| s.len() <= MAX_MSG));
        assert_eq!(chunks("hi\nthere"), vec!["hi\nthere".to_string()]);
    }
}
