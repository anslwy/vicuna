use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WorkFetchContent {
    pub url: String,
    pub content: String,
    pub truncated: bool,
    pub size: u64,
}

/// Fetch a web page as plain text (the agent's `fetch_url` tool: read the
/// article behind a search-result link instead of guessing from snippets).
/// Unlike `ollama_fetch_text` — allowlisted to the Ollama registry for
/// catalog Fetch — this takes any http(s) URL: the agent needs the open web.
/// http(s) scheme only, 30s timeout, 1MB raw cap, HTML stripped to text,
/// ~64KB text cap like `read`.
pub async fn work_fetch_url_logic(url: String) -> Result<WorkFetchContent, String> {
    let url = url.trim().to_string();
    if url.is_empty() || url.len() > 2048 {
        return Err("fetch_url needs a full http(s) URL".to_string());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("fetch_url needs a full http(s) URL (not a bare host or other scheme)".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("fetch failed: {e}"))?
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8")
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("fetch failed: HTTP {}", res.status()));
    }
    let ctype = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !ctype.is_empty()
        && !ctype.contains("html")
        && !ctype.starts_with("text/")
        && !ctype.contains("json")
        && !ctype.contains("xml")
        && !ctype.contains("javascript")
    {
        let short = ctype.split(';').next().unwrap_or(&ctype);
        return Err(format!(
            "unsupported content type ({short}) — fetch_url reads pages and text, not files"
        ));
    }
    let bytes = res.bytes().await.map_err(|e| format!("fetch failed: {e}"))?;
    const RAW_CAP: usize = 1024 * 1024;
    let raw = &bytes[..bytes.len().min(RAW_CAP)];
    if raw.iter().take(4096).any(|&b| b == 0) {
        return Err("binary content — fetch_url reads text pages only".to_string());
    }
    let raw_str = String::from_utf8_lossy(raw);
    let sniff_html = raw_str.trim_start().starts_with('<');
    let text = if ctype.contains("html") || (ctype.is_empty() && sniff_html) {
        html_to_text(&raw_str)
    } else {
        raw_str.trim().to_string()
    };
    if text.is_empty() {
        return Err("page contained no readable text".to_string());
    }
    const TEXT_CAP: usize = 65536;
    let full_len = text.chars().count() as u64;
    let truncated = full_len > TEXT_CAP as u64;
    let content = if truncated {
        text.chars().take(TEXT_CAP).collect()
    } else {
        text
    };
    Ok(WorkFetchContent {
        url,
        content,
        truncated,
        size: full_len,
    })
}

/// Block-level tags that break a line in the extracted text.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "br"
            | "hr"
            | "tr"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "blockquote"
            | "pre"
            | "figure"
            | "figcaption"
            | "dd"
            | "dt"
            | "dl"
    )
}

/// Byte offset of `</name` (ASCII case-insensitive) in `hay`, if any.
/// A whole-string `to_lowercase` would shift byte indices on some Unicode,
/// so the comparison stays byte-wise.
fn find_close_tag(hay: &[u8], name: &str) -> Option<usize> {
    let needle = format!("</{name}");
    let nb = needle.as_bytes();
    if hay.len() < nb.len() {
        return None;
    }
    hay.windows(nb.len()).position(|w| w.eq_ignore_ascii_case(nb))
}

/// Contents of the first `<title>` tag, entity-decoded and flattened to one
/// line. News pages put the headline there, so it leads the fetched text.
fn extract_html_title(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    if bytes.len() < 6 {
        return None;
    }
    let open = bytes.windows(6).position(|w| w.eq_ignore_ascii_case(b"<title"))?;
    let after_open = html[open..].find('>')? + open + 1;
    let tail = &bytes[after_open..];
    if tail.len() < 8 {
        return None;
    }
    let rel = tail.windows(8).position(|w| w.eq_ignore_ascii_case(b"</title>"))?;
    let inner = &html[after_open..after_open + rel];
    // titles carry no markup, but strip tags defensively
    let mut text = String::with_capacity(inner.len());
    let mut skipping = false;
    for c in inner.chars() {
        if c == '<' {
            skipping = true;
            continue;
        }
        if c == '>' {
            skipping = false;
            continue;
        }
        if !skipping {
            text.push(c);
        }
    }
    let flat = decode_html_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() { None } else { Some(flat) }
}

/// One entity body (between `&` and `;`): the five XML predefines,
/// `&nbsp;` (as a plain space), and numeric `&#123;` / `&#x1F;` references.
/// Unknown entities return None and pass through untouched.
fn decode_entity(ent: &str) -> Option<String> {
    match ent {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" | "#39" => Some("'".to_string()),
        "nbsp" => Some(" ".to_string()),
        _ if ent.starts_with('#') => {
            let num = &ent[1..];
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse::<u32>().ok()
            };
            code.and_then(char::from_u32).map(|c| {
                if c == '\u{a0}' {
                    " ".to_string()
                } else {
                    c.to_string()
                }
            })
        }
        _ => None,
    }
}

/// Decode HTML entities in text. Unknown entities pass through untouched.
pub(crate) fn decode_html_entities(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some(semi) = bytes[i..]
                .iter()
                .position(|&b| b == b';')
                .filter(|&p| p > 0 && p <= 12)
                .map(|p| i + p)
            && let Some(d) = decode_entity(&s[i + 1..semi])
        {
            out.push_str(&d);
            i = semi + 1;
            continue;
        }
        // `&` without a valid entity (or any other byte) copies through;
        // multibyte chars copy whole so indices stay on char boundaries.
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

/// Strip HTML down to readable text: drop script/style blocks (Next.js pages
/// bury 100KB+ of JSON in `__NEXT_DATA__`), turn block tags into newlines,
/// strip the rest, decode entities, collapse whitespace. Single pass, no
/// dependencies; byte-wise with char-boundary-safe advances.
pub(crate) fn html_to_text(html: &str) -> String {
    let title = extract_html_title(html);
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                match html[i..].find("-->") {
                    Some(end) => i += end + 3,
                    None => break,
                }
                continue;
            }
            let mut j = i + 1;
            let closing = j < bytes.len() && bytes[j] == b'/';
            if closing {
                j += 1;
            }
            let name_start = j;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let name = html[name_start..j].to_lowercase();
            if name.is_empty() && !closing {
                // not a tag: doctype/processing-instruction skips to '>',
                // a bare `<` in prose stays literal
                if j < bytes.len() && (bytes[j] == b'!' || bytes[j] == b'?') {
                    i = html[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
                } else {
                    out.push('<');
                    i += 1;
                }
                continue;
            }
            if (name == "script" || name == "style") && !closing {
                match find_close_tag(&bytes[i..], &name) {
                    Some(p) => {
                        let rest = &html[i + p..];
                        i += p + rest.find('>').map(|q| q + 1).unwrap_or(rest.len());
                    }
                    None => break,
                }
                continue;
            }
            if is_block_tag(&name) {
                out.push('\n');
            }
            if name == "td" || name == "th" {
                out.push(' ');
            }
            i = html[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
            continue;
        }
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch_len = html[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&html[i..end]);
            i = end;
        }
    }
    let decoded = decode_html_entities(&out);
    let body = decoded
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match title {
        Some(t) if !t.is_empty() && !body.starts_with(&t) => format!("{t}\n\n{body}"),
        _ => body,
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_fetch_url(url: String) -> Result<WorkFetchContent, String> {
    work_fetch_url_logic(url).await
}
