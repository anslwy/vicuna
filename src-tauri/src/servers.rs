// Remote Vicuna servers (desktop client side).
//
// The sidebar's server picker switches the whole frontend between this
// computer's backend (Tauri IPC) and a headless `vicuna serve` on
// another machine (typically a VPS behind `tailscale serve`). When a server
// is active, src/lib/bridge.ts routes every backend command here and this
// module forwards it over HTTP(S) — from Rust, so the webview never deals
// with CORS/mixed content and the token never returns to JS after connect.
//
// Recents live in `~/vicuna/config/servers.json` (0600): id, url, name,
// token, lastUsed.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerEntry {
    pub id: String,
    pub url: String,
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub last_used: i64,
}

/// What the frontend sees (never the token).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub id: String,
    pub url: String,
    pub name: String,
    pub last_used: i64,
}

impl From<&ServerEntry> for ServerInfo {
    fn from(s: &ServerEntry) -> Self {
        ServerInfo { id: s.id.clone(), url: s.url.clone(), name: s.name.clone(), last_used: s.last_used }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn path() -> Option<PathBuf> {
    crate::opencode::home_dir().map(|h| h.join("vicuna").join("config").join("servers.json"))
}

fn load() -> Vec<ServerEntry> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(list: &[ServerEntry]) -> Result<(), String> {
    let p = path().ok_or("no home folder")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn random_id() -> String {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).expect("os randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `https://host:8443/`, `host:port`, or either with `#token` → (base URL, token).
/// A bare `*.ts.net` host defaults to https (that's what `tailscale serve`
/// gives you); anything else bare defaults to http.
pub fn parse_server_url(input: &str) -> Result<(String, Option<String>), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter the server URL, e.g. https://my-vps.tailnet.ts.net:8443".into());
    }
    let (addr, token) = match input.split_once('#') {
        Some((a, t)) => (a.trim(), Some(t.trim().to_string()).filter(|t| !t.is_empty())),
        None => (input, None),
    };
    let mut url = if addr.contains("://") {
        addr.to_string()
    } else {
        let host = addr.split([':', '/']).next().unwrap_or("");
        let scheme = if host.ends_with(".ts.net") { "https" } else { "http" };
        format!("{scheme}://{addr}")
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("Only http:// and https:// servers are supported.".into());
    }
    while url.ends_with('/') {
        url.pop();
    }
    let host_part = url.split("://").nth(1).unwrap_or("");
    if host_part.is_empty() || host_part.contains('/') {
        return Err("Use just the server address, without a path.".into());
    }
    Ok((url, token))
}

/// Display name: host[:port] (what the picker shows).
fn short_name(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).to_string()
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())
}

fn transport_error(url: &str, e: reqwest::Error) -> String {
    if e.is_timeout() {
        format!("{url} did not answer in time.")
    } else if e.is_connect() {
        format!("Cannot reach {url}. Is `vicuna serve` running there?")
    } else {
        format!("{url}: {e}")
    }
}

/// One HTTP call to a server. `body` = POST json, else GET.
pub async fn request(url: &str, token: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, String> {
    let c = client(timeout)?;
    let full = format!("{url}{path}");
    let rb = match body {
        Some(b) => c.post(&full).json(b),
        None => c.get(&full),
    };
    let res = rb.bearer_auth(token).send().await.map_err(|e| transport_error(url, e))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status.as_u16() == 401 {
        return Err("The server rejected the token. Reconnect with its current token.".into());
    }
    if !status.is_success() {
        return Err(v.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("HTTP {status}")));
    }
    Ok(v)
}

pub fn list() -> Vec<ServerInfo> {
    let mut l = load();
    l.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    l.iter().map(ServerInfo::from).collect()
}

/// Verify (`server_info` through the authed invoke route), then save/update
/// by URL and mark it most recent.
pub async fn connect(url_input: &str, token: Option<String>) -> Result<ServerInfo, String> {
    let (url, frag) = parse_server_url(url_input)?;
    let existing = load().into_iter().find(|s| s.url == url);
    let token = token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(frag)
        .or(existing.as_ref().map(|s| s.token.clone()))
        .ok_or("Enter the server's token (printed by `vicuna serve`).")?;
    let info = request(&url, &token, "/invoke", Some(&json!({ "cmd": "server_info", "args": {} })), Duration::from_secs(15)).await?;
    if info.get("ok").and_then(Value::as_bool) != Some(true) {
        let e = info.get("error").and_then(Value::as_str).unwrap_or("not a Vicuna server");
        return Err(if e.contains("unknown bridge command") {
            "That bridge is too old or not in server mode. Run `vicuna serve` there.".into()
        } else {
            e.to_string()
        });
    }
    let mut l = load();
    let entry = match l.iter_mut().find(|s| s.url == url) {
        Some(s) => {
            s.token = token;
            s.last_used = now_ms();
            s.name = short_name(&url);
            s.clone()
        }
        None => {
            let s = ServerEntry { id: random_id(), name: short_name(&url), url, token, last_used: now_ms() };
            l.push(s.clone());
            s
        }
    };
    save(&l)?;
    Ok(ServerInfo::from(&entry))
}

pub fn touch(id: &str) -> Result<(), String> {
    let mut l = load();
    if let Some(s) = l.iter_mut().find(|s| s.id == id) {
        s.last_used = now_ms();
    }
    save(&l)
}

pub fn remove(id: &str) -> Result<(), String> {
    let mut l = load();
    l.retain(|s| s.id != id);
    save(&l)
}

/// Forward one call to server `id`: `path` is `/invoke` (with body) or
/// `/poll/<channel>` (GET). Returns the server's JSON as-is.
pub async fn call(id: &str, path: &str, body: Option<Value>, timeout_ms: Option<u64>) -> Result<Value, String> {
    if !(path == "/invoke" || path.starts_with("/poll/")) {
        return Err("unsupported server path".into());
    }
    let s = load().into_iter().find(|s| s.id == id).ok_or("That server is no longer in your list.")?;
    let timeout = timeout_ms.map(Duration::from_millis).unwrap_or(DEFAULT_TIMEOUT);
    request(&s.url, &s.token, path, body.as_ref(), timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse() {
        assert_eq!(parse_server_url("https://vps.tail1.ts.net:8443/").unwrap(), ("https://vps.tail1.ts.net:8443".into(), None));
        assert_eq!(parse_server_url("vps.tail1.ts.net:8443").unwrap().0, "https://vps.tail1.ts.net:8443");
        assert_eq!(parse_server_url("127.0.0.1:6767#tok").unwrap(), ("http://127.0.0.1:6767".into(), Some("tok".into())));
        assert!(parse_server_url("https://x/y").is_err());
        assert!(parse_server_url("ftp://x").is_err());
        assert!(parse_server_url(" ").is_err());
        assert_eq!(short_name("https://vps.tail1.ts.net:8443"), "vps.tail1.ts.net:8443");
    }

    /// Live round trip against a running `vicuna serve` (opt-in):
    ///   HOME=$(mktemp -d) VICUNA_TEST_SERVER_URL=http://127.0.0.1:7421 \
    ///   VICUNA_TEST_SERVER_TOKEN=... cargo test --lib live_server -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_server_connect_call_poll() {
        let url = std::env::var("VICUNA_TEST_SERVER_URL").expect("VICUNA_TEST_SERVER_URL");
        let token = std::env::var("VICUNA_TEST_SERVER_TOKEN").expect("VICUNA_TEST_SERVER_TOKEN");
        let bad = connect(&url, Some("wrong".into())).await.unwrap_err();
        assert!(bad.contains("rejected"), "{bad}");
        let sv = connect(&format!("{url}/"), Some(token.clone())).await.unwrap();
        assert_eq!(list().len(), 1);
        // reconnect without a token reuses the saved one
        let again = connect(&url, None).await.unwrap();
        assert_eq!(again.id, sv.id);
        let r = call(&sv.id, "/invoke", Some(json!({ "cmd": "chats_load", "args": {} })), None).await.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert!(r["data"].is_array());
        let p = call(&sv.id, "/poll/nonexistent", None, Some(5000)).await.unwrap();
        assert!(p["events"].is_array());
        assert!(call(&sv.id, "/etc/passwd", None, None).await.is_err());
        remove(&sv.id).unwrap();
        assert!(list().is_empty());
    }
}
