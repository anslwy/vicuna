// Chat storage for a headless Vicuna server (`vicuna serve`).
//
// The desktop app keeps chats in the sql plugin's vicuna.db and rewrites the
// whole table set on every save (one transaction, see `chats_save`). A server
// is shared by every desktop that connects to it, so a full rewrite from one
// client would delete the sessions another client just created. Saves here
// are therefore a MERGE: the client sends only the chats it changed plus the
// ids it deleted itself; each changed chat is replaced (row + messages) and
// nothing else is touched. All in one transaction, like `chats_save`.
//
// Schema is identical to the desktop's (store.ts createSqliteBackend), so a
// server database could be opened by the desktop and vice versa.

use std::path::Path;

use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;

/// Pool type re-exported so the bridge needs no sqlx dependency of its own.
pub type Pool = SqlitePool;

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS chats (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL, forked_from TEXT, fork_from_index INTEGER)",
    "CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', thinking TEXT NOT NULL DEFAULT '', model TEXT, idx INTEGER NOT NULL)",
    "CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, idx)",
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
];
const CHAT_MIGRATIONS: &[&str] = &["forked_from TEXT", "fork_from_index INTEGER", "model TEXT", "mode TEXT", "workspace TEXT", "goal TEXT", "updated_at INTEGER", "extra TEXT", "rev INTEGER", "server_recorded INTEGER"];
const MSG_MIGRATIONS: &[&str] = &["images TEXT", "gen_ms REAL", "gen_tps REAL", "summary INTEGER", "gen_cost REAL"];

/// Open (creating if needed) a server database at `path`.
pub async fn open(path: &Path) -> Result<SqlitePool, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    init_schema(&pool).await?;
    Ok(pool)
}

/// Consistent snapshot of a (possibly live, WAL-mode) server database into
/// `dest` via `VACUUM INTO`: SQLite reads it inside one transaction, so the
/// copy includes committed WAL pages and is never torn — unlike copying
/// `vicuna.db` + `-wal` + `-shm` by hand. Opens read-only; never migrates.
pub async fn backup_to(src: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    let opts = SqliteConnectOptions::new().filename(src).read_only(true).create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| format!("open {}: {e}", src.display()))?;
    let r = sqlx::query("VACUUM INTO ?")
        .bind(dest.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .map_err(|e| format!("backup to {}: {e}", dest.display()));
    pool.close().await;
    r.map(|_| ())
}

pub async fn init_schema(pool: &SqlitePool) -> Result<(), String> {
    for q in SCHEMA {
        sqlx::query(q).execute(pool).await.map_err(|e| format!("schema: {e}"))?;
    }
    for col in CHAT_MIGRATIONS {
        let _ = sqlx::query(&format!("ALTER TABLE chats ADD COLUMN {col}")).execute(pool).await;
    }
    for col in MSG_MIGRATIONS {
        let _ = sqlx::query(&format!("ALTER TABLE messages ADD COLUMN {col}")).execute(pool).await;
    }
    Ok(())
}

const CHAT_COLS: &str = "id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra, rev";

fn chat_json(r: &sqlx::sqlite::SqliteRow, messages: Vec<Value>) -> Value {
    let created: i64 = r.try_get("created_at").unwrap_or(0);
    let forked: Option<String> = r.try_get("forked_from").ok().flatten();
    json!({
        "id": r.try_get::<String, _>("id").unwrap_or_default(),
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "createdAt": created,
        "updatedAt": r.try_get::<Option<i64>, _>("updated_at").ok().flatten().unwrap_or(created),
        "forkedFrom": forked.and_then(|f| serde_json::from_str::<Value>(&f).ok()).unwrap_or(Value::Null),
        "forkFromIndex": r.try_get::<Option<i64>, _>("fork_from_index").ok().flatten(),
        "mode": if r.try_get::<Option<String>, _>("mode").ok().flatten().as_deref() == Some("work") { "work" } else { "chat" },
        "workspace": r.try_get::<Option<String>, _>("workspace").ok().flatten(),
        "goal": r.try_get::<Option<String>, _>("goal").ok().flatten(),
        "extra": r.try_get::<Option<String>, _>("extra").ok().flatten(),
        "rev": r.try_get::<Option<i64>, _>("rev").ok().flatten().unwrap_or(0),
        "messages": messages,
    })
}

fn message_json(m: &sqlx::sqlite::SqliteRow) -> Value {
    let mut o = json!({
        "role": m.try_get::<String, _>("role").unwrap_or_default(),
        "content": m.try_get::<String, _>("content").unwrap_or_default(),
    });
    let s = |k: &str| m.try_get::<Option<String>, _>(k).ok().flatten().filter(|v| !v.is_empty());
    if let Some(t) = s("thinking") {
        o["thinking"] = json!(t);
    }
    if let Some(t) = s("model") {
        o["model"] = json!(t);
    }
    if let Some(imgs) = s("images").and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_array) {
        o["images"] = imgs;
    }
    for (col, key) in [("gen_ms", "genMs"), ("gen_tps", "genTps"), ("gen_cost", "genCost")] {
        if let Some(v) = m.try_get::<Option<f64>, _>(col).ok().flatten() {
            o[key] = json!(v);
        }
    }
    if m.try_get::<Option<i64>, _>("summary").ok().flatten() == Some(1) {
        o["summary"] = json!(true);
    }
    o
}

/// Every chat with all messages, shaped like the frontend's `Chat`, newest
/// activity first (same order as the desktop's sqlite loadChats).
pub async fn load_chats(pool: &SqlitePool) -> Result<Value, String> {
    let mut tx = pool.begin().await.map_err(|e| format!("load chats: {e}"))?;
    let chats = sqlx::query(&format!("SELECT {CHAT_COLS} FROM chats ORDER BY COALESCE(updated_at, created_at) DESC"))
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("load chats: {e}"))?;
    let msgs = sqlx::query(
        "SELECT chat_id, role, content, thinking, model, images, gen_ms, gen_tps, summary, gen_cost FROM messages ORDER BY chat_id, idx",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| format!("load messages: {e}"))?;
    tx.commit().await.map_err(|e| format!("load chats: {e}"))?;
    let mut by_chat: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for m in &msgs {
        let id: String = m.try_get("chat_id").unwrap_or_default();
        by_chat.entry(id).or_default().push(message_json(m));
    }
    Ok(Value::Array(
        chats
            .iter()
            .map(|r| {
                let id: String = r.try_get("id").unwrap_or_default();
                chat_json(r, by_chat.remove(&id).unwrap_or_default())
            })
            .collect(),
    ))
}

/// Result of a merge: the new revision of every chat that was written, and
/// the server's current copy of every chat that was refused.
#[derive(Debug, Default)]
pub struct MergeOutcome {
    pub revs: std::collections::HashMap<String, i64>,
    pub conflicts: Vec<Value>,
}

impl MergeOutcome {
    pub fn to_json(&self) -> Value {
        json!({ "revs": self.revs, "conflicts": self.conflicts })
    }
}

async fn insert_messages<'c>(
    tx: &mut sqlx::Transaction<'c, sqlx::Sqlite>,
    chat_id: &str,
    messages: &[crate::MsgSaveRow],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(chat_id).execute(&mut **tx).await?;
    for (i, m) in messages.iter().enumerate() {
        let images = m.images.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default());
        sqlx::query(
            "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(chat_id)
        .bind(&m.role)
        .bind(&m.content)
        .bind(&m.thinking)
        .bind(&m.model)
        .bind(i as i64)
        .bind(images)
        .bind(m.gen_ms)
        .bind(m.gen_tps)
        .bind(m.summary.map(|b| if b { 1 } else { 0 }))
        .bind(m.gen_cost)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Replace the given chats (row + every message) and delete `deleted`, in one
/// transaction. Chats not mentioned are left alone (other clients' work).
///
/// Each chat carries a revision (`rev`, bumped on every write, by clients and
/// by the server's own turn recorder). A client sends the revision it last
/// saw as `baseRev`; when the server's copy has moved on since (the server
/// recorded a Work turn while that window was asleep, or another window
/// saved), the chat is NOT written and its current copy comes back in
/// `conflicts` so the client can adopt it. Chats in `live` (a Work turn the
/// server is recording right now) are refused the same way: the server is
/// their only writer until the turn ends. A save without `baseRev` (older
/// clients) is applied like before only until the server first records a
/// turn in that chat. Older windows cannot overwrite a recorded transcript.
pub(crate) async fn merge_chats(
    pool: &SqlitePool,
    chats: Vec<crate::ChatSaveRow>,
    deleted: Vec<String>,
    live: &std::collections::HashSet<String>,
) -> Result<MergeOutcome, String> {
    let err = |e: sqlx::Error| format!("save chats: {e}");
    let mut out = MergeOutcome::default();
    let mut refused: Vec<String> = Vec::new();
    // IMMEDIATE: a deferred read-then-write transaction fails at once with
    // SQLITE_BUSY ("database is locked") when another writer got there first
    // (busy_timeout can't help an upgrade); IMMEDIATE waits its turn instead
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(err)?;
    for id in &deleted {
        sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(id).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("DELETE FROM chats WHERE id = $1").bind(id).execute(&mut *tx).await.map_err(err)?;
    }
    for chat in chats {
        if chat.id.is_empty() {
            continue;
        }
        let current = sqlx::query("SELECT rev, server_recorded FROM chats WHERE id = $1")
            .bind(&chat.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?;
        let exists = current.is_some();
        let cur_rev = current.as_ref().and_then(|r| r.try_get::<Option<i64>, _>("rev").ok().flatten()).unwrap_or(0);
        let recorded = current.as_ref().and_then(|r| r.try_get::<Option<i64>, _>("server_recorded").ok().flatten()).unwrap_or(0);
        if exists && (live.contains(&chat.id) || chat.base_rev.is_some_and(|b| b != cur_rev) || (recorded != 0 && chat.base_rev.is_none())) {
            refused.push(chat.id.clone());
            continue;
        }
        let rev = cur_rev + 1;
        sqlx::query(
            "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra, rev) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) ON CONFLICT(id) DO UPDATE SET title=excluded.title, created_at=excluded.created_at, forked_from=excluded.forked_from, fork_from_index=excluded.fork_from_index, mode=excluded.mode, workspace=excluded.workspace, goal=excluded.goal, updated_at=excluded.updated_at, extra=excluded.extra, rev=excluded.rev",
        )
        .bind(&chat.id)
        .bind(&chat.title)
        .bind(chat.created_at)
        .bind(&chat.forked_from)
        .bind(chat.fork_from_index)
        .bind(chat.mode.clone().unwrap_or_else(|| "chat".into()))
        .bind(&chat.workspace)
        .bind(&chat.goal)
        .bind(chat.updated_at.unwrap_or(chat.created_at))
        .bind(&chat.extra)
        .bind(rev)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        insert_messages(&mut tx, &chat.id, &chat.messages).await.map_err(err)?;
        out.revs.insert(chat.id.clone(), rev);
    }
    tx.commit().await.map_err(err)?;
    for id in refused {
        if let Some(c) = load_chat(pool, &id).await? {
            out.conflicts.push(c);
        }
    }
    Ok(out)
}

/// One chat with all its messages (same shape as `load_chats` entries).
pub async fn load_chat(pool: &SqlitePool, id: &str) -> Result<Option<Value>, String> {
    let err = |e: sqlx::Error| format!("load chat: {e}");
    let mut tx = pool.begin().await.map_err(err)?;
    let Some(r) = sqlx::query(&format!("SELECT {CHAT_COLS} FROM chats WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
    else {
        return Ok(None);
    };
    let msgs = sqlx::query(
        "SELECT role, content, thinking, model, images, gen_ms, gen_tps, summary, gen_cost FROM messages WHERE chat_id = $1 ORDER BY idx",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(Some(chat_json(&r, msgs.iter().map(message_json).collect())))
}

/// The server's own writer (Work turns recorded while no window may be
/// open): replace one chat's messages, bump its revision and optionally its
/// activity time. Returns the new revision; errors when the chat is gone
/// (deleted mid-turn), so nothing resurrects it.
pub async fn write_messages(pool: &SqlitePool, id: &str, messages: &[Value], updated_at: Option<i64>) -> Result<i64, String> {
    let err = |e: sqlx::Error| format!("record turn: {e}");
    let rows: Vec<crate::MsgSaveRow> =
        serde_json::from_value(Value::Array(messages.to_vec())).map_err(|e| format!("record turn: {e}"))?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(err)?;
    let rev: Option<Option<i64>> = sqlx::query_scalar("SELECT rev FROM chats WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?;
    let Some(rev) = rev else {
        return Err(format!("record turn: session {id} no longer exists"));
    };
    let rev = rev.unwrap_or(0) + 1;
    insert_messages(&mut tx, id, &rows).await.map_err(err)?;
    sqlx::query("UPDATE chats SET rev = $2, updated_at = COALESCE($3, updated_at), server_recorded = 1 WHERE id = $1")
        .bind(id)
        .bind(rev)
        .bind(updated_at)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(rev)
}

/// Create a chat the server starts on its own (a scheduled send into a new
/// session). Same payload shape as a client save row.
pub async fn create_chat(pool: &SqlitePool, chat: &Value) -> Result<(), String> {
    merge_chats(pool, parse_rows(&Value::Array(vec![chat.clone()]))?, vec![], &Default::default()).await.map(|_| ())
}

pub async fn meta_get(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM meta WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("meta: {e}"))
}

pub async fn meta_put(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query("INSERT OR REPLACE INTO meta (key, value) VALUES ($1, $2)")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| format!("meta: {e}"))
}

/// Escape a user string for a SQLite `LIKE … ESCAPE '\'` pattern.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// A display excerpt around the first match (or the opening user message).
fn excerpt(text: &str, q: &str) -> String {
    let flat = text.replace(['\r', '\n'], " ");
    let flat = flat.trim();
    if q.is_empty() {
        return flat.chars().take(160).collect();
    }
    let low = flat.to_lowercase();
    let Some(idx) = low.find(&q.to_lowercase()) else {
        return flat.chars().take(160).collect();
    };
    let chars: Vec<char> = flat.chars().collect();
    // byte index → char index (the match may follow multi-byte text)
    let start_char = flat[..idx].chars().count();
    let len = q.chars().count();
    let from = start_char.saturating_sub(40);
    let to = (start_char + len + 80).min(chars.len());
    let body: String = chars[from..to].iter().collect();
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.push_str(&body);
    if to < chars.len() {
        out.push('…');
    }
    out
}

/// One chat's snippet: the message that matches `q`, else its first user
/// message, else its first message.
async fn chat_snippet(pool: &SqlitePool, chat_id: &str, q: &str) -> Result<String, String> {
    let err = |e: sqlx::Error| format!("search chats: {e}");
    if !q.is_empty() {
        let like = format!("%{}%", escape_like(q));
        let hit: Option<String> = sqlx::query_scalar(
            "SELECT content FROM messages WHERE chat_id = $1 AND content LIKE $2 ESCAPE '\\' ORDER BY idx LIMIT 1",
        )
        .bind(chat_id)
        .bind(&like)
        .fetch_optional(pool)
        .await
        .map_err(err)?;
        if let Some(c) = hit.filter(|c| !c.trim().is_empty()) {
            return Ok(excerpt(&c, q));
        }
    }
    let first: Option<String> = sqlx::query_scalar(
        "SELECT content FROM messages WHERE chat_id = $1 AND role = 'user' ORDER BY idx LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(pool)
    .await
    .map_err(err)?;
    let first = match first.filter(|c| !c.trim().is_empty()) {
        Some(c) => Some(c),
        None => sqlx::query_scalar("SELECT content FROM messages WHERE chat_id = $1 ORDER BY idx LIMIT 1")
            .bind(chat_id)
            .fetch_optional(pool)
            .await
            .map_err(err)?,
    };
    Ok(excerpt(first.as_deref().unwrap_or(""), ""))
}

async fn chat_hit(pool: &SqlitePool, r: &sqlx::sqlite::SqliteRow, q: &str) -> Result<Value, String> {
    let id: String = r.try_get("id").unwrap_or_default();
    let created: i64 = r.try_get("created_at").unwrap_or(0);
    Ok(json!({
        "id": id,
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "updatedAt": r.try_get::<Option<i64>, _>("updated_at").ok().flatten().unwrap_or(created),
        "mode": if r.try_get::<Option<String>, _>("mode").ok().flatten().as_deref() == Some("work") { "work" } else { "chat" },
        "workspace": r.try_get::<Option<String>, _>("workspace").ok().flatten(),
        "snippet": chat_snippet(pool, &r.try_get::<String, _>("id").unwrap_or_default(), q).await?,
    }))
}

const HIT_COLS: &str = "id, title, created_at, updated_at, mode, workspace";

/// Lightweight cross-host search used by the Cmd+P palette of another window:
/// chats whose id, title or message text matches `query`, plus the chats named
/// by `ids` (a pasted session link). Display rows only — no message bodies.
pub async fn search_chats(pool: &SqlitePool, query: &str, ids: &[String], limit: i64) -> Result<Value, String> {
    let limit = limit.clamp(1, 50);
    let err = |e: sqlx::Error| format!("search chats: {e}");
    let q = query.trim();
    let mut hits: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for id in ids.iter().take(10) {
        let row = sqlx::query(&format!("SELECT {HIT_COLS} FROM chats WHERE lower(id) = lower($1)"))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(err)?;
        if let Some(r) = row {
            let hit = chat_hit(pool, &r, "").await?;
            seen.push(hit["id"].as_str().unwrap_or_default().to_string());
            hits.push(hit);
        }
    }
    if !q.is_empty() {
        let like = format!("%{}%", escape_like(q));
        let rows = sqlx::query(&format!(
            "SELECT {HIT_COLS} FROM chats c WHERE c.id LIKE $1 ESCAPE '\\' OR c.title LIKE $1 ESCAPE '\\' \
             OR EXISTS (SELECT 1 FROM messages m WHERE m.chat_id = c.id AND m.content LIKE $1 ESCAPE '\\') \
             ORDER BY COALESCE(c.updated_at, c.created_at) DESC LIMIT $2"
        ))
        .bind(&like)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(err)?;
        for r in &rows {
            let id: String = r.try_get("id").unwrap_or_default();
            if seen.contains(&id) {
                continue;
            }
            hits.push(chat_hit(pool, r, q).await?);
            seen.push(id);
        }
    }
    Ok(Value::Array(hits))
}

/// Merge the frontend's save payload (same shape as `chats_save`).
pub async fn merge_json(
    pool: &SqlitePool,
    chats: &Value,
    deleted: Vec<String>,
    live: &std::collections::HashSet<String>,
) -> Result<MergeOutcome, String> {
    merge_chats(pool, parse_rows(chats)?, deleted, live).await
}

/// Parse the frontend's save payload (same shape as `chats_save`).
pub(crate) fn parse_rows(v: &Value) -> Result<Vec<crate::ChatSaveRow>, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("bad chats payload: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, title: &str, msgs: &[(&str, &str)]) -> Value {
        json!({
            "id": id, "title": title, "createdAt": 1, "updatedAt": 2, "mode": "work", "workspace": "/srv/p",
            "messages": msgs.iter().map(|(r, c)| json!({ "role": r, "content": c, "thinking": "", "images": null })).collect::<Vec<_>>(),
        })
    }

    #[tokio::test]
    async fn merge_never_touches_other_clients_chats() {
        let dir = std::env::temp_dir().join(format!("vicuna-server-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = open(&dir.join("vicuna.db")).await.unwrap();

        // client A creates a, client B creates b
        merge_chats(&pool, parse_rows(&json!([row("a", "A", &[("user", "hi")])])).unwrap(), vec![], &Default::default()).await.unwrap();
        merge_chats(&pool, parse_rows(&json!([row("b", "B", &[("user", "yo")])])).unwrap(), vec![], &Default::default()).await.unwrap();
        // A edits a (adds a reply) without knowing about b
        merge_chats(&pool, parse_rows(&json!([row("a", "A2", &[("user", "hi"), ("assistant", "hello")])])).unwrap(), vec![], &Default::default()).await.unwrap();

        let all = load_chats(&pool).await.unwrap();
        let ids: Vec<&str> = all.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        let a = all.as_array().unwrap().iter().find(|c| c["id"] == "a").unwrap();
        assert_eq!(a["title"], "A2");
        assert_eq!(a["messages"].as_array().unwrap().len(), 2);
        assert_eq!(a["mode"], "work");
        assert_eq!(a["workspace"], "/srv/p");

        // archiving rides in `extra` and survives a merge untouched
        let mut ar = row("a", "A2", &[("user", "hi"), ("assistant", "hello")]);
        ar["extra"] = json!(r#"{"archived":true}"#);
        merge_chats(&pool, parse_rows(&json!([ar])).unwrap(), vec![], &Default::default()).await.unwrap();
        let all = load_chats(&pool).await.unwrap();
        let a = all.as_array().unwrap().iter().find(|c| c["id"] == "a").unwrap();
        assert!(a["extra"].as_str().unwrap_or("").contains("\"archived\":true"), "{a}");
        assert_eq!(a["messages"].as_array().unwrap().len(), 2);

        // B deletes b; a stays
        merge_chats(&pool, vec![], vec!["b".into()], &Default::default()).await.unwrap();
        let all = load_chats(&pool).await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn backup_snapshots_a_live_wal_db() {
        let dir = std::env::temp_dir().join(format!("vicuna-server-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = open(&dir.join("vicuna.db")).await.unwrap();
        merge_chats(&pool, parse_rows(&json!([row("a", "A", &[("user", "hi")])])).unwrap(), vec![], &Default::default()).await.unwrap();
        // pool stays open: the row may still live only in the -wal file
        let dest = dir.join("bk/vicuna.db");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        backup_to(&dir.join("vicuna.db"), &dest).await.unwrap();
        assert!(backup_to(&dir.join("vicuna.db"), &dest).await.is_err(), "never overwrites");
        let copy = open(&dest).await.unwrap();
        let all = load_chats(&copy).await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 1);
        assert_eq!(all[0]["messages"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_finds_titles_ids_and_message_text() {
        let dir = std::env::temp_dir().join(format!("vicuna-server-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = open(&dir.join("vicuna.db")).await.unwrap();
        merge_chats(
            &pool,
            parse_rows(&json!([
                row("id-alpha", "Deploy notes", &[("user", "the haystack needle lives here")]),
                row("id-beta", "Groceries", &[("user", "milk and eggs")]),
            ]))
            .unwrap(),
            vec![],
            &Default::default(),
        )
        .await
        .unwrap();

        // title match
        let hits = search_chats(&pool, "deploy", &[], 10).await.unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
        assert_eq!(hits[0]["id"], "id-alpha");
        assert_eq!(hits[0]["title"], "Deploy notes");
        // message-text match carries an excerpt around the hit
        let hits = search_chats(&pool, "NEEDLE", &[], 10).await.unwrap();
        assert_eq!(hits[0]["id"], "id-alpha");
        assert!(hits[0]["snippet"].as_str().unwrap().contains("needle"), "{hits}");
        // pasted session id wins, even with no matching text
        let hits = search_chats(&pool, "", &["ID-BETA".to_string()], 10).await.unwrap();
        assert_eq!(hits[0]["id"], "id-beta");
        assert_eq!(hits[0]["mode"], "work");
        assert_eq!(hits[0]["workspace"], "/srv/p");
        // LIKE wildcards are literal, not patterns
        let hits = search_chats(&pool, "%", &[], 10).await.unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 0, "{hits}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stale_client_save_never_overwrites_server_rows() {
        let dir = std::env::temp_dir().join(format!("vicuna-server-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = open(&dir.join("vicuna.db")).await.unwrap();
        // window saves the turn start: user + empty placeholder (rev 1)
        let out = merge_chats(&pool, parse_rows(&json!([row("w", "W", &[("user", "go"), ("assistant", "")])])).unwrap(), vec![], &Default::default())
            .await
            .unwrap();
        assert_eq!(out.revs["w"], 1);
        // the server records the turn while the window sleeps (rev 2)
        let full = vec![json!({"role": "user", "content": "go"}), json!({"role": "assistant", "content": "all done", "model": "opencode"})];
        assert_eq!(write_messages(&pool, "w", &full, None).await.unwrap(), 2);
        // a live turn refuses every client write, even with the right rev
        let mut live = std::collections::HashSet::new();
        live.insert("w".to_string());
        let mut cur = row("w", "W", &[("user", "go"), ("assistant", "x")]);
        cur["baseRev"] = json!(2);
        let out = merge_chats(&pool, parse_rows(&json!([cur])).unwrap(), vec![], &live).await.unwrap();
        assert!(out.revs.is_empty());
        // the window wakes with its old copy (baseRev 1): refused, gets the server copy
        let mut stale = row("w", "W", &[("user", "go"), ("assistant", "")]);
        stale["baseRev"] = json!(1);
        let out = merge_chats(&pool, parse_rows(&json!([stale])).unwrap(), vec![], &Default::default()).await.unwrap();
        assert!(out.revs.is_empty());
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(out.conflicts[0]["rev"], 2);
        assert_eq!(out.conflicts[0]["messages"][1]["content"], "all done");
        let c = load_chat(&pool, "w").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], "all done", "stale save must not clobber");
        // after adopting rev 2 the window's next save goes through
        let mut fresh = row("w", "W renamed", &[("user", "go"), ("assistant", "all done")]);
        fresh["baseRev"] = json!(2);
        let out = merge_chats(&pool, parse_rows(&json!([fresh])).unwrap(), vec![], &Default::default()).await.unwrap();
        assert_eq!(out.revs["w"], 3);
        // Old windows do not send revisions at all. They must not bypass
        // protection, even after a current window has renamed the chat.
        let old = row("w", "W", &[("user", "go"), ("assistant", "")]);
        let out = merge_chats(&pool, parse_rows(&json!([old])).unwrap(), vec![], &Default::default()).await.unwrap();
        assert!(out.revs.is_empty());
        assert_eq!(out.conflicts[0]["messages"][1]["content"], "all done");
        // a deleted session is never resurrected by a late recorder write
        merge_chats(&pool, vec![], vec!["w".into()], &Default::default()).await.unwrap();
        assert!(write_messages(&pool, "w", &full, None).await.is_err());
        assert!(load_chat(&pool, "w").await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recorded_rows_survive_a_server_restart() {
        let dir = std::env::temp_dir().join(format!("vicuna-server-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("vicuna.db");
        {
            let pool = open(&path).await.unwrap();
            merge_chats(&pool, parse_rows(&json!([row("r", "R", &[("user", "go"), ("assistant", "")])])).unwrap(), vec![], &Default::default())
                .await
                .unwrap();
            let partial = vec![json!({"role": "user", "content": "go"}), json!({"role": "assistant", "content": "half way", "thinking": "hmm"})];
            write_messages(&pool, "r", &partial, Some(99)).await.unwrap();
            pool.close().await; // the server dies mid-turn
        }
        let pool = open(&path).await.unwrap();
        let c = load_chat(&pool, "r").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], "half way");
        assert_eq!(c["messages"][1]["thinking"], "hmm");
        assert_eq!(c["updatedAt"], 99);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
