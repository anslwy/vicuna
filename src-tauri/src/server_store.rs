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
const CHAT_MIGRATIONS: &[&str] = &["forked_from TEXT", "fork_from_index INTEGER", "model TEXT", "mode TEXT", "workspace TEXT", "goal TEXT", "updated_at INTEGER"];
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

const CHAT_COLS: &str = "id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at";

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
    let chats = sqlx::query(&format!("SELECT {CHAT_COLS} FROM chats ORDER BY COALESCE(updated_at, created_at) DESC"))
        .fetch_all(pool)
        .await
        .map_err(|e| format!("load chats: {e}"))?;
    let msgs = sqlx::query(
        "SELECT chat_id, role, content, thinking, model, images, gen_ms, gen_tps, summary, gen_cost FROM messages ORDER BY chat_id, idx",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load messages: {e}"))?;
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

/// Replace the given chats (row + every message) and delete `deleted`, in one
/// transaction. Chats not mentioned are left alone (other clients' work).
pub(crate) async fn merge_chats(pool: &SqlitePool, chats: Vec<crate::ChatSaveRow>, deleted: Vec<String>) -> Result<(), String> {
    let err = |e: sqlx::Error| format!("save chats: {e}");
    let mut tx = pool.begin().await.map_err(err)?;
    for id in &deleted {
        sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(id).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("DELETE FROM chats WHERE id = $1").bind(id).execute(&mut *tx).await.map_err(err)?;
    }
    for chat in chats {
        if chat.id.is_empty() {
            continue;
        }
        sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(&chat.id).execute(&mut *tx).await.map_err(err)?;
        sqlx::query(
            "INSERT OR REPLACE INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        for (i, m) in chat.messages.iter().enumerate() {
            let images = m.images.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default());
            sqlx::query(
                "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(&chat.id)
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
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        }
    }
    tx.commit().await.map_err(err)
}

/// Merge the frontend's save payload (same shape as `chats_save`).
pub async fn merge_json(pool: &SqlitePool, chats: &Value, deleted: Vec<String>) -> Result<(), String> {
    merge_chats(pool, parse_rows(chats)?, deleted).await
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
        merge_chats(&pool, parse_rows(&json!([row("a", "A", &[("user", "hi")])])).unwrap(), vec![]).await.unwrap();
        merge_chats(&pool, parse_rows(&json!([row("b", "B", &[("user", "yo")])])).unwrap(), vec![]).await.unwrap();
        // A edits a (adds a reply) without knowing about b
        merge_chats(&pool, parse_rows(&json!([row("a", "A2", &[("user", "hi"), ("assistant", "hello")])])).unwrap(), vec![]).await.unwrap();

        let all = load_chats(&pool).await.unwrap();
        let ids: Vec<&str> = all.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        let a = all.as_array().unwrap().iter().find(|c| c["id"] == "a").unwrap();
        assert_eq!(a["title"], "A2");
        assert_eq!(a["messages"].as_array().unwrap().len(), 2);
        assert_eq!(a["mode"], "work");
        assert_eq!(a["workspace"], "/srv/p");

        // B deletes b; a stays
        merge_chats(&pool, vec![], vec!["b".into()]).await.unwrap();
        let all = load_chats(&pool).await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
