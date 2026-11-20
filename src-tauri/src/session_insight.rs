//! Read-only views over the session database for vicuna-mcp (`list_sessions`,
//! `session_overview`, `search_sessions`, `read_session plain`). Everything
//! here reads sqlite directly, so it works on a headless `vicuna serve` with
//! no app window open (a lead driven from Telegram, say).
//!
//! Note: `server/telegram.rs` has its own tool-card flattening for phone
//! messages (`phone_text`); this module keeps a separate copy so the two can
//! evolve independently (the phone one folds runs of tools, this one keeps
//! every call on its own line).

use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;

/// Live state of one session, gathered from the vicuna-mcp turn registry.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Live {
    pub running: bool,
    /// an `ask_user` question or permission card is waiting on the user
    pub asking: bool,
}

pub type LiveMap = HashMap<String, Live>;

const OPENER: &str = "<details class=\"tool-result\">";
const ASK: &str = "<div class=\"ask-log\">";

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
}

/// One line for a tool card's `<summary>`: "tool — {json args}" -> "▸ tool: args".
fn card_line(summary: &str) -> String {
    let s = decode_entities(&strip_tags(summary));
    let s = s.trim();
    let (name, args) = match s.split_once(" — ") {
        Some((n, a)) => (n.trim(), a.trim()),
        None => (s, ""),
    };
    let name = name.strip_prefix("mcp__").and_then(|r| r.split_once("__").map(|(_, t)| t)).unwrap_or(name);
    // prefer the obvious field of a JSON args object
    let args = match serde_json::from_str::<Value>(args) {
        Ok(Value::Object(m)) => ["command", "file_path", "path", "pattern", "query", "url", "description"]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| args.to_string()),
        _ => args.to_string(),
    };
    let args = args.split_whitespace().collect::<Vec<_>>().join(" ");
    if args.is_empty() {
        format!("▸ {name}")
    } else {
        format!("▸ {name}: {}", clip(&args, 120))
    }
}

/// Assistant transcript markup -> plain text: every tool card collapses to one
/// `▸ tool: args` line (its output is dropped), ask records become prose, and
/// no HTML is left. A card ends at the last `</details>` before the next card
/// opener (harness notes inside a card may contain fake closers, see #82); an
/// unterminated last card (a live turn) is just its line.
pub fn plain_text(content: &str) -> String {
    let mut out = String::new();
    let mut rest = content;
    loop {
        let d = rest.find(OPENER);
        let a = rest.find(ASK);
        let (pos, is_card) = match (d, a) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some(d), Some(a)) => (d.min(a), d < a),
            (Some(d), None) => (d, true),
            (None, Some(a)) => (a, false),
        };
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if is_card {
            let body = &tail[OPENER.len()..];
            let next = body.find(OPENER).map(|n| n + OPENER.len()).unwrap_or(tail.len());
            let summary = body
                .find("<summary>")
                .and_then(|s0| body[s0 + 9..].find("</summary>").map(|e| &body[s0 + 9..s0 + 9 + e]))
                .unwrap_or("tool");
            out.push_str("\n");
            out.push_str(&card_line(summary));
            out.push_str("\n");
            rest = match tail[..next].rfind("</details>") {
                Some(e) => &tail[e + "</details>".len()..],
                None => &tail[next..],
            };
        } else {
            let end = tail.find("</div></div>").map(|e| e + "</div></div>".len());
            let block = &tail[..end.unwrap_or(tail.len())];
            let q = block
                .find("<div class=\"ask-log-q\">")
                .map(|i| &block[i..])
                .and_then(|b| b.find("</div>").map(|e| strip_tags(&b[..e])))
                .unwrap_or_default();
            let a = block
                .find("<div class=\"ask-log-a\">")
                .map(|i| strip_tags(&block[i..]))
                .unwrap_or_default();
            let a = a.trim();
            let a = a.strip_prefix("Answer:").map(str::trim).unwrap_or(a);
            out.push_str(&format!("[asked the user: {} → {}]", decode_entities(q.trim()), decode_entities(a)));
            rest = &tail[block.len()..];
        }
    }
    // drop any stray markup, collapse blank runs
    let mut s = out;
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    s.trim().to_string()
}

/// The last `max` chars of the plain text, cut at a line start when cheap.
pub fn plain_tail(content: &str, max: usize) -> String {
    let p = plain_text(content);
    let n = p.chars().count();
    if n <= max {
        return p;
    }
    let mut cut: String = p.chars().skip(n - max).collect();
    if let Some(nl) = cut.find('\n') {
        if nl < 200 {
            cut = cut[nl + 1..].to_string();
        }
    }
    format!("…{cut}")
}

fn extra_of(raw: Option<String>) -> Value {
    raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()).filter(Value::is_object).unwrap_or(json!({}))
}

/// `since` -> epoch ms: a number (ms), `30m`/`2h`/`3d`/`1w` ago, or a local
/// date/time sqlite understands (`2026-11-19`, `2026-11-19 14:30`, ISO 8601).
async fn since_ms(pool: &sqlx::SqlitePool, since: &str) -> Result<Option<i64>, String> {
    let s = since.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Some(n));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    if let Some(unit) = s.chars().last() {
        if let Ok(n) = s[..s.len() - unit.len_utf8()].trim().parse::<f64>() {
            let mult = match unit {
                'm' => 60_000.0,
                'h' => 3_600_000.0,
                'd' => 86_400_000.0,
                'w' => 604_800_000.0,
                _ => 0.0,
            };
            if mult > 0.0 {
                return Ok(Some(now - (n * mult) as i64));
            }
        }
    }
    let v: Option<i64> = sqlx::query_scalar("SELECT CAST(strftime('%s', $1, 'utc') AS INTEGER) * 1000")
        .bind(s)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("since: {e}"))?;
    v.map(Some).ok_or_else(|| format!("`since` not understood: {s} (use e.g. 2h, 3d, or 2026-11-19 14:30)"))
}

const ROW_SQL: &str = "SELECT c.id, c.title, c.mode, c.workspace, c.extra, \
     COALESCE(c.updated_at, c.created_at) AS last_ms, \
     datetime(COALESCE(c.updated_at, c.created_at) / 1000, 'unixepoch', 'localtime') AS last_local, \
     datetime(c.created_at / 1000, 'unixepoch', 'localtime') AS created_local, \
     (SELECT COUNT(*) FROM messages m WHERE m.chat_id = c.id) AS message_count \
     FROM chats c";

fn row_json(r: &sqlx::sqlite::SqliteRow, live: &LiveMap) -> Value {
    let id: String = r.try_get("id").unwrap_or_default();
    let x = extra_of(r.try_get::<Option<String>, _>("extra").ok().flatten());
    let l = live.get(&id).copied().unwrap_or_default();
    let mode = r.try_get::<Option<String>, _>("mode").ok().flatten().unwrap_or_else(|| "chat".into());
    json!({
        "id": id,
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "status": if l.running { "running" } else { "idle" },
        "waiting_on_question": l.asking,
        "mode": mode,
        "engine": x.get("engine").cloned().unwrap_or(Value::Null),
        "model": x.get("workModel").cloned().filter(|m| m.as_str() != Some("")).unwrap_or(Value::Null),
        "workspace": r.try_get::<Option<String>, _>("workspace").ok().flatten(),
        "last_activity": r.try_get::<Option<String>, _>("last_local").ok().flatten(),
        "last_activity_ms": r.try_get::<i64, _>("last_ms").unwrap_or(0),
        "created": r.try_get::<Option<String>, _>("created_local").ok().flatten(),
        "archived": x.get("archived").and_then(Value::as_bool).unwrap_or(false),
        "pinned": x.get("pinned").and_then(Value::as_bool).unwrap_or(false),
        "lead": x.get("role").and_then(Value::as_str) == Some("lead"),
        "lead_id": x.get("leadId").cloned().filter(|v| v.is_string()).unwrap_or(Value::Null),
        "message_count": r.try_get::<i64, _>("message_count").unwrap_or(0),
    })
}

/// Every session row with filters applied (newest activity first, or oldest
/// creation first for `sort: created`), plus how many archived rows were hidden.
async fn filtered_rows(pool: &sqlx::SqlitePool, args: &Value, live: &LiveMap) -> Result<(Vec<Value>, usize), String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let include_archived = args.get("include_archived").and_then(Value::as_bool).unwrap_or(false);
    let status = s("status").to_lowercase();
    if !["", "running", "idle", "any", "all"].contains(&status.as_str()) {
        return Err("`status` must be running, idle or any.".into());
    }
    let mode = s("mode").to_lowercase();
    let since = since_ms(pool, &s("since")).await?;
    let q = s("query").to_lowercase();
    let order = match s("sort").as_str() {
        "" | "recent" => "last_ms DESC",
        "created" => "c.created_at DESC",
        "title" => "c.title COLLATE NOCASE ASC",
        o => return Err(format!("`sort` must be recent, created or title (got {o}).")),
    };
    let rows = sqlx::query(&format!("{ROW_SQL} ORDER BY {order}"))
        .fetch_all(pool)
        .await
        .map_err(|e| format!("list sessions: {e}"))?;
    let mut hidden = 0;
    let mut out = Vec::new();
    for r in &rows {
        let v = row_json(r, live);
        if since.is_some_and(|t| v["last_activity_ms"].as_i64().unwrap_or(0) < t) {
            continue;
        }
        if (status == "running" && v["status"] != "running") || (status == "idle" && v["status"] != "idle") {
            continue;
        }
        if !mode.is_empty() && v["mode"] != mode.as_str() {
            continue;
        }
        if !q.is_empty()
            && !v["title"].as_str().unwrap_or("").to_lowercase().contains(&q)
            && !v["workspace"].as_str().unwrap_or("").to_lowercase().contains(&q)
            && !v["id"].as_str().unwrap_or("").starts_with(&q)
        {
            continue;
        }
        if v["archived"] == true && !include_archived {
            hidden += 1;
            continue;
        }
        out.push(v);
    }
    Ok((out, hidden))
}

/// `list_sessions` / `manage_sessions list`.
pub async fn list(pool: &sqlx::SqlitePool, args: &Value, live: &LiveMap) -> Result<Value, String> {
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20).clamp(1, 200) as usize;
    let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0).max(0) as usize;
    let (rows, hidden) = filtered_rows(pool, args, live).await?;
    let total = rows.len();
    let page: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let next = offset + page.len();
    Ok(json!({
        "total": total,
        "hidden_archived": hidden,
        "offset": offset,
        "returned": page.len(),
        "has_more": next < total,
        "next_offset": if next < total { json!(next) } else { Value::Null },
        "sessions": page,
    }))
}

/// One readable line per session (manage_sessions list keeps a text shape).
pub fn list_text(v: &Value) -> String {
    let mut lines = Vec::new();
    for r in v["sessions"].as_array().into_iter().flatten() {
        let mut tags = String::new();
        if r["lead"] == true {
            tags.push_str(" [lead]");
        }
        if r["archived"] == true {
            tags.push_str(" [archived]");
        }
        if r["waiting_on_question"] == true {
            tags.push_str(" [waiting on question]");
        }
        let eng = match (r["engine"].as_str(), r["model"].as_str()) {
            (Some(e), Some(m)) => format!("{e}/{m}"),
            (Some(e), None) => e.to_string(),
            _ => r["mode"].as_str().unwrap_or("").to_string(),
        };
        let worker = r["lead_id"].as_str().map(|l| format!(" · worker of {}", &l[..8.min(l.len())])).unwrap_or_default();
        lines.push(format!(
            "- {} · \"{}\" · {} · {} · {} · last {}{worker}{tags}",
            r["id"].as_str().unwrap_or(""),
            r["title"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            eng,
            r["workspace"].as_str().unwrap_or("-"),
            r["last_activity"].as_str().unwrap_or("?"),
        ));
    }
    let mut head = format!("Sessions ({} shown of {}", lines.len(), v["total"]);
    if v["hidden_archived"].as_u64().unwrap_or(0) > 0 {
        head.push_str(&format!("; {} archived hidden, pass include_archived: true", v["hidden_archived"]));
    }
    head.push_str("):");
    if v["has_more"] == true {
        lines.push(format!("(more: offset {})", v["next_offset"]));
    }
    format!("{head}\n{}", lines.join("\n"))
}

/// Message `idx` of role `role`, first or last; content cut to a window so a
/// 150 KB reply costs only `window` chars.
async fn pick_message(pool: &sqlx::SqlitePool, id: &str, role: &str, last: bool, window: i64) -> Result<Option<String>, String> {
    let (order, sub) = if last { ("DESC", "substr(content, -$3)") } else { ("ASC", "substr(content, 1, $3)") };
    sqlx::query_scalar(&format!(
        "SELECT {sub} FROM messages WHERE chat_id = $1 AND role = $2 AND content != '' ORDER BY idx {order} LIMIT 1"
    ))
    .bind(id)
    .bind(role)
    .bind(window)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("overview: {e}"))
}

/// `session_overview`: what the N most recently active sessions are doing.
pub async fn overview(pool: &sqlx::SqlitePool, args: &Value, live: &LiveMap) -> Result<Value, String> {
    let n = args.get("limit").and_then(Value::as_i64).unwrap_or(5).clamp(1, 20) as usize;
    let reply_chars = args.get("reply_chars").and_then(Value::as_i64).unwrap_or(800).clamp(100, 8000) as usize;
    let mut a = args.clone();
    if a.get("mode").is_none() {
        a["mode"] = json!("work");
    }
    if a["mode"] == "any" {
        a["mode"] = json!("");
    }
    let (rows, hidden) = filtered_rows(pool, &a, live).await?;
    let mut out = Vec::new();
    for mut r in rows.into_iter().take(n) {
        let id = r["id"].as_str().unwrap_or("").to_string();
        let first = pick_message(pool, &id, "user", false, 2000).await?.unwrap_or_default();
        let latest = pick_message(pool, &id, "user", true, 2000).await?.unwrap_or_default();
        let reply = pick_message(pool, &id, "assistant", true, (reply_chars * 8).max(20_000) as i64).await?.unwrap_or_default();
        r["first_request"] = json!(clip(&first, 300));
        r["latest_user_message"] = json!(clip(&latest, 300));
        r["latest_reply_tail"] = json!(plain_tail(&reply, reply_chars));
        if let Some(o) = r.as_object_mut() {
            o.remove("last_activity_ms");
            o.remove("created");
        }
        out.push(r);
    }
    Ok(json!({ "hidden_archived": hidden, "sessions": out }))
}

/// `search_sessions`: titles + message text (server_store::search_chats).
pub async fn search(pool: &sqlx::SqlitePool, args: &Value, live: &LiveMap) -> Result<Value, String> {
    let q = args.get("query").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if q.is_empty() {
        return Err("`query` is required.".into());
    }
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10).clamp(1, 50) as usize;
    let include_archived = args.get("include_archived").and_then(Value::as_bool).unwrap_or(false);
    let hits = crate::server_store::search_chats(pool, q, &[], 50).await?;
    let mut out = Vec::new();
    let mut hidden = 0;
    for h in hits.as_array().into_iter().flatten() {
        let id = h["id"].as_str().unwrap_or("");
        let row = sqlx::query(&format!("{ROW_SQL} WHERE c.id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("search: {e}"))?;
        let Some(row) = row else { continue };
        let r = row_json(&row, live);
        if r["archived"] == true && !include_archived {
            hidden += 1;
            continue;
        }
        if out.len() >= limit {
            continue;
        }
        out.push(json!({
            "id": id,
            "title": r["title"],
            "snippet": decode_entities(&decode_entities(&plain_text(h["snippet"].as_str().unwrap_or("")))),
            "last_activity": r["last_activity"],
            "status": r["status"],
            "workspace": r["workspace"],
            "archived": r["archived"],
        }));
    }
    Ok(json!({ "query": q, "hidden_archived": hidden, "results": out }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (sqlx::SqlitePool, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("vicuna-insight-{}-{}", std::process::id(), rand_suffix()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = crate::server_store::open(&dir.join("vicuna.db")).await.unwrap();
        let ins = |id: &str, title: &str, extra: &str, t: i64| {
            let (id, title, extra) = (id.to_string(), title.to_string(), extra.to_string());
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO chats (id, title, created_at, updated_at, mode, workspace, extra) VALUES ($1,$2,$3,$3,'work','/w',$4)")
                    .bind(&id).bind(&title).bind(t).bind(&extra).execute(&pool).await.unwrap();
            }
        };
        ins("aaa", "Visible one", r#"{"engine":"claude","workModel":"haiku","leadId":"lll"}"#, 3000).await;
        ins("bbb", "Archived one", r#"{"archived":true}"#, 2000).await;
        ins("lll", "Lead", r#"{"role":"lead","pinned":true}"#, 1000).await;
        let msg = |chat: &str, idx: i64, role: &str, content: &str| {
            let (chat, role, content) = (chat.to_string(), role.to_string(), content.to_string());
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO messages (chat_id, role, content, idx) VALUES ($1,$2,$3,$4)")
                    .bind(&chat).bind(&role).bind(&content).bind(idx).execute(&pool).await.unwrap();
            }
        };
        msg("aaa", 0, "user", "Please fix the zebra bug").await;
        msg("aaa", 1, "assistant", "Looking.\n\n<details class=\"tool-result\"><summary>Bash — {&quot;command&quot;:&quot;ls -la &lt;x&gt;&quot;}</summary><pre><code>a.txt\n</details> fake</code></pre></details>\n\nAll done, zebra fixed.").await;
        msg("aaa", 2, "user", "thanks").await;
        msg("bbb", 0, "user", "archived zebra talk").await;
        (pool, dir)
    }

    fn rand_suffix() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    #[test]
    fn plain_text_collapses_cards_and_asks() {
        let s = "Hi\n\n<details class=\"tool-result\"><summary>mcp__vicuna-mcp__ask — x</summary><pre><code>out &lt;!--</code></pre></details>\n\n\
                 <details class=\"tool-result\"><summary>Read — {&quot;file_path&quot;:&quot;/a/b.rs&quot;}</summary><pre><code>body";
        let p = plain_text(s);
        assert_eq!(p, "Hi\n\n▸ ask: x\n\n▸ Read: /a/b.rs", "{p}");
        let a = "Q <div class=\"ask-log\"><div class=\"ask-log-q\">Which?</div><div class=\"ask-log-a\"><strong>Answer:</strong> Kiwi &amp; lime</div></div> end";
        assert_eq!(plain_text(a), "Q [asked the user: Which? → Kiwi & lime] end");
        assert!(!plain_tail(&"x".repeat(5000), 100).contains('<'));
        assert_eq!(plain_tail(&"y".repeat(5000), 100).chars().count(), 101);
    }

    #[tokio::test]
    async fn list_hides_archived_by_default_and_tags_them_on_request() {
        let (pool, dir) = db().await;
        let mut live = LiveMap::new();
        live.insert("aaa".into(), Live { running: true, asking: true });
        let v = list(&pool, &json!({}), &live).await.unwrap();
        let ids: Vec<&str> = v["sessions"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["aaa", "lll"]);
        assert_eq!(v["hidden_archived"], 1);
        assert_eq!(v["sessions"][0]["status"], "running");
        assert_eq!(v["sessions"][0]["waiting_on_question"], true);
        assert_eq!(v["sessions"][0]["lead_id"], "lll");
        assert_eq!(v["sessions"][0]["model"], "haiku");
        let t = list_text(&v);
        assert!(t.contains("1 archived hidden") && !t.contains("[archived]"), "{t}");
        let v = list(&pool, &json!({"include_archived": true}), &live).await.unwrap();
        assert_eq!(v["total"], 3);
        let t = list_text(&v);
        assert!(t.contains("\"Archived one\"") && t.contains("[archived]") && t.contains("[lead]"), "{t}");
        let v = list(&pool, &json!({"status": "running"}), &live).await.unwrap();
        assert_eq!(v["total"], 1);
        let v = list(&pool, &json!({"since": "1970-01-01 00:00:02", "sort": "title", "include_archived": true}), &LiveMap::new()).await;
        assert!(v.is_ok(), "{v:?}");
        let v = list(&pool, &json!({"since": "1h"}), &live).await.unwrap();
        assert_eq!(v["total"], 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn overview_is_plain_text() {
        let (pool, dir) = db().await;
        let v = overview(&pool, &json!({"limit": 1}), &LiveMap::new()).await.unwrap();
        let s = &v["sessions"][0];
        assert_eq!(s["id"], "aaa");
        assert_eq!(s["first_request"], "Please fix the zebra bug");
        assert_eq!(s["latest_user_message"], "thanks");
        let tail = s["latest_reply_tail"].as_str().unwrap();
        assert_eq!(tail, "Looking.\n\n▸ Bash: ls -la <x>\n\nAll done, zebra fixed.");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn search_finds_messages_and_skips_archived() {
        let (pool, dir) = db().await;
        let v = search(&pool, &json!({"query": "zebra"}), &LiveMap::new()).await.unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["results"][0]["id"], "aaa");
        assert!(v["results"][0]["snippet"].as_str().unwrap().contains("zebra"));
        assert_eq!(v["hidden_archived"], 1);
        let v = search(&pool, &json!({"query": "zebra", "include_archived": true}), &LiveMap::new()).await.unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
