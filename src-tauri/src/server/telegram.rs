//! Telegram bridge: talk to the lead session from a phone.
//!
//! One long-poll loop per server (`getUpdates`, no webhook, so it works on a
//! tailnet-only box). The bot is public, so nothing reaches the lead until a
//! Telegram user proves they can see Vicuna: the app shows a pairing code and
//! only the user who sends it is accepted (one paired user). The token lives
//! in a 0600 `~/vicuna/config/telegram.json` and never goes back to a window.
//!
//! Text/photos from the paired user go to the session marked lead (queued
//! into its running turn, else a new server-started turn). Every lead turn,
//! whoever started it (the phone, a window, a worker report), is mirrored:
//! a progress message edited while it runs, then the final reply. Agent
//! questions and permission cards become inline-keyboard buttons; a plain
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

// ---------- config ----------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
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

fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VICUNA_TELEGRAM_CONFIG") {
        return Some(PathBuf::from(p));
    }
    Some(crate::opencode::home_dir()?.join("vicuna").join("config").join("telegram.json"))
}

pub fn load_config() -> Config {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(c: &Config) -> Result<(), String> {
    let p = config_path().ok_or("no home directory")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(c).unwrap_or_default()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
    bump();
    Ok(())
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

#[derive(Default)]
struct Runtime {
    /// bumped on every config change so the poller restarts its request
    generation: u64,
    last_error: String,
    polling: bool,
    /// lead turns being mirrored (turn id)
    following: HashSet<String>,
    /// the ask card shown on the phone: (session, ask_id, options, multi)
    asks: HashMap<String, (String, Vec<String>, bool)>,
    /// the model list last offered by /model: (engine, model ids)
    model_menu: Option<(String, Vec<String>)>,
}

fn rt() -> &'static Mutex<Runtime> {
    static R: OnceLock<Mutex<Runtime>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn bump() {
    if let Ok(mut r) = rt().lock() {
        r.generation += 1;
    }
}

fn set_error(e: impl Into<String>) {
    if let Ok(mut r) = rt().lock() {
        r.last_error = e.into();
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

pub async fn action(st: &Arc<BridgeState>, cmd: &str, args: &Value) -> Result<Value, String> {
    let _ = st;
    match cmd {
        "telegram_status" => Ok(status()),
        "telegram_save" => {
            let token = args.get("token").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if !valid_token_shape(&token) {
                return Err("That doesn't look like a bot token (123456789:AA…). Copy it from @BotFather.".into());
            }
            let me = call(&token, "getMe", json!({})).await?;
            let mut c = load_config();
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
            save_config(&c)?;
            set_error("");
            Ok(status())
        }
        "telegram_set" => {
            let mut c = load_config();
            if c.token.is_empty() {
                return Err("no bot token saved".into());
            }
            if let Some(e) = args.get("enabled").and_then(Value::as_bool) {
                c.enabled = e;
            }
            save_config(&c)?;
            Ok(status())
        }
        "telegram_unpair" => {
            let mut c = load_config();
            c.user_id = None;
            c.chat_id = None;
            c.user_name.clear();
            c.pair_code = new_pair_code();
            save_config(&c)?;
            Ok(status())
        }
        "telegram_remove" => {
            if let Some(p) = config_path() {
                let _ = std::fs::remove_file(p);
            }
            bump();
            set_error("");
            Ok(status())
        }
        "whisper_status" => Ok(crate::whisper::status()),
        "whisper_install" => crate::whisper::start_install(args.get("model").and_then(Value::as_str).unwrap_or("tiny")),
        "whisper_uninstall" => crate::whisper::uninstall(args.get("model").and_then(Value::as_str).unwrap_or("tiny")),
        _ => Err(format!("unknown telegram action {cmd}")),
    }
}

pub fn status() -> Value {
    let c = load_config();
    let (err, polling) = rt().lock().map(|r| (r.last_error.clone(), r.polling)).unwrap_or_default();
    json!({
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

// ---------- the loop ----------

pub fn spawn(st: Arc<BridgeState>) {
    tokio::spawn(async move {
        let mut offset: i64 = 0;
        let mut last_token = String::new();
        loop {
            let c = load_config();
            if c.token.is_empty() || !c.enabled {
                if let Ok(mut r) = rt().lock() {
                    r.polling = false;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            if c.token != last_token {
                offset = 0;
                last_token = c.token.clone();
                register_commands(&c.token).await;
            }
            // mirror lead turns nobody started from here (window, worker report)
            if let Some(chat) = c.chat_id {
                watch_lead(&st, &c.token, chat).await;
            }
            let generation = rt().lock().map(|r| r.generation).unwrap_or(0);
            let req = call(&c.token, "getUpdates", json!({"offset": offset, "timeout": 20, "allowed_updates": ["message", "callback_query"]}));
            // a config change (new token, disable) cancels the long poll
            let changed = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if rt().lock().map(|r| r.generation).unwrap_or(0) != generation {
                        break;
                    }
                    if c.chat_id.is_some() && lead_has_unfollowed_turn(&st).await {
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
                    if let Ok(mut r) = rt().lock() {
                        r.polling = true;
                        r.last_error.clear();
                    }
                    for u in updates.as_array().cloned().unwrap_or_default() {
                        if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                            offset = offset.max(id + 1);
                        }
                        if let Err(e) = handle_update(&st, &u).await {
                            set_error(e);
                        }
                    }
                }
                Err(e) => {
                    if let Ok(mut r) = rt().lock() {
                        r.polling = false;
                    }
                    set_error(e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
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

async fn lead_has_unfollowed_turn(st: &BridgeState) -> bool {
    let Some(id) = lead_id(st).await else { return false };
    match live_turn(st, &id) {
        Some((tid, _)) => !rt().lock().map(|r| r.following.contains(&tid)).unwrap_or(true),
        None => false,
    }
}

async fn watch_lead(st: &Arc<BridgeState>, token: &str, chat: i64) {
    if let Some(id) = lead_id(st).await {
        if let Some((tid, _)) = live_turn(st, &id) {
            follow(st.clone(), token.to_string(), chat, id, tid);
        }
    }
}

async fn handle_update(st: &Arc<BridgeState>, u: &Value) -> Result<(), String> {
    let mut c = load_config();
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
            send_now_reply(st, &token, chat).await;
            return Ok(());
        }
        if let Some(e) = data.strip_prefix("e:") {
            let out = pick_engine(st, e).await;
            reply_or_edit(&token, chat, msg_id, out).await;
            return Ok(());
        }
        if let Some(i) = data.strip_prefix("m:") {
            let out = pick_model(st, i.parse().unwrap_or(usize::MAX)).await;
            reply_or_edit(&token, chat, msg_id, (out, None)).await;
            return Ok(());
        }
        if let Some((sid, idx)) = data.strip_prefix("a:").and_then(|d| d.rsplit_once(':')) {
            let idx: usize = idx.parse().unwrap_or(usize::MAX);
            let chat = c.chat_id.unwrap_or_default();
            match answer_ask(st, sid, Answer::Choice(idx)).await {
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
            save_config(&c)?;
            send_text(&token, chat, "✅ Paired with Vicuna. Messages you send here go to your lead session.\n\n/status · /stop · /help", None).await;
        } else if private {
            send_text(&token, chat, "This bot is private. Open Vicuna → right-click the lead session → Telegram, and send the pairing code shown there.", None).await;
        }
        return Ok(());
    }
    if from != c.user_id {
        return Ok(()); // strangers are ignored once paired
    }
    let t = text.trim();
    if t == "/help" || t == "/start" {
        send_text(&token, chat, "Send any message to talk to your lead session. Photos, files and voice memos work too.\n/status: what the lead is doing\n/model: switch the lead's harness and model (/model claude haiku)\n/send_now: interrupt the running turn and send your queued messages now\n/stop: stop the lead's running turn", None).await;
        return Ok(());
    }
    let Some(lead) = lead_id(st).await else {
        send_text(&token, chat, "No lead session. In Vicuna, right-click a Work session → Make lead.", None).await;
        return Ok(());
    };
    if t == "/model" || t.starts_with("/model ") || t.starts_with("/model@") {
        let rest: Vec<&str> = t.split_whitespace().skip(1).collect();
        let (text, kb) = match rest.as_slice() {
            [] => model_overview(st, &lead).await,
            [e] => pick_engine(st, e).await,
            [e, m, ..] => match set_lead(st, &lead, e, m).await {
                Ok(s) => (s, None),
                Err(e) => (format!("⚠️ {e}"), None),
            },
        };
        send_text(&token, chat, &text, kb).await;
        return Ok(());
    }
    if t == "/status" {
        let db = st.db.as_ref().ok_or("no server db")?;
        let ch = crate::server_store::load_chat(db, &lead).await?.unwrap_or(Value::Null);
        let title = ch.get("title").and_then(Value::as_str).unwrap_or("Lead");
        let state = if live_turn(st, &lead).is_some() { "working" } else { "idle" };
        let (engine, model) = lead_engine_model(&ch);
        send_text(&token, chat, &format!("Lead \"{title}\" is {state} · {engine}/{model}"), None).await;
        return Ok(());
    }
    if t == "/send_now" || t.starts_with("/send_now@") {
        send_now_reply(st, &token, chat).await;
        return Ok(());
    }
    if t == "/stop" {
        match live_turn(st, &lead) {
            Some((tid, _)) => {
                if let Some(t) = st.agent.get_turn(&tid) { crate::work_agent::agent_stop(&t); }
                send_text(&token, chat, "⏹ Stopping the lead's turn…", None).await;
            }
            None => {
                send_text(&token, chat, "The lead is idle.", None).await;
            }
        }
        return Ok(());
    }
    // a text reply answers a waiting free-form question
    if !t.is_empty() && pending_ask_for(st, &lead).is_some() {
        match answer_ask(st, &lead, Answer::Text(t.to_string())).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                send_text(&token, chat, &format!("⚠️ {e}"), None).await;
                return Ok(());
            }
        }
    }
    let mut text_out = t.to_string();
    // voice memos: transcribe on the server (if a Whisper model is installed),
    // show the transcript on the phone first, then send it to the lead as text
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
                VoiceOutcome::Failed => {} // the audio file goes to the lead below
            }
            if !text_out.is_empty() && text_out != t {
                // transcript ready: a waiting free-form question takes it as the answer
                if pending_ask_for(st, &lead).is_some() {
                    if let Err(e) = answer_ask(st, &lead, Answer::Text(text_out.clone())).await {
                        send_text(&token, chat, &format!("⚠️ {e}"), None).await;
                    }
                    return Ok(());
                }
            }
        } else {
            send_text(&token, chat, "🎙 Got your voice memo. No transcription model is installed on the server, so it goes to the lead as an audio file.", None).await;
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
                match crate::upload_stage_file_logic(lead.clone(), name.clone(), b64).await {
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
    let queued = live_turn(st, &lead).is_some();
    if let Err(e) = turns::deliver(st, &lead, m).await {
        send_text(&token, chat, &format!("⚠️ {e}"), None).await;
        return Ok(());
    }
    if queued {
        let kb = json!({"inline_keyboard": [[{"text": "⚡ Send now", "callback_data": "sn"}]]});
        send_text(&token, chat, "📨 Queued for the lead's running turn. Tap Send now (or /send_now) to interrupt it and send right away.", Some(kb)).await;
    }
    if let Some((tid, _)) = live_turn(st, &lead) {
        follow(st.clone(), token, chat, lead, tid);
    }
    Ok(())
}

async fn send_now_reply(st: &Arc<BridgeState>, token: &str, chat: i64) {
    let Some(lead) = lead_id(st).await else {
        send_text(token, chat, "No lead session.", None).await;
        return;
    };
    if live_turn(st, &lead).is_none() {
        send_text(token, chat, "The lead is idle, nothing is waiting.", None).await;
        return;
    }
    send_text(token, chat, "⚡ Interrupting the lead…", None).await;
    // the stop + restart can take a while: don't hold up the update loop
    let (st, token) = (st.clone(), token.to_string());
    tokio::spawn(async move {
        match turns::send_now(&st, &lead).await {
            Ok(0) => {
                send_text(&token, chat, "Nothing is queued: your messages already reached the lead.", None).await;
            }
            Ok(_) => {
                if let Some((tid, _)) = live_turn(&st, &lead) {
                    follow(st.clone(), token, chat, lead, tid);
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
    ("status", "What the lead is doing, harness and model"),
    ("model", "Switch the lead's harness and model"),
    ("send_now", "Interrupt the lead and send queued messages now"),
    ("stop", "Stop the lead's running turn"),
    ("help", "How this bot works"),
];

async fn register_commands(token: &str) {
    let cmds: Vec<Value> = COMMANDS.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
    if let Err(e) = call(token, "setMyCommands", json!({"commands": cmds})).await {
        set_error(e);
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
fn lead_engine_model(chat: &Value) -> (String, String) {
    let x = turns::parse_extra(chat);
    let engine = crate::work_engine::effective_engine(x.engine.as_deref());
    let model = if x.engine.is_some() { x.work_model.clone() } else { engine.saved_model() };
    (engine.id().to_string(), model.filter(|m| !m.is_empty()).unwrap_or_else(|| "default".into()))
}

async fn model_overview(st: &Arc<BridgeState>, lead: &str) -> (String, Option<Value>) {
    let ch = match st.db.as_ref() {
        Some(db) => crate::server_store::load_chat(db, lead).await.ok().flatten().unwrap_or(Value::Null),
        None => Value::Null,
    };
    let (engine, model) = lead_engine_model(&ch);
    let info = serde_json::to_value(crate::work_engine::engines_info().await).unwrap_or(Value::Null);
    let rows: Vec<Value> = info["engines"].as_array().into_iter().flatten()
        .filter(|e| e["installed"] == json!(true))
        .map(|e| {
            let id = e["id"].as_str().unwrap_or("");
            let mark = if id == engine { "✓ " } else { "" };
            json!([{"text": format!("{mark}{}", e["label"].as_str().unwrap_or(id)), "callback_data": format!("e:{id}")}])
        })
        .collect();
    (format!("Lead runs {engine}/{model}.\nPick a harness:"), (!rows.is_empty()).then(|| json!({"inline_keyboard": rows})))
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
async fn pick_engine(st: &Arc<BridgeState>, e: &str) -> (String, Option<Value>) {
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
        r.model_menu = Some((engine.id().to_string(), ids));
    }
    (format!("{} models:", engine.id()), Some(json!({"inline_keyboard": rows})))
}

async fn pick_model(st: &Arc<BridgeState>, i: usize) -> String {
    let pick = rt().lock().ok().and_then(|r| r.model_menu.clone());
    let Some((engine, ids)) = pick else { return "⚠️ That menu expired, send /model again.".into() };
    let Some(model) = ids.get(i).cloned() else { return "⚠️ Unknown model.".into() };
    let Some(lead) = lead_id(st).await else { return "No lead session.".into() };
    match set_lead(st, &lead, &engine, &model).await {
        Ok(s) => s,
        Err(e) => format!("⚠️ {e}"),
    }
}

async fn set_lead(st: &Arc<BridgeState>, lead: &str, engine: &str, model: &str) -> Result<String, String> {
    turns::cli_session(st, "update", &json!({"id": lead, "engine": engine, "model": model})).await?;
    let note = if live_turn(st, lead).is_some() { " (from the next turn; one is running now)" } else { "" };
    Ok(format!("✅ Lead now runs {engine}/{model}{note}."))
}

/// Minimum gap between edits of the voice placeholder (Telegram rate limits).
const PARTIAL_EDIT_EVERY: Duration = Duration::from_millis(1500);

#[derive(Debug, PartialEq)]
pub enum VoiceOutcome {
    Text(String),
    /// no speech: nothing goes to the lead
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
            show(format!("⚠️ Couldn't transcribe the voice memo ({e}). Sending the audio file to the lead instead.")).await;
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
    let (tid, turn) = live_turn(st, session).ok_or("The lead's turn already ended.")?;
    let wire = if multi { json!([text]).to_string() } else { text.clone() };
    turns::record_answer(st, &turn, &tid, &ask_id, wire, Some(&text), false)?;
    if let Ok(mut r) = rt().lock() {
        r.asks.remove(session);
    }
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

// ---------- mirroring a lead turn ----------

fn follow(st: Arc<BridgeState>, token: String, chat: i64, session: String, turn_id: String) {
    {
        let Ok(mut r) = rt().lock() else { return };
        if !r.following.insert(turn_id.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let progress = send_text(&token, chat, "⏳ Lead is working…", None).await;
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
                        let q = a.get("question").and_then(Value::as_str).unwrap_or("The lead has a question");
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
