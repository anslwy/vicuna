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

/// Copy a file from a server's workspace into `dest_dir` (~/Downloads) in
/// 4MB base64 chunks over `/invoke work_read_chunk`. A clashing name gets
/// ` (N)` appended; a failed transfer removes the partial file.
pub async fn download_file(id: &str, root: &str, path: &str, dest_dir: &std::path::Path) -> Result<String, String> {
    use base64::Engine as _;
    use std::io::Write as _;
    const CHUNK: u64 = 4 * 1024 * 1024;
    let name = std::path::Path::new(path.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or("invalid file name")?
        .to_string();
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("download failed: {e}"))?;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut dest = dest_dir.join(&name);
    let mut n = 1;
    while dest.exists() {
        dest = dest_dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    let mut file = std::fs::File::create(&dest).map_err(|e| format!("download failed: {e}"))?;
    let result: Result<(), String> = async {
        let mut offset = 0u64;
        loop {
            let body = serde_json::json!({
                "cmd": "work_read_chunk",
                "args": {"root": root, "path": path, "offset": offset, "length": CHUNK},
            });
            let v = call(id, "/invoke", Some(body), Some(120_000)).await.map_err(|e| {
                if e.contains("unknown bridge command") {
                    "The server's Vicuna is too old to download files; update it.".to_string()
                } else {
                    e
                }
            })?;
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                let msg = v.get("error").and_then(Value::as_str).unwrap_or("download failed");
                if msg.contains("unknown bridge command") {
                    return Err("The server's Vicuna is too old to download files; update it.".into());
                }
                return Err(msg.to_string());
            }
            let data = v.get("data").unwrap_or(&v);
            let size = data.get("size").and_then(Value::as_u64).ok_or("bad server response")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.get("data").and_then(Value::as_str).unwrap_or(""))
                .map_err(|e| format!("bad server response: {e}"))?;
            file.write_all(&bytes).map_err(|e| format!("download failed: {e}"))?;
            offset += bytes.len() as u64;
            if offset >= size || bytes.is_empty() {
                return Ok(());
            }
        }
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    Ok(dest.to_string_lossy().to_string())
}

#[cfg(feature = "desktop")]
mod lb_proxy {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    type BoxBody = http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>;

    fn full_body(data: impl Into<bytes::Bytes>) -> BoxBody {
        use http_body_util::BodyExt;
        http_body_util::Full::new(data.into()).boxed()
    }

    fn err_resp(status: u16, msg: &str) -> hyper::Response<BoxBody> {
        hyper::Response::builder()
            .status(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(full_body(msg.to_string()))
            .unwrap()
    }

    fn http_client() -> &'static reqwest::Client {
        static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
        CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client")
        })
    }

    enum UpstreamStream {
        Plain(tokio::net::TcpStream),
        Tls(tokio_native_tls::TlsStream<tokio::net::TcpStream>),
    }

    impl AsyncRead for UpstreamStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match &mut *self {
                Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
                Self::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for UpstreamStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            match &mut *self {
                Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
                Self::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            }
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match &mut *self {
                Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
                Self::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
            }
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match &mut *self {
                Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
                Self::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            }
        }
    }

    #[derive(Clone)]
    struct Target {
        base_url: String,
        token: String,
        is_tls: bool,
        host: String,
        port: u16,
        host_header: String,
    }

    async fn connect_upstream(t: &Target) -> Result<UpstreamStream, String> {
        let tcp = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::TcpStream::connect((t.host.as_str(), t.port)),
        )
        .await
        .map_err(|_| "connect timeout".to_string())?
        .map_err(|e| e.to_string())?;
        if !t.is_tls {
            return Ok(UpstreamStream::Plain(tcp));
        }
        let cx = tokio_native_tls::native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let tls = tokio_native_tls::TlsConnector::from(cx)
            .connect(&t.host, tcp)
            .await
            .map_err(|e| e.to_string())?;
        Ok(UpstreamStream::Tls(tls))
    }

    async fn handle_ws(
        mut req: hyper::Request<hyper::body::Incoming>,
        target: Target,
        path_and_query: String,
    ) -> hyper::Response<BoxBody> {
        let mut upstream = match connect_upstream(&target).await {
            Ok(s) => s,
            Err(e) => return err_resp(502, &format!("remote connect failed: {e}")),
        };
        let mut raw_req = format!(
            "{} /lb{path_and_query} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n",
            req.method(),
            target.host_header,
            target.token
        );
        for (name, val) in req.headers() {
            let lower = name.as_str().to_ascii_lowercase();
            if lower == "host" || lower == "authorization" {
                continue;
            }
            if let Ok(v) = val.to_str() {
                raw_req.push_str(&format!("{name}: {v}\r\n"));
            }
        }
        raw_req.push_str("\r\n");

        if upstream.write_all(raw_req.as_bytes()).await.is_err() {
            return err_resp(502, "remote write failed");
        }

        let mut buf = Vec::with_capacity(2048);
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), upstream.read(&mut tmp)).await {
                Ok(Ok(0)) | Err(_) | Ok(Err(_)) => return err_resp(502, "remote handshake failed"),
                Ok(Ok(n)) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos;
                    }
                    if buf.len() > 16 * 1024 {
                        return err_resp(502, "remote header too large");
                    }
                }
            }
        };

        let head = String::from_utf8_lossy(&buf[..header_end]);
        let leftover = buf[header_end + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_code: u16 = lines
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(502);

        let mut builder = hyper::Response::builder().status(status_code);
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                builder = builder.header(k.trim(), v.trim());
            }
        }
        if status_code != 101 {
            return builder.body(full_body(leftover)).unwrap_or_else(|_| err_resp(502, "bad response"));
        }

        tokio::spawn(async move {
            if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                let mut client_io = hyper_util::rt::TokioIo::new(upgraded);
                if !leftover.is_empty() && client_io.write_all(&leftover).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client_io, &mut upstream).await;
            }
        });

        builder.body(full_body("")).unwrap()
    }

    async fn handle_req(
        req: hyper::Request<hyper::body::Incoming>,
        target: Target,
    ) -> hyper::Response<BoxBody> {
        use http_body_util::BodyExt;
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());

        let is_ws = req
            .headers()
            .get(hyper::header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
        if is_ws {
            return handle_ws(req, target, path_and_query).await;
        }

        let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
            Ok(m) => m,
            Err(_) => return err_resp(405, "unsupported method"),
        };
        let url = format!("{}/lb{path_and_query}", target.base_url);
        let mut fwd = http_client()
            .request(method, &url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", target.token));
        for (name, val) in req.headers() {
            let lower = name.as_str().to_ascii_lowercase();
            if matches!(lower.as_str(), "host" | "authorization" | "connection" | "transfer-encoding" | "content-length") {
                continue;
            }
            if let (Ok(rn), Ok(rv)) = (
                reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
                reqwest::header::HeaderValue::from_bytes(val.as_bytes()),
            ) {
                fwd = fwd.header(rn, rv);
            }
        }
        let body_bytes = match req.into_body().collect().await {
            Ok(c) => c.to_bytes(),
            Err(_) => return err_resp(400, "unreadable body"),
        };
        if !body_bytes.is_empty() {
            fwd = fwd.body(body_bytes);
        }

        let resp = match fwd.send().await {
            Ok(r) => r,
            Err(e) => return err_resp(502, &format!("remote request failed: {e}")),
        };
        let status = resp.status().as_u16();
        let mut builder = hyper::Response::builder().status(status);
        for (name, val) in resp.headers() {
            let lower = name.as_str().to_ascii_lowercase();
            if matches!(lower.as_str(), "transfer-encoding" | "connection" | "content-length") {
                continue;
            }
            if let (Ok(hn), Ok(hv)) = (
                hyper::header::HeaderName::from_bytes(name.as_str().as_bytes()),
                hyper::header::HeaderValue::from_bytes(val.as_bytes()),
            ) {
                builder = builder.header(hn, hv);
            }
        }
        let bytes = resp.bytes().await.unwrap_or_default();
        builder.body(full_body(bytes)).unwrap_or_else(|_| err_resp(502, "bad response"))
    }

    pub async fn proxy_url(id: &str) -> Result<String, String> {
        static PROXIES: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();
        let map = PROXIES.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(g) = map.lock() {
            if let Some(&port) = g.get(id) {
                return Ok(format!("http://127.0.0.1:{port}"));
            }
        }

        let s = super::load()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("That server is no longer in your list.")?;
        let parsed = reqwest::Url::parse(&s.url).map_err(|e| format!("bad server url: {e}"))?;
        let is_tls = parsed.scheme() == "https";
        let host = parsed.host_str().ok_or("server url missing host")?.to_string();
        let port = parsed.port_or_known_default().ok_or("server url missing port")?;
        let host_header = match parsed.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        let target = Target {
            base_url: s.url.trim_end_matches('/').to_string(),
            token: s.token,
            is_tls,
            host,
            port,
            host_header,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("lb proxy bind failed: {e}"))?;
        let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();

        if let Ok(mut g) = map.lock() {
            if let Some(&existing) = g.get(id) {
                return Ok(format!("http://127.0.0.1:{existing}"));
            }
            g.insert(id.to_string(), local_port);
        }

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let target = target.clone();
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let svc = hyper::service::service_fn(move |req| {
                        let target = target.clone();
                        async move { Ok::<_, std::convert::Infallible>(handle_req(req, target).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .with_upgrades()
                        .await;
                });
            }
        });

        Ok(format!("http://127.0.0.1:{local_port}"))
    }
}

#[cfg(feature = "desktop")]
pub async fn lb_proxy_url(id: &str) -> Result<String, String> {
    lb_proxy::proxy_url(id).await
}

#[cfg(not(feature = "desktop"))]
pub async fn lb_proxy_url(_id: &str) -> Result<String, String> {
    Err("lb proxy requires desktop build".into())
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
