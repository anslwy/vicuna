// `vicuna serve` and `vicuna web`: one server (the backend + the web UI
// bundled into the binary) on one port. `serve` runs it for the desktop app
// and the browser alike; `web` is the friendly shortcut that opens the
// browser, reusing a default `vicuna serve` that is already running.
//
// Auth: always token-protected, even on loopback (other local users and
// any web page could otherwise drive a backend that runs shell commands).
// The browser receives the token in the URL fragment (`/#token=…`), which
// never goes to the server or into logs; src/lib/bridge.ts moves it to
// sessionStorage and strips it from the address bar.

use std::path::PathBuf;

use vicuna_lib::server::web::WebRoot;
use vicuna_lib::server::{server_dir, server_token, Config};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub const DEFAULT_PORT: u16 = 7421;
pub const DEFAULT_BIND: &str = "127.0.0.1";

#[derive(Debug, Clone, PartialEq)]
pub struct ServeOpts {
    pub bind: String,
    pub port: u16,
    pub db: Option<PathBuf>,
    pub tailscale: bool,
    pub https_port: Option<u16>,
    pub web_dir: Option<PathBuf>,
    /// `vicuna web` only: open the browser (`--no-open` clears it).
    pub open: bool,
}

impl Default for ServeOpts {
    fn default() -> Self {
        ServeOpts { bind: DEFAULT_BIND.into(), port: DEFAULT_PORT, db: None, tailscale: false, https_port: None, web_dir: None, open: true }
    }
}

impl ServeOpts {
    /// `vicuna web` with nothing that changes which server it means: it may
    /// attach to a `vicuna serve` already running on the default port.
    pub fn may_attach(&self) -> bool {
        *self == ServeOpts::default()
    }
}

/// Parse `serve` options; `web` additionally accepts `--no-open`.
pub fn parse(args: &[String], web: bool) -> Result<ServeOpts, String> {
    let mut o = ServeOpts::default();
    let mut it = args.iter();
    let port = |v: String, flag: &str| v.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| format!("bad {flag} {v:?} (1-65535)"));
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--port" | "-p" => o.port = port(val()?, a)?,
            "--bind" => o.bind = val()?,
            "--db" => o.db = Some(PathBuf::from(val()?)),
            "--web-dir" => o.web_dir = Some(PathBuf::from(val()?)),
            "--tailscale" => o.tailscale = true,
            "--https-port" => o.https_port = Some(port(val()?, a)?),
            "--no-open" if web => o.open = false,
            other => return Err(format!("unknown option {other}")),
        }
    }
    if o.https_port.is_some() && !o.tailscale {
        return Err("--https-port needs --tailscale".into());
    }
    Ok(o)
}

/// The UI to serve: `--web-dir` / `VICUNA_WEB_DIR`, else the embedded build.
/// `Ok(None)` = a dev binary built without `build/`: serve the API only.
pub fn web_root(o: &ServeOpts) -> Result<Option<WebRoot>, String> {
    let dir = o.web_dir.clone().or_else(|| std::env::var_os("VICUNA_WEB_DIR").map(PathBuf::from));
    pick_root(dir, WEB_FILES)
}

fn pick_root(dir: Option<PathBuf>, embedded: &'static [(&'static str, &'static [u8])]) -> Result<Option<WebRoot>, String> {
    match dir {
        Some(d) => {
            let root = WebRoot::Dir(d.clone());
            if root.has_index() { Ok(Some(root)) } else { Err(format!("no index.html in {}", d.display())) }
        }
        None => {
            let root = WebRoot::Embedded(embedded);
            Ok(root.has_index().then_some(root))
        }
    }
}

pub fn config(o: &ServeOpts, web: Option<WebRoot>, token: String, default_db: PathBuf) -> Config {
    Config {
        bind: o.bind.clone(),
        port: o.port,
        token: Some(token),
        db: Some(o.db.clone().unwrap_or(default_db)),
        tailscale: o.tailscale,
        https_port: o.https_port,
        resource_dir: vicuna_lib::browser::headless_resource_dir(),
        web,
    }
}

/// Browser URL for a bound address (a wildcard bind opens via loopback).
pub fn browser_url(addr: std::net::SocketAddr, token: &str) -> String {
    let host = if addr.ip().is_unspecified() { "127.0.0.1".to_string() } else if addr.is_ipv6() { format!("[{}]", addr.ip()) } else { addr.ip().to_string() };
    format!("http://{host}:{}/#token={token}", addr.port())
}

/// The tailnet app URL plus the token fragment.
pub fn tailnet_browser_url(tailnet: &str, token: &str) -> String {
    format!("{}/#token={token}", tailnet.trim_end_matches('/'))
}

/// Startup banner lines (everything but tailscale, which runs a command).
pub fn banner(addr: std::net::SocketAddr, cfg: &Config, token: &str) -> Vec<String> {
    let mut l = vec![
        format!("vicuna server {} on http://{addr}", env!("CARGO_PKG_VERSION")),
        format!("  chats:   {}", cfg.db.as_ref().map(|d| d.display().to_string()).unwrap_or_default()),
        format!("  Server:  http://{addr}   (desktop app: Connect to new server…)"),
        format!("  Token:   {token}"),
    ];
    if cfg.web.is_some() {
        l.push(format!("  Browser: {}", browser_url(addr, token)));
    } else {
        l.push("  Browser: web UI not bundled in this build (run `npm run build` and rebuild, or pass --web-dir); serving the API only".into());
    }
    l
}

/// Bind, print the banner (+ tailscale), optionally open the browser, serve forever.
pub fn run(o: ServeOpts, open: bool) {
    let web = web_root(&o).unwrap_or_else(|e| fail(&e));
    if open && web.is_none() {
        fail("this vicuna binary was built without the web UI. Run `npm run build` and rebuild, or pass --web-dir <path to build/>");
    }
    let token = server_token();
    let cfg = config(&o, web, token.clone(), server_dir().join("vicuna.db"));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    let bound = rt.block_on(vicuna_lib::server::bind(&cfg)).unwrap_or_else(|e| fail(&format!("{e} (pick another port with --port N)")));
    let addr = bound.addr;
    for line in banner(addr, &cfg, &token) {
        println!("{line}");
    }
    if cfg.tailscale {
        match crate::cli::tailscale_expose(addr.port(), cfg.https_port) {
            Ok(url) => {
                println!("  Tailnet: {url}   (desktop app, with the token above)");
                if cfg.web.is_some() {
                    println!("  Tailnet browser: {}", tailnet_browser_url(&url, &token));
                }
            }
            Err(e) => println!("  tailscale: {e}"),
        }
    } else {
        println!("Expose it to your tailnet (HTTPS) with `--tailscale`, or by hand:");
        println!("  tailscale serve --bg --https=8443 http://127.0.0.1:{}", addr.port());
    }
    if !addr.ip().is_loopback() {
        println!("WARNING: listening on {} over plain HTTP (token-protected); prefer --bind 127.0.0.1 with --tailscale.", cfg.bind);
    }
    if open {
        open_browser(&browser_url(addr, &token));
    }
    println!("Ctrl-C to stop.");
    rt.block_on(bound.serve_until(crate::shutdown_signal()));
}

/// `vicuna web`: reuse a default `vicuna serve` when one already serves the
/// UI, else start one with these options and open the browser.
pub fn web(o: ServeOpts) {
    if o.may_attach() {
        let token = server_token();
        let addr: std::net::SocketAddr = (std::net::Ipv4Addr::LOCALHOST, DEFAULT_PORT).into();
        if probe_running(addr, &token) {
            let url = browser_url(addr, &token);
            println!("vicuna serve is already running on http://{addr}");
            println!("  Browser: {url}");
            open_browser(&url);
            return;
        }
    }
    let open = o.open;
    run(o, open);
}

/// A Vicuna server on `addr` that accepts our token and serves the web UI.
fn probe_running(addr: std::net::SocketAddr, token: &str) -> bool {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async {
        let Ok(c) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).build() else { return false };
        let base = format!("http://{addr}");
        let health = c.get(format!("{base}/health")).send().await.map(|r| r.status().is_success()).unwrap_or(false);
        if !health {
            return false;
        }
        let authed = c
            .post(format!("{base}/invoke"))
            .bearer_auth(token)
            .json(&serde_json::json!({"cmd": "server_info", "args": {}}))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !authed {
            return false;
        }
        match c.get(format!("{base}/")).send().await {
            Ok(r) => {
                let html = r.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/html"));
                r.status().is_success() && html
            }
            Err(_) => false,
        }
    })
}

fn open_browser(url: &str) {
    if let Err(e) = crate::cli::open_url(url) {
        println!("  (could not open a browser: {e}; open the URL above yourself)");
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("vicuna: {msg}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults() {
        let o = parse(&[], true).unwrap();
        assert_eq!((o.bind.as_str(), o.port, o.open, o.tailscale), ("127.0.0.1", 7421, true, false));
        assert_eq!(o.db, None);
        assert!(o.may_attach());
    }

    #[test]
    fn flags() {
        let o = parse(&a(&["-p", "9000", "--bind", "0.0.0.0", "--db", "/x/v.db", "--no-open", "--tailscale", "--https-port", "8444", "--web-dir", "build"]), true).unwrap();
        assert_eq!(o.port, 9000);
        assert_eq!(o.bind, "0.0.0.0");
        assert_eq!(o.db, Some(PathBuf::from("/x/v.db")));
        assert!(!o.open && o.tailscale);
        assert_eq!(o.https_port, Some(8444));
        assert_eq!(o.web_dir, Some(PathBuf::from("build")));
        assert_eq!(parse(&a(&["--port", "1234"]), false).unwrap().port, 1234);
    }

    #[test]
    fn serve_takes_web_dir_but_not_no_open() {
        assert_eq!(parse(&a(&["--web-dir", "/b"]), false).unwrap().web_dir, Some(PathBuf::from("/b")));
        assert!(parse(&a(&["--no-open"]), false).is_err());
    }

    #[test]
    fn any_server_flag_disables_attach() {
        for f in [&["--port", "9"][..], &["--bind", "0.0.0.0"], &["--db", "/x"], &["--tailscale"], &["--web-dir", "b"], &["--no-open"]] {
            assert!(!parse(&a(f), true).unwrap().may_attach(), "{f:?}");
        }
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [&["--port", "0"][..], &["--port", "70000"], &["-p", "abc"], &["--port"], &["--bogus"], &["--https-port", "8443"]] {
            assert!(parse(&a(bad), true).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_is_always_token_protected() {
        let o = parse(&a(&["--bind", "0.0.0.0"]), false).unwrap();
        let c = config(&o, None, "tok".into(), PathBuf::from("/d/v.db"));
        assert_eq!(c.token.as_deref(), Some("tok"));
        assert_eq!(c.db, Some(PathBuf::from("/d/v.db")));
        assert!(c.web.is_none());
    }

    #[test]
    fn urls() {
        assert_eq!(browser_url("127.0.0.1:7421".parse().unwrap(), "t"), "http://127.0.0.1:7421/#token=t");
        assert_eq!(browser_url("0.0.0.0:9000".parse().unwrap(), "t"), "http://127.0.0.1:9000/#token=t");
        assert_eq!(browser_url("[::]:9000".parse().unwrap(), "t"), "http://127.0.0.1:9000/#token=t");
        assert_eq!(browser_url("[::1]:9000".parse().unwrap(), "t"), "http://[::1]:9000/#token=t");
        assert_eq!(tailnet_browser_url("https://h.ts.net:8443/", "t"), "https://h.ts.net:8443/#token=t");
        assert_eq!(tailnet_browser_url("https://h.ts.net", "t"), "https://h.ts.net/#token=t");
    }

    #[test]
    fn missing_ui_falls_back_to_api_only() {
        assert!(pick_root(None, &[]).unwrap().is_none());
        static FILES: &[(&str, &[u8])] = &[("index.html", b"<html>")];
        assert!(pick_root(None, FILES).unwrap().is_some());
        assert!(pick_root(Some("/nonexistent-vicuna-web".into()), FILES).unwrap_err().contains("no index.html"));
    }

    #[test]
    fn banner_shows_browser_url_beside_app_info() {
        let o = ServeOpts::default();
        let addr = "127.0.0.1:7421".parse().unwrap();
        static FILES: &[(&str, &[u8])] = &[("index.html", b"<html>")];
        let with = banner(addr, &config(&o, Some(WebRoot::Embedded(FILES)), "t".into(), "/d".into()), "t");
        assert!(with.iter().any(|l| l.contains("Token:   t")));
        assert!(with.iter().any(|l| l.contains("Browser: http://127.0.0.1:7421/#token=t")));
        let without = banner(addr, &config(&o, None, "t".into(), "/d".into()), "t");
        assert!(without.iter().any(|l| l.contains("API only")));
        assert!(!without.iter().any(|l| l.contains("#token=")));
    }

    #[test]
    fn attach_probe_misses_when_nothing_listens() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        assert!(!probe_running(addr, "t"));
    }
}
