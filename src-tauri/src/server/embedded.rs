// The desktop app's backend: the same server `vicuna serve` runs, started
// inside the app process — or, when a `vicuna serve` already runs on this
// machine (port 7421, same ~/vicuna/server/token), that one is used instead.
// The main window then talks to it exactly like a server window talks to a
// remote server (`server_call` with the reserved id "local"), and the CLI's
// default (`vicuna session …` → 127.0.0.1:7421) reaches the very same
// sessions. One backend, two frontends.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::{server_dir, server_token, BridgeState, Config};

/// The port `vicuna serve` and the CLI default to.
pub const DEFAULT_PORT: u16 = 7421;

/// Where the main window's backend lives.
#[derive(Clone, Debug)]
pub struct LocalBackend {
    pub url: String,
    pub token: String,
    /// false = an already-running `vicuna serve` was adopted
    pub embedded: bool,
}

static LOCAL: Mutex<Option<Result<LocalBackend, String>>> = Mutex::new(None);
static EMBEDDED_STATE: OnceLock<Arc<BridgeState>> = OnceLock::new();

/// The local backend once known (None while starting).
pub fn local() -> Option<Result<LocalBackend, String>> {
    LOCAL.lock().ok().and_then(|g| g.clone())
}

/// Wait (up to `timeout`) for the local backend to be up.
pub async fn wait_local(timeout: Duration) -> Result<LocalBackend, String> {
    let end = std::time::Instant::now() + timeout;
    loop {
        if let Some(r) = local() {
            return r;
        }
        if std::time::Instant::now() > end {
            return Err("Vicuna's local backend is still starting".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn set_local(r: Result<LocalBackend, String>) {
    if let Ok(mut g) = LOCAL.lock() {
        *g = Some(r);
    }
}

/// The embedded server's state (Quit kills its children).
pub fn embedded_state() -> Option<Arc<BridgeState>> {
    EMBEDDED_STATE.get().cloned()
}

/// Is a Vicuna server answering on `url` with `token`?
async fn probe(url: &str, token: &str) -> bool {
    let Ok(c) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else { return false };
    let Ok(r) = c.post(format!("{url}/invoke")).bearer_auth(token).json(&json!({ "cmd": "server_info", "args": {} })).send().await else {
        return false;
    };
    let v: Value = r.json().await.unwrap_or(Value::Null);
    v.get("ok").and_then(Value::as_bool) == Some(true) && v["data"]["persist"] == json!(true)
}

/// Use a running `vicuna serve` on 7421, else start ours (7421, or any free
/// port when 7421 is taken by something else). Records the result for
/// `server_call("local")` and writes `~/vicuna/server/port` for the CLI.
pub async fn start(resource_dir: Option<PathBuf>) -> Result<LocalBackend, String> {
    let r = start_inner(resource_dir).await;
    if let Ok(b) = &r {
        if let Some(port) = b.url.rsplit(':').next() {
            let _ = std::fs::create_dir_all(server_dir());
            let _ = std::fs::write(server_dir().join("port"), port);
        }
    }
    r
}

/// Desktop startup: start (or adopt) the backend, import the desktop's own
/// chats once, and only then publish it to `server_call("local")`, so the
/// window's first load already lists the imported chats.
pub async fn launch(resource_dir: Option<PathBuf>, desktop_db: Option<PathBuf>) {
    let r = start(resource_dir).await;
    match &r {
        Ok(b) => {
            if let Some(db) = desktop_db {
                match import_desktop_chats(db, b).await {
                    Ok(0) => {}
                    Ok(n) => eprintln!("vicuna: imported {n} chats into {}", b.url),
                    Err(e) => eprintln!("vicuna: chat import failed: {e}"),
                }
            }
        }
        Err(e) => eprintln!("vicuna: local backend failed to start: {e}"),
    }
    set_local(r);
}

/// 7421, or `VICUNA_LOCAL_PORT` (tests: never collide with a real server).
fn default_port() -> u16 {
    std::env::var("VICUNA_LOCAL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

async fn start_inner(resource_dir: Option<PathBuf>) -> Result<LocalBackend, String> {
    let token = server_token();
    let default_url = format!("http://127.0.0.1:{}", default_port());
    if probe(&default_url, &token).await {
        return Ok(LocalBackend { url: default_url, token, embedded: false });
    }
    let mut cfg = Config {
        bind: "127.0.0.1".into(),
        port: default_port(),
        token: Some(token.clone()),
        db: Some(server_dir().join("vicuna.db")),
        resource_dir,
        ..Default::default()
    };
    let bound = match super::bind(&cfg).await {
        Ok(b) => b,
        Err(e) if e.contains("bind") => {
            // 7421 is someone else's (or a server with another token)
            cfg.port = 0;
            super::bind(&cfg).await?
        }
        Err(e) => return Err(e),
    };
    let url = format!("http://{}", bound.addr);
    let _ = EMBEDDED_STATE.set(bound.state.clone());
    tokio::spawn(bound.serve());
    Ok(LocalBackend { url, token, embedded: true })
}

/// Quit: end everything the embedded server started (terminals, background
/// jobs, agent shells, its browser daemon). An adopted `vicuna serve` keeps
/// running — it is not ours.
pub fn shutdown() {
    let Some(st) = embedded_state() else { return };
    super::shutdown_state(&st);
}

// ---------- one-time import of the desktop's own chats ----------

fn import_marker(url: &str) -> Option<PathBuf> {
    let home = crate::opencode::home_dir()?;
    let tag: String = url.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    Some(home.join("vicuna").join("config").join(format!("desktop-import-{tag}.done")))
}

/// Copy the chats the desktop kept in its own sqlite (`desktop_db`, the
/// app-config `vicuna.db`) into the local backend, once per backend. Only ids
/// the server doesn't have are sent, so nothing there is overwritten; the
/// desktop file is read, never written. Returns how many were imported.
pub async fn import_desktop_chats(desktop_db: PathBuf, backend: &LocalBackend) -> Result<usize, String> {
    let Some(marker) = import_marker(&backend.url) else { return Ok(0) };
    if marker.exists() || !desktop_db.exists() {
        return Ok(0);
    }
    let local = crate::server_store::load_desktop_chats(&desktop_db).await?;
    let call = |cmd: &str, args: Value| {
        let (url, token) = (backend.url.clone(), backend.token.clone());
        let body = json!({ "cmd": cmd, "args": args });
        async move { crate::servers::request(&url, &token, "/invoke", Some(&body), Duration::from_secs(120)).await }
    };
    let have = call("chats_load", json!({})).await?;
    let have_ids: std::collections::HashSet<&str> = have["data"]
        .as_array()
        .filter(|_| have["ok"] == true)
        .ok_or_else(|| have["error"].as_str().unwrap_or("Invalid chats_load response during desktop import").to_string())?
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    let rows = desktop_import_rows(local, &have_ids);
    let n = rows.len();
    // in batches: one huge body could pass the server's 32 MB cap
    for chunk in rows.chunks(50) {
        let r = call("chats_merge", json!({ "chats": chunk, "deleted": [] })).await?;
        if r.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(r.get("error").and_then(Value::as_str).unwrap_or("import failed").to_string());
        }
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, format!("{n} chats imported from {}\n", desktop_db.display()));
    Ok(n)
}

fn desktop_import_rows(local: Value, have_ids: &std::collections::HashSet<&str>) -> Vec<Value> {
    local
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["id"].as_str().is_some_and(|id| !have_ids.contains(id)))
        .cloned()
        .map(|mut c| {
            // load_chats yields forkedFrom as an object, save rows take JSON
            // text. Send text so an adopted older server (0.0.23 rejected the
            // object: "invalid type: map") still accepts forked chats.
            if c["forkedFrom"].is_object() {
                c["forkedFrom"] = Value::String(c["forkedFrom"].to_string());
            }
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn desktop_import_reads_old_schema_and_sends_legacy_fork_format() {
        let path = std::env::temp_dir().join(format!("vicuna-desktop-import-{}.db", std::process::id()));
        let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE chats (id TEXT, title TEXT, created_at INTEGER, forked_from TEXT);
             CREATE TABLE messages (chat_id TEXT, role TEXT, content TEXT, idx INTEGER);
             INSERT INTO chats VALUES ('parent', 'Parent', 1, NULL);
             INSERT INTO chats VALUES ('fork', 'Fork', 2, '{\"id\":\"parent\"}');
             INSERT INTO messages VALUES ('fork', 'user', 'hello', 0);",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let original = std::fs::read(&path).unwrap();

        let loaded = crate::server_store::load_desktop_chats(&path).await.unwrap();
        let rows = desktop_import_rows(loaded, &["parent"].into_iter().collect());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "fork");
        assert_eq!(rows[0]["forkedFrom"].as_str(), Some(r#"{"id":"parent"}"#));
        assert_eq!(rows[0]["messages"][0]["content"], "hello");
        assert!(std::fs::read(&path).unwrap() == original, "source database was changed");
        std::fs::remove_file(path).unwrap();
    }

    /// The desktop startup path end to end, isolated: temp HOME, a foreign
    /// occupant on the "default" port. Must start its own server elsewhere,
    /// write the port file, import the desktop's chats once, never twice.
    /// Changes HOME for the process, so it is opt-in and must run alone:
    /// `cargo test --no-default-features --features rustls --lib embedded -- --ignored --test-threads=1`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn embedded_start_falls_back_and_imports_once() {
        let home = std::env::temp_dir().join(format!("vicuna-embedded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: opt-in test run alone (--test-threads=1)
        unsafe { std::env::set_var("HOME", &home); }
        // SAFETY: opt-in test run alone (--test-threads=1)
        unsafe { std::env::remove_var("VICUNA_SERVER_TOKEN"); }
        // something that is not a Vicuna server holds the default port
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = squatter.local_addr().unwrap().port();
        // SAFETY: opt-in test run alone (--test-threads=1)
        unsafe { std::env::set_var("VICUNA_LOCAL_PORT", busy.to_string()); }
        // the desktop's own sqlite with one chat
        let desktop_db = home.join("desktop.db");
        let pool = crate::server_store::open(&desktop_db).await.unwrap();
        crate::server_store::create_chat(
            &pool,
            &json!({"id":"desk-1","title":"from desktop","createdAt":1,"updatedAt":1,"mode":"work","workspace":"/tmp","messages":[{"role":"user","content":"hi"}]}),
        )
        .await
        .unwrap();
        pool.close().await;

        let b = start(None).await.expect("embedded start");
        assert!(b.embedded);
        assert!(!b.url.ends_with(&format!(":{busy}")), "fell back from the busy port: {}", b.url);
        let port = std::fs::read_to_string(server_dir().join("port")).unwrap();
        assert!(b.url.ends_with(&format!(":{}", port.trim())));
        assert!(probe(&b.url, &b.token).await, "authed server_info answers");
        assert!(!probe(&b.url, "wrong").await, "wrong token refused");

        assert_eq!(import_desktop_chats(desktop_db.clone(), &b).await.unwrap(), 1);
        assert_eq!(import_desktop_chats(desktop_db.clone(), &b).await.unwrap(), 0, "once per backend");
        let r = crate::servers::request(&b.url, &b.token, "/invoke", Some(&json!({"cmd":"chats_load","args":{}})), Duration::from_secs(10)).await.unwrap();
        assert!(r["data"].as_array().unwrap().iter().any(|c| c["id"] == "desk-1" && c["title"] == "from desktop"), "{r}");
        drop(squatter);
        let _ = std::fs::remove_dir_all(&home);
    }
}
