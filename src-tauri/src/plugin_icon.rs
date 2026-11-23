//! Optional, inert SVG images for plugin harnesses. Failure always means fallback.
use std::{io::Read, path::Path};
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub const MAX_ICON: usize = 32 * 1024;

pub fn valid_path(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.ends_with(".svg")
        && name.split('/').all(|p| !p.is_empty() && p != "." && !p.contains("..")
            && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
}

/// Deliberately small SVG subset: paths, geometry and gradients. No entities,
/// CSS, executable elements, event handlers, or non-fragment references.
fn safe_svg(svg: &str) -> bool {
    let mut rest = svg.trim_start_matches('\u{feff}').trim();
    if rest.starts_with("<?xml ") {
        let Some((_, tail)) = rest.split_once("?>") else { return false };
        rest = tail.trim_start();
    }
    if !rest.starts_with("<svg") || !rest.as_bytes().get(4).is_some_and(|c| c.is_ascii_whitespace() || *c == b'>' || *c == b'/') {
        return false;
    }
    let lower = rest.to_ascii_lowercase();
    if lower.contains("javascript:") || lower.contains('&') || lower.contains('\\') { return false; }
    let mut stack = Vec::new();
    let mut root_seen = false;
    while !rest.is_empty() {
        let Some(after) = rest.strip_prefix('<') else { return false };
        let Some(end) = after.find('>') else { return false };
        let tag = &after[..end];
        rest = after[end + 1..].trim_start();
        if let Some(name) = tag.strip_prefix('/') {
            if stack.pop() != Some(name.trim()) { return false; }
            continue;
        }
        let tag = tag.trim_end();
        let single = tag.ends_with('/');
        let tag = tag.trim_end_matches('/').trim_end();
        let n = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let name = &tag[..n];
        if !["svg", "g", "defs", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "linearGradient", "radialGradient", "stop", "clipPath", "mask", "use"].contains(&name) {
            return false;
        }
        if stack.is_empty() {
            if root_seen || name != "svg" { return false; }
            root_seen = true;
        }
        let mut attrs = tag[n..].trim();
        while !attrs.is_empty() {
            let len = attrs.find(|c: char| !(c.is_ascii_alphanumeric() || "-_:".contains(c))).unwrap_or(attrs.len());
            if len == 0 { return false; }
            let key = attrs[..len].to_ascii_lowercase();
            attrs = attrs[len..].trim_start();
            let Some(tail) = attrs.strip_prefix('=') else { return false };
            attrs = tail.trim_start();
            let Some(quote @ ('\'' | '"')) = attrs.chars().next() else { return false };
            let Some(end) = attrs[1..].find(quote) else { return false };
            let value = &attrs[1..end + 1];
            let v = value.trim().to_ascii_lowercase();
            if key.starts_with("on") || key == "style" || key == "xml:base" || value.contains('<')
                || ((key == "href" || key.ends_with(":href")) && !v.starts_with('#'))
                || (v.contains("url(") && (!v.starts_with("url(#") || !v.ends_with(')') || v.matches("url(").count() != 1)) {
                return false;
            }
            attrs = &attrs[end + 2..];
            if !attrs.is_empty() && !attrs.starts_with(char::is_whitespace) { return false; }
            attrs = attrs.trim_start();
        }
        if !single { stack.push(name); }
    }
    root_seen && stack.is_empty()
}

pub fn load(dir: &Path, name: &str) -> Option<String> {
    if !valid_path(name) { return None; }
    let root = dir.canonicalize().ok()?;
    let path = root.join("files").join(name).canonicalize().ok()?;
    if !path.starts_with(&root) { return None; }
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() { return None; }
    let mut bytes = Vec::new();
    file.take((MAX_ICON + 1) as u64).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_ICON || !safe_svg(std::str::from_utf8(&bytes).ok()?) { return None; }
    Some(format!("data:image/svg+xml;base64,{}", STANDARD.encode(bytes)))
}

pub fn declared_icon(dir: &Path, harness: Option<&str>, plugin: Option<&str>) -> Option<String> {
    load(dir, harness.or(plugin)?)
}

pub fn engine_icon(id: &str) -> Option<String> {
    let key = id.strip_prefix("plugin:")?;
    let (pid, hid) = crate::plugin_harness::split_account(key).0.split_once('/')?;
    let (manifest, dir) = crate::plugins::installed_manifest(pid)?;
    let harness = manifest.harnesses.iter().find(|h| h.id == hid)?;
    declared_icon(&dir, harness.icon.as_deref(), manifest.icon.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn svg_subset_rejects_active_content() {
        for good in ["<svg/>", " <?xml version=\"1.0\"?>\n<svg><defs><linearGradient id='b'><stop stop-color='blue'/></linearGradient></defs><path fill='url(#b)' d='M0 0'/></svg>"] {
            assert!(safe_svg(good), "{good}");
        }
        for bad in ["<svgish/>", "<svg><script/></svg>", "<svg><foreignObject/></svg>", "<svg onclick = 'x'/>", "<svg ONLOAD='x'/>", "<svg><use href='https://bad/x'/></svg>", "<svg><use xlink:href='//bad/x'/></svg>", "<svg><use href='data:x'/></svg>", "<svg><use href='javaScript:x'/></svg>", "<svg><use href='&#106;avascript:x'/></svg>", "<!DOCTYPE svg><svg/>", "<svg style='fill:url(https://bad)'/>", "<svg><path fill='url(https://bad)'/></svg>", "<svg>", "<svg/><svg/>"] {
            assert!(!safe_svg(bad), "{bad}");
        }
    }
    #[test]
    fn bounded_confined_icon_loading() {
        let dir = std::env::temp_dir().join(format!("vicuna-icon-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("files/ok.svg"), b"<svg/>").unwrap();
        assert!(declared_icon(&dir, None, None).is_none());
        assert_eq!(declared_icon(&dir, None, Some("ok.svg")), load(&dir, "ok.svg"));
        assert!(declared_icon(&dir, Some("missing.svg"), Some("ok.svg")).is_none());
        assert_eq!(load(&dir, "ok.svg"), Some("data:image/svg+xml;base64,PHN2Zy8+".into()));
        for name in ["../ok.svg", "/ok.svg", "https://a/x.svg", "a\\b.svg", "a/../ok.svg", "x.png", "missing.svg"] { assert!(load(&dir, name).is_none()); }
        std::fs::write(dir.join("files/big.svg"), vec![b' '; MAX_ICON + 1]).unwrap();
        assert!(load(&dir, "big.svg").is_none());
        std::fs::write(dir.join("files/bad.svg"), b"<svg onload='x'/>").unwrap();
        assert!(load(&dir, "bad.svg").is_none());
        #[cfg(unix)] {
            let outside = dir.with_extension("svg");
            std::fs::write(&outside, b"<svg/>").unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("files/link.svg")).unwrap();
            assert!(load(&dir, "link.svg").is_none());
            std::fs::remove_file(outside).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
