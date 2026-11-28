// Session attachments read on demand: inline images (`messages.images`, data
// URLs) and staged upload files (`~/vicuna/uploads/<session>/<file>`).
//
// `read_session` only ever carries lightweight metadata ({index, mime, bytes}
// / {name, bytes}); the bytes travel through `read_attachment` (vicuna-mcp) or
// the server's `server_read_attachment` action, one attachment at a time.
// No schema change: everything is derived from the existing columns/folders.

use base64::Engine as _;
use serde_json::{json, Value};
use sqlx::Row;
use std::path::{Path, PathBuf};

/// Hard cap on one attachment's decoded size.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// MIME types returned as MCP `image` blocks.
pub const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// `data:<mime>;base64,<payload>` → (mime, payload with whitespace removed).
pub fn parse_data_url(s: &str) -> Option<(String, String)> {
    let rest = s.trim().strip_prefix("data:")?;
    let (head, payload) = rest.split_once(',')?;
    let mime = head.strip_suffix(";base64")?.trim().to_ascii_lowercase();
    if mime.is_empty() {
        return None;
    }
    Some((mime, payload.chars().filter(|c| !c.is_whitespace()).collect()))
}

/// Decoded size of a base64 payload, without decoding it.
pub fn b64_decoded_len(b64: &str) -> usize {
    let n = b64.len();
    let pad = b64.bytes().rev().take_while(|b| *b == b'=').count().min(2);
    (n / 4) * 3 + match n % 4 { 2 => 1, 3 => 2, _ => 0 } - if n % 4 == 0 { pad } else { 0 }
}

/// Metadata for a message's images: `[{index, mime, bytes}]` (never base64).
/// Accepts the stored JSON text or an already-parsed array of data URLs.
pub fn images_meta(images: &Value) -> Vec<Value> {
    let arr = match images {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        v => v.clone(),
    };
    arr.as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, v)| {
            let (mime, b64) = parse_data_url(v.as_str()?)?;
            Some(json!({ "index": i, "mime": mime, "bytes": b64_decoded_len(&b64) }))
        })
        .collect()
}

/// Regular files staged for a session: `[{name, bytes}]`, sorted by name.
pub fn uploads_meta(session_id: &str) -> Vec<Value> {
    let Ok(dir) = crate::updates_uploads::upload_session_dir(session_id) else { return Vec::new() };
    uploads_meta_in(&dir)
}

pub fn uploads_meta_in(dir: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let md = std::fs::symlink_metadata(e.path()).ok()?;
            if !md.is_file() {
                return None;
            }
            Some((e.file_name().to_string_lossy().to_string(), md.len()))
        })
        .collect();
    out.sort();
    out.into_iter().map(|(name, bytes)| json!({ "name": name, "bytes": bytes })).collect()
}

/// Strict basename check for an upload name.
pub fn check_basename(name: &str) -> Result<(), String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.len() > 255;
    if bad { Err(format!("Invalid file name {name:?}: pass a plain file name from read_session's `uploads`.")) } else { Ok(()) }
}

/// Resolve `<dir>/<name>` and require its canonical form to stay inside `dir`
/// (rejects symlinks that escape) and be a regular file.
pub fn safe_upload_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    check_basename(name)?;
    let base = dir.canonicalize().map_err(|_| "This session has no uploaded files.".to_string())?;
    let p = dir.join(name).canonicalize().map_err(|_| format!("No uploaded file named {name:?} in this session."))?;
    if !p.starts_with(&base) || p.parent() != Some(base.as_path()) {
        return Err(format!("Refused {name:?}: it points outside the session's upload folder."));
    }
    if !p.is_file() {
        return Err(format!("{name:?} is not a regular file."));
    }
    Ok(p)
}

/// MIME type from a file name (enough for the image whitelist + common files).
pub fn mime_for_name(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Which attachment to read.
#[derive(Debug, Clone, PartialEq)]
pub enum Which {
    Inline { message_idx: i64, image_index: usize },
    Upload { filename: String },
}

/// Parse `message_idx` + `image_index` / `filename` (mutually exclusive).
pub fn parse_which(args: &Value) -> Result<Which, String> {
    let file = args.get("filename").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let msg = args.get("message_idx").filter(|v| !v.is_null());
    match (msg, file) {
        (Some(_), Some(_)) => Err("Pass either `message_idx` (inline image) or `filename` (uploaded file), not both.".into()),
        (None, Some(f)) => Ok(Which::Upload { filename: f.to_string() }),
        (Some(m), None) => {
            let message_idx = m.as_i64().filter(|n| *n >= 0).ok_or("`message_idx` must be a non-negative integer.")?;
            let image_index = match args.get("image_index").filter(|v| !v.is_null()) {
                None => 0,
                Some(v) => v.as_u64().ok_or("`image_index` must be a non-negative integer.")? as usize,
            };
            Ok(Which::Inline { message_idx, image_index })
        }
        (None, None) => Err("Pass `message_idx` (+ optional `image_index`) for an inline image, or `filename` for an uploaded file (see read_session's `images` / `uploads`).".into()),
    }
}

/// Read one inline image from `messages.images`.
pub async fn read_inline(pool: &sqlx::SqlitePool, session_id: &str, message_idx: i64, image_index: usize) -> Result<Attachment, String> {
    let row = sqlx::query("SELECT images FROM messages WHERE chat_id = $1 AND idx = $2")
        .bind(session_id)
        .bind(message_idx)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read_attachment failed: {e}"))?
        .ok_or_else(|| format!("Message {message_idx} not found in session {session_id}."))?;
    let raw: Option<String> = row.try_get("images").ok().flatten();
    let arr = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    let item = arr.as_array().and_then(|a| a.get(image_index)).and_then(Value::as_str)
        .ok_or_else(|| format!("Message {message_idx} has no image {image_index}."))?;
    let (mime, b64) = parse_data_url(item).ok_or("That image is not a base64 data URL.")?;
    if b64_decoded_len(&b64) > MAX_BYTES {
        return Err(format!("Image is larger than the {} MB limit.", MAX_BYTES / 1024 / 1024));
    }
    let data = base64::engine::general_purpose::STANDARD.decode(&b64).map_err(|e| format!("Image data is not valid base64: {e}"))?;
    let ext = mime.split('/').nth(1).unwrap_or("bin").replace("jpeg", "jpg");
    Ok(Attachment { name: format!("message{message_idx}-image{image_index}.{ext}"), mime, data })
}

/// Read one staged upload file of a session.
pub fn read_upload_in(dir: &Path, filename: &str) -> Result<Attachment, String> {
    let p = safe_upload_path(dir, filename)?;
    let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    if len as usize > MAX_BYTES {
        return Err(format!("{filename:?} is larger than the {} MB limit.", MAX_BYTES / 1024 / 1024));
    }
    let data = std::fs::read(&p).map_err(|e| format!("Cannot read {filename:?}: {e}"))?;
    Ok(Attachment { name: filename.to_string(), mime: mime_for_name(filename).to_string(), data })
}

pub async fn read_local(pool: Option<&sqlx::SqlitePool>, session_id: &str, which: &Which) -> Result<Attachment, String> {
    match which {
        Which::Inline { message_idx, image_index } => {
            let pool = pool.ok_or("vicuna.db is not available")?;
            read_inline(pool, session_id, *message_idx, *image_index).await
        }
        Which::Upload { filename } => {
            let dir = crate::updates_uploads::upload_session_dir(session_id)?;
            read_upload_in(&dir, filename)
        }
    }
}

/// Wire form for `server_read_attachment`.
pub fn to_wire(a: &Attachment) -> Value {
    json!({ "name": a.name, "mime": a.mime, "bytes": a.data.len(), "data": base64::engine::general_purpose::STANDARD.encode(&a.data) })
}

pub fn from_wire(v: &Value) -> Result<Attachment, String> {
    let b64 = v["data"].as_str().ok_or("the server returned no attachment data")?;
    if b64_decoded_len(b64) > MAX_BYTES {
        return Err(format!("Attachment is larger than the {} MB limit.", MAX_BYTES / 1024 / 1024));
    }
    let data = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("bad attachment data: {e}"))?;
    Ok(Attachment { name: sanitize_name(v["name"].as_str().unwrap_or("")), mime: v["mime"].as_str().unwrap_or("application/octet-stream").to_ascii_lowercase(), data })
}

/// A remote-supplied name is only a hint: basename, safe characters, no `..`.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut clean: String = base.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.')).collect();
    while clean.contains("..") {
        clean = clean.replace("..", ".");
    }
    let clean = clean.trim_matches(|c| c == '.' || c == ' ').to_string();
    if clean.is_empty() { "attachment".into() } else { clean }
}

/// `~/vicuna/cache/remote-attachments/` (never /tmp).
pub fn cache_dir() -> Result<PathBuf, String> {
    Ok(crate::updates_uploads::uploads_home_dir()?.join("vicuna").join("cache").join("remote-attachments"))
}

/// Save under `dir` with a collision-safe name (`name`, `name (1)`, …).
pub fn save_unique(dir: &Path, name: &str, data: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let safe: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c }).collect();
    let safe = if safe.trim_matches('.').is_empty() { "attachment".to_string() } else { safe };
    let (stem, ext) = match safe.rsplit_once('.') { Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")), _ => (safe.clone(), String::new()) };
    for n in 0..10_000 {
        let candidate = if n == 0 { dir.join(&safe) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                use std::io::Write as _;
                f.write_all(data).map_err(|e| format!("Cannot write {}: {e}", candidate.display()))?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot write {}: {e}", candidate.display())),
        }
    }
    Err("Too many files with that name in the attachment cache.".into())
}

/// MCP tool result: an `image` block for whitelisted images, else (or with
/// `save_to_file`) the bytes saved to the cache dir and the path returned.
pub fn mcp_result(a: &Attachment, source: &str, save_to_file: bool) -> Result<Value, String> {
    mcp_result_in(a, source, save_to_file, &cache_dir()?)
}

pub fn mcp_result_in(a: &Attachment, source: &str, save_to_file: bool, cache: &Path) -> Result<Value, String> {
    if a.data.len() > MAX_BYTES {
        return Err(format!("Attachment is larger than the {} MB limit.", MAX_BYTES / 1024 / 1024));
    }
    let is_image = IMAGE_MIMES.contains(&a.mime.as_str());
    if is_image && !save_to_file {
        let text = format!("{source}: {} ({}, {} bytes), attached below.", a.name, a.mime, a.data.len());
        return Ok(json!({ "content": [
            { "type": "text", "text": text },
            { "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&a.data), "mimeType": a.mime },
        ], "isError": false }));
    }
    let path = save_unique(cache, &a.name, &a.data)?;
    let text = format!("{source}: {} saved to {} ({}, {} bytes).", a.name, path.display(), a.mime, a.data.len());
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
}

/// Scratch folder for tests, under `target/` (never the system temp dir).
#[cfg(test)]
pub fn test_dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-attachments").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_and_sizes() {
        assert_eq!(parse_data_url("data:image/JPEG;base64,QU\nJD"), Some(("image/jpeg".into(), "QUJD".into())));
        assert_eq!(parse_data_url("nope"), None);
        assert_eq!(parse_data_url("data:image/png,raw"), None);
        for s in ["", "a", "ab", "abc", "abcd", "abcde"] {
            let b = base64::engine::general_purpose::STANDARD.encode(s);
            assert_eq!(b64_decoded_len(&b), s.len(), "{s}");
        }
        let m = images_meta(&json!(r#"["data:image/png;base64,eA==","junk"]"#));
        assert_eq!(m, vec![json!({"index":0,"mime":"image/png","bytes":1})]);
        assert!(!m[0].to_string().contains("eA=="));
    }

    #[test]
    fn which_args() {
        assert_eq!(parse_which(&json!({"message_idx": 2})).unwrap(), Which::Inline { message_idx: 2, image_index: 0 });
        assert_eq!(parse_which(&json!({"filename": "a.txt"})).unwrap(), Which::Upload { filename: "a.txt".into() });
        assert!(parse_which(&json!({"message_idx": 1, "filename": "a"})).is_err());
        assert!(parse_which(&json!({})).is_err());
        assert!(parse_which(&json!({"message_idx": -1})).is_err());
    }

    #[test]
    fn uploads_are_contained() {
        let root = test_dir("att");
        let dir = root.join("sess");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("secret"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("secret"), dir.join("link")).unwrap();
        assert_eq!(uploads_meta_in(&dir), vec![json!({"name":"a.txt","bytes":5})], "dirs and symlinks are not listed");
        assert_eq!(read_upload_in(&dir, "a.txt").unwrap().data, b"hello");
        for bad in ["../secret", "..", "sub/x", "a\\b", "x\0", "", "sub"] {
            assert!(read_upload_in(&dir, bad).is_err(), "{bad:?}");
        }
        #[cfg(unix)]
        assert!(read_upload_in(&dir, "link").unwrap_err().contains("outside"));
        let big = vec![0u8; MAX_BYTES + 1];
        std::fs::write(dir.join("big.bin"), &big).unwrap();
        assert!(read_upload_in(&dir, "big.bin").unwrap_err().contains("limit"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn results_and_cache() {
        let cache = test_dir("att-cache");
        let img = Attachment { name: "p.png".into(), mime: "image/png".into(), data: vec![1, 2, 3] };
        let r = mcp_result_in(&img, "Session s", false, &cache).unwrap();
        assert_eq!(r["content"][1]["type"], "image");
        assert_eq!(r["content"][1]["mimeType"], "image/png");
        // save_to_file + non-images land in the cache dir with unique names
        let p1 = save_unique(&cache, "p.png", &img.data).unwrap();
        let p2 = save_unique(&cache, "p.png", &img.data).unwrap();
        assert_ne!(p1, p2);
        assert!(p2.file_name().unwrap().to_string_lossy().contains("(1)"));
        let svg = Attachment { name: "x.svg".into(), mime: "image/svg+xml".into(), data: b"<svg/>".to_vec() };
        assert!(!IMAGE_MIMES.contains(&svg.mime.as_str()), "svg is not whitelisted");
        let big = Attachment { name: "b.png".into(), mime: "image/png".into(), data: vec![0; MAX_BYTES + 1] };
        assert!(mcp_result_in(&big, "s", false, &cache).is_err());
        let w = from_wire(&to_wire(&img)).unwrap();
        assert_eq!(w, img);
        let evil = from_wire(&json!({"name": "../../etc/pass.wd", "mime": "text/plain", "data": "eA=="})).unwrap();
        assert!(!evil.name.contains('/') && !evil.name.contains(".."), "{}", evil.name);
        let _ = std::fs::remove_dir_all(&cache);
    }
}
