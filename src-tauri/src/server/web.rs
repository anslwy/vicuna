// `vicuna web`: the server also serves the built SvelteKit app (`build/`),
// so one process is the whole Vicuna in a browser. The files are public (no
// data in them); every API route keeps its bearer-token check, and the
// browser gets the token through the URL fragment (see src/lib/bridge.ts).

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

/// Where the web UI comes from: files compiled into the binary (release
/// CLI), or a `build/` folder on disk (dev, or a binary built before
/// `npm run build`).
#[derive(Clone, Debug)]
pub enum WebRoot {
    Embedded(&'static [(&'static str, &'static [u8])]),
    Dir(PathBuf),
}

pub struct WebFile {
    pub body: Cow<'static, [u8]>,
    pub content_type: &'static str,
    pub cache_control: &'static str,
}

/// Paths that belong to the API and must never fall back to index.html.
pub fn is_api_path(path: &str) -> bool {
    let p = path;
    p == "/invoke"
        || p == "/health"
        || p == "/events"
        || p == "/lb"
        || ["/poll/", "/events/", "/api/", "/lb/", "/plugin-panel/"].iter().any(|pre| p.starts_with(pre))
}

pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Clean, relative file path for a URL path, or None for anything that
/// tries to climb out (`..`, absolute pieces, backslashes, NUL, %-escapes
/// that decode to those).
pub fn safe_rel(path: &str) -> Option<String> {
    let decoded = percent_decode(path)?;
    if decoded.contains('\\') || decoded.contains('\0') {
        return None;
    }
    let rel = decoded.trim_start_matches('/');
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl WebRoot {
    fn read(&self, rel: &str) -> Option<Cow<'static, [u8]>> {
        if rel.is_empty() {
            return None;
        }
        match self {
            WebRoot::Embedded(files) => files.iter().find(|(p, _)| *p == rel).map(|(_, b)| Cow::Borrowed(*b)),
            WebRoot::Dir(dir) => {
                let p = dir.join(rel);
                // the joined path must stay inside the root even via symlinks
                let (canon, root) = (p.canonicalize().ok()?, dir.canonicalize().ok()?);
                if !canon.starts_with(&root) || !canon.is_file() {
                    return None;
                }
                std::fs::read(canon).ok().map(Cow::Owned)
            }
        }
    }

    /// Does this root actually hold the app (index.html)?
    pub fn has_index(&self) -> bool {
        self.read("index.html").is_some()
    }

    /// The response for a GET of `path`: the file, `<dir>/index.html`, or
    /// the SPA fallback for extension-less routes. None = 404.
    pub fn resolve(&self, path: &str) -> Option<WebFile> {
        if is_api_path(path) {
            return None;
        }
        let rel = safe_rel(path)?;
        let candidates = [rel.clone(), if rel.is_empty() { "index.html".into() } else { format!("{rel}/index.html") }];
        for c in candidates {
            if let Some(body) = self.read(&c) {
                return Some(WebFile { body, content_type: content_type(&c), cache_control: cache_for(&c) });
            }
        }
        // client-side route (`/s/<id>`): hand back the app shell; missing
        // assets (anything with an extension) stay 404s
        let last = rel.rsplit('/').next().unwrap_or("");
        if last.contains('.') {
            return None;
        }
        let body = self.read("index.html")?;
        Some(WebFile { body, content_type: content_type("index.html"), cache_control: cache_for("index.html") })
    }
}

fn cache_for(rel: &str) -> &'static str {
    if rel.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FILES: &[(&str, &[u8])] = &[
        ("index.html", b"<html>app</html>"),
        ("favicon.png", b"png"),
        ("_app/immutable/entry/start.abc.js", b"js"),
        ("_app/immutable/assets/app.css", b"css"),
        ("_app/version.json", b"{}"),
    ];

    #[test]
    fn serves_files_with_types_and_cache() {
        let r = WebRoot::Embedded(FILES);
        let f = r.resolve("/").unwrap();
        assert_eq!(&*f.body, b"<html>app</html>");
        assert_eq!(f.content_type, "text/html; charset=utf-8");
        assert_eq!(f.cache_control, "no-cache");
        let js = r.resolve("/_app/immutable/entry/start.abc.js").unwrap();
        assert_eq!(js.content_type, "text/javascript; charset=utf-8");
        assert!(js.cache_control.contains("immutable"));
        assert_eq!(r.resolve("/_app/immutable/assets/app.css").unwrap().content_type, "text/css; charset=utf-8");
        assert_eq!(r.resolve("/favicon.png").unwrap().content_type, "image/png");
        assert_eq!(r.resolve("/_app/version.json").unwrap().cache_control, "no-cache");
    }

    #[test]
    fn mime_table() {
        for (p, t) in [
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.svg", "image/svg+xml"),
            ("a.JPG", "image/jpeg"),
            ("a.ico", "image/x-icon"),
            ("a.woff2", "font/woff2"),
            ("a.ttf", "font/ttf"),
            ("a.wasm", "application/wasm"),
            ("a.json", "application/json"),
            ("noext", "application/octet-stream"),
        ] {
            assert_eq!(content_type(p), t, "{p}");
        }
    }

    #[test]
    fn spa_fallback_but_not_for_assets_or_api() {
        let r = WebRoot::Embedded(FILES);
        assert_eq!(&*r.resolve("/s/1234").unwrap().body, b"<html>app</html>");
        assert!(r.resolve("/missing.js").is_none());
        for api in ["/invoke", "/health", "/events", "/poll/x", "/api/x", "/lb", "/lb/x", "/plugin-panel/a/b/c.html"] {
            assert!(r.resolve(api).is_none(), "{api}");
        }
    }

    #[test]
    fn traversal_is_rejected() {
        for p in ["/../etc/passwd", "/%2e%2e/etc/passwd", "/_app/../../x", "/a%5c..%5cb", "/%00", "/%zz"] {
            assert!(safe_rel(p).is_none(), "{p}");
        }
        assert_eq!(safe_rel("/./_app/x.js").as_deref(), Some("_app/x.js"));
        // a dir root never reads outside itself
        let tmp = std::env::temp_dir().join(format!("vicuna-web-test-{}", std::process::id()));
        let root = tmp.join("build");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "<html>disk</html>").unwrap();
        std::fs::write(tmp.join("secret.txt"), "nope").unwrap();
        let r = WebRoot::Dir(root.clone());
        assert!(r.has_index());
        assert!(r.resolve("/../secret.txt").is_none());
        assert!(r.resolve("/%2e%2e/secret.txt").is_none());
        assert_eq!(&*r.resolve("/anything").unwrap().body, b"<html>disk</html>");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(tmp.join("secret.txt"), root.join("link.txt")).unwrap();
            assert!(r.resolve("/link.txt").is_none());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
