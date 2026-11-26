// `vicuna web`: the whole Vicuna (bundled web UI + the same backend as
// `vicuna serve`) on one port, opened in the default browser.
//
// Auth: always token-protected, even on loopback (other local users and
// any web page could otherwise drive a backend that runs shell commands).
// The browser receives the token in the URL fragment (`/#token=…`), which
// never goes to the server or into logs; src/lib/bridge.ts moves it to
// sessionStorage and strips it from the address bar.

use std::path::PathBuf;

use vicuna_lib::server::web::WebRoot;
use vicuna_lib::server::Config;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub const DEFAULT_PORT: u16 = 7422;

#[derive(Debug, Clone, PartialEq)]
pub struct WebOpts {
    pub bind: String,
    pub port: u16,
    pub db: Option<PathBuf>,
    pub open: bool,
    pub tailscale: bool,
    pub https_port: Option<u16>,
    pub web_dir: Option<PathBuf>,
}

pub fn parse(args: &[String]) -> Result<WebOpts, String> {
    let mut o = WebOpts { bind: "127.0.0.1".into(), port: DEFAULT_PORT, db: None, open: true, tailscale: false, https_port: None, web_dir: None };
    let mut it = args.iter();
    let port = |v: String, flag: &str| v.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| format!("bad {flag} {v:?} (1-65535)"));
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--port" | "-p" => o.port = port(val()?, a)?,
            "--bind" => o.bind = val()?,
            "--db" => o.db = Some(PathBuf::from(val()?)),
            "--web-dir" => o.web_dir = Some(PathBuf::from(val()?)),
            "--no-open" => o.open = false,
            "--tailscale" => o.tailscale = true,
            "--https-port" => o.https_port = Some(port(val()?, a)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if o.https_port.is_some() && !o.tailscale {
        return Err("--https-port needs --tailscale".into());
    }
    Ok(o)
}

/// The UI to serve: `--web-dir` / `VICUNA_WEB_DIR`, else the embedded build.
pub fn web_root(o: &WebOpts) -> Result<WebRoot, String> {
    let dir = o.web_dir.clone().or_else(|| std::env::var_os("VICUNA_WEB_DIR").map(PathBuf::from));
    let root = match dir {
        Some(d) => WebRoot::Dir(d),
        None => WebRoot::Embedded(WEB_FILES),
    };
    if !root.has_index() {
        return Err(match root {
            WebRoot::Dir(d) => format!("no index.html in {}", d.display()),
            WebRoot::Embedded(_) => "this vicuna binary was built without the web UI. Run `npm run build` and rebuild, or pass --web-dir <path to build/>".into(),
        });
    }
    Ok(root)
}

pub fn config(o: &WebOpts, root: WebRoot, token: String, default_db: PathBuf) -> Config {
    Config {
        bind: o.bind.clone(),
        port: o.port,
        token: Some(token),
        db: Some(o.db.clone().unwrap_or(default_db)),
        tailscale: o.tailscale,
        https_port: o.https_port,
        resource_dir: vicuna_lib::browser::headless_resource_dir(),
        web: Some(root),
    }
}

/// Browser URL for a bound address (a wildcard bind opens via loopback).
pub fn browser_url(addr: std::net::SocketAddr, token: &str) -> String {
    let host = if addr.ip().is_unspecified() { "127.0.0.1".to_string() } else if addr.is_ipv6() { format!("[{}]", addr.ip()) } else { addr.ip().to_string() };
    format!("http://{host}:{}/#token={token}", addr.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults() {
        let o = parse(&[]).unwrap();
        assert_eq!((o.bind.as_str(), o.port, o.open, o.tailscale), ("127.0.0.1", 7422, true, false));
        assert_eq!(o.db, None);
    }

    #[test]
    fn flags() {
        let o = parse(&a(&["-p", "9000", "--bind", "0.0.0.0", "--db", "/x/v.db", "--no-open", "--tailscale", "--https-port", "8444", "--web-dir", "build"])).unwrap();
        assert_eq!(o.port, 9000);
        assert_eq!(o.bind, "0.0.0.0");
        assert_eq!(o.db, Some(PathBuf::from("/x/v.db")));
        assert!(!o.open && o.tailscale);
        assert_eq!(o.https_port, Some(8444));
        assert_eq!(o.web_dir, Some(PathBuf::from("build")));
        assert_eq!(parse(&a(&["--port", "1234"])).unwrap().port, 1234);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [&["--port", "0"][..], &["--port", "70000"], &["-p", "abc"], &["--port"], &["--bogus"], &["--https-port", "8443"]] {
            assert!(parse(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_is_always_token_protected() {
        let o = parse(&a(&["--bind", "0.0.0.0"])).unwrap();
        let c = config(&o, WebRoot::Embedded(&[]), "tok".into(), PathBuf::from("/d/v.db"));
        assert_eq!(c.token.as_deref(), Some("tok"));
        assert_eq!(c.db, Some(PathBuf::from("/d/v.db")));
        assert!(c.web.is_some());
    }

    #[test]
    fn urls() {
        assert_eq!(browser_url("127.0.0.1:7422".parse().unwrap(), "t"), "http://127.0.0.1:7422/#token=t");
        assert_eq!(browser_url("0.0.0.0:9000".parse().unwrap(), "t"), "http://127.0.0.1:9000/#token=t");
        assert_eq!(browser_url("[::1]:9000".parse().unwrap(), "t"), "http://[::1]:9000/#token=t");
    }

    #[test]
    fn missing_dir_is_a_clear_error() {
        let o = WebOpts { web_dir: Some("/nonexistent-vicuna-web".into()), ..parse(&[]).unwrap() };
        assert!(web_root(&o).unwrap_err().contains("no index.html"));
    }
}
