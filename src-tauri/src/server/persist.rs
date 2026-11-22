// Server-side turn recording (`vicuna serve`).
//
// A Work turn on a server used to be persisted only by the window that
// started it: the bridge queued the turn's events, the window drained them,
// built the assistant bubble and saved it back. With the laptop closed the
// turn kept running but nothing reached the database (and a server restart
// lost the queue for good). The recorder below builds the very same message
// list on the server as the events arrive, so the server can write it itself.
//
// Everything here mirrors `runWorkTurn`'s `handleEvent` in ChatApp.svelte and
// the formatters in src/lib/work.ts byte for byte (rows written by either side
// render identically). Keep the two in sync: `recorder_matches_frontend_format`
// pins the output.
//
// Pure: no I/O, no locks. The orchestration lives in server_turns.rs.

use serde_json::{json, Value};

/// JS `s.slice(0, n)`: the first `n` UTF-16 code units (the frontend counts
/// those, not chars, so astral characters must count twice here too).
pub fn js_slice(s: &str, n: usize) -> String {
    let mut units = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let w = c.len_utf16();
        if units + w > n {
            // JS would keep a lone high surrogate here; a str can't, so the
            // whole character goes (only reachable on a split astral char)
            return s[..i].to_string();
        }
        units += w;
        end = i + c.len_utf8();
    }
    s[..end].to_string()
}

/// work.ts `escapeHtml` (& < > " only — not the apostrophe).
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn arg_string(args: &Value, key: &str, fallback: &str) -> String {
    // JS `String(args.x ?? fallback)`
    match args.get(key) {
        None | Some(Value::Null) => fallback.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// work.ts `toolLabel`. `raw_args` is the args JSON exactly as it arrived on
/// the wire (`JSON.stringify(JSON.parse(raw))` == raw for serde's compact
/// output, and keeps the key order serde_json's sorted maps would lose).
pub fn tool_label(name: &str, args: &Value, raw_args: &str) -> String {
    let s = |k: &str, f: &str| arg_string(args, k, f);
    let summary = match name {
        "exec" => s("command", ""),
        "exec_input" => s("text", ""),
        "exec_status" | "exec_kill" => s("id", "(shell)"),
        "read" | "write" | "list" | "view_image" => s("path", "."),
        "grep" => s("pattern", ""),
        "sessions" => s("query", ""),
        "read_session" => s("id", ""),
        "read_upload" => s("name", ""),
        "fetch_url" => s("url", ""),
        "mcp_add" | "mcp_remove" | "mcp_enable" | "mcp_disable" => s("name", ""),
        "mcp_list" => String::new(),
        "say" => s("text", ""),
        "ask" => s("question", ""),
        _ => raw_args.to_string(),
    };
    format!("{name} — {}", js_slice(&summary, 120))
}

/// work.ts `formatToolCallHead`.
pub fn tool_call_head(name: &str, args: &Value, raw_args: &str) -> String {
    format!(
        "\n\n<details class=\"tool-result\"><summary>{}</summary><pre><code>",
        escape_html(&tool_label(name, args, raw_args))
    )
}

/// work.ts `formatToolCallTail`.
pub fn tool_call_tail(output: &str) -> String {
    format!("{}</code></pre></details>", escape_html(&js_slice(output, 6000)))
}

/// toolFormat.ts `formatDisplayNote`: inside an open tool card the reply is
/// raw `<pre><code>`, so a harness note (an approved command with `<!--` in
/// it) must be escaped or it hides the rest of the reply.
pub fn display_note(text: &str, in_tool_card: bool) -> String {
    if in_tool_card { escape_html(text) } else { text.to_string() }
}

/// work.ts `formatAskRecord`.
pub fn ask_record(question: &str, answer: &str, dismissed: bool) -> String {
    let one = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let q = escape_html(&one(question));
    let a = if dismissed {
        "<span class=\"ask-log-dismissed\">Dismissed</span>".to_string()
    } else {
        format!("<strong>Answer:</strong> {}", escape_html(&one(answer)))
    };
    format!("\n\n<div class=\"ask-log\"><div class=\"ask-log-q\">{q}</div><div class=\"ask-log-a\">{a}</div></div>\n\n")
}

/// workGate.ts `workReplyModel`.
pub fn work_reply_model(reported: Option<&str>, picked: Option<&str>, engine: &str) -> String {
    if let Some(r) = reported.map(str::trim).filter(|r| !r.is_empty()) {
        return r.to_string();
    }
    let Some(p) = picked.map(str::trim).filter(|p| !p.is_empty()) else {
        return engine.to_string();
    };
    if matches!(engine, "claude" | "codex" | "fx" | "agy" | "pi") {
        format!("{engine}/{p}")
    } else {
        p.to_string()
    }
}

/// The event kinds whose effect is in the recorded messages. A window that
/// reattaches takes the recorder's snapshot instead of replaying these (they
/// would double the text); the rest (asks, session control, done) still flow.
pub fn is_content_event(kind: &str) -> bool {
    matches!(
        kind,
        "token" | "think" | "model" | "roundstart" | "roundend" | "displaynote" | "queuedrained" | "toolstart" | "toolend"
            | "bgcompleted" | "contextcompacted" | "status"
    )
}

pub fn assistant_placeholder(model: &str) -> Value {
    json!({ "role": "assistant", "content": "", "thinking": "", "model": model })
}

/// The display transcript of one session during a Work turn, built from the
/// turn's events exactly like the window builds it.
#[derive(Debug, Clone)]
pub struct Recorder {
    pub chat_id: String,
    pub messages: Vec<Value>,
    /// index of the live assistant bubble (moves when queued prompts drain)
    pub cur: usize,
    pub turn_model: String,
    workspace: Option<String>,
    pick: Option<String>,
    engine: String,
    /// in-flight tool call: (name, args, raw args JSON)
    pending_head: Option<(String, Value, String)>,
    /// the ask card still waiting for an answer (the askrequest event)
    pub pending_ask: Option<Value>,
    /// ask-log records answered while a tool-result <details> card is open,
    /// flushed right after </details> on toolend
    pending_ask_records: Vec<String>,
    /// messages changed since the last write
    pub dirty: bool,
    /// a user message was added (bumps the session's activity time)
    pub touched: bool,
}

impl Recorder {
    pub fn new(chat_id: &str, messages: Vec<Value>, cur: usize, pick: Option<String>, engine: &str) -> Self {
        let turn_model = messages
            .get(cur)
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| work_reply_model(None, pick.as_deref(), engine));
        let workspace = messages
            .get(cur)
            .and_then(|m| m.get("workspace"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self {
            chat_id: chat_id.to_string(),
            messages,
            cur,
            turn_model,
            workspace,
            pick,
            engine: engine.to_string(),
            pending_head: None,
            pending_ask: None,
            pending_ask_records: Vec::new(),
            dirty: false,
            touched: false,
        }
    }

    fn cur_mut(&mut self) -> Option<&mut serde_json::Map<String, Value>> {
        self.messages.get_mut(self.cur).and_then(Value::as_object_mut)
    }

    fn append(&mut self, s: &str) {
        if let Some(m) = self.cur_mut() {
            let c = m.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            m.insert("content".into(), Value::String(c + s));
            self.dirty = true;
        }
    }

    /// The live assistant text (what a worker report quotes).
    pub fn last_reply(&self) -> String {
        match self.messages.last() {
            Some(m) if m.get("role").and_then(Value::as_str) == Some("assistant") => {
                m.get("content").and_then(Value::as_str).unwrap_or("").to_string()
            }
            _ => String::new(),
        }
    }

    /// Apply one turn event. `raw` is its JSON text (for tool args order).
    pub fn apply(&mut self, ev: &Value, raw: &str) {
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        let text = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match kind {
            "token" | "think" => {
                let add = text("text");
                let model = self.turn_model.clone();
                if let Some(m) = self.cur_mut() {
                    // flushTokens rebuilds the bubble as an assistant message
                    // carrying the turn model and the stats so far
                    m.insert("role".into(), json!("assistant"));
                    let key = if kind == "token" { "content" } else { "thinking" };
                    let prev = m.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                    m.insert(key.into(), Value::String(prev + &add));
                    if !m.contains_key("thinking") {
                        m.insert("thinking".into(), json!(""));
                    }
                    m.insert("model".into(), json!(model));
                    self.dirty = true;
                }
            }
            "model" => {
                self.turn_model = work_reply_model(ev.get("model").and_then(Value::as_str), self.pick.as_deref(), &self.engine);
                let model = self.turn_model.clone();
                if let Some(m) = self.cur_mut() {
                    if m.get("role").and_then(Value::as_str) == Some("assistant")
                        && m.get("model").and_then(Value::as_str) != Some(model.as_str())
                    {
                        m.insert("model".into(), json!(model));
                        self.dirty = true;
                    }
                }
            }
            "roundend" => {
                if let Some(m) = self.cur_mut() {
                    if m.get("role").and_then(Value::as_str) == Some("assistant") {
                        let ms = m.get("genMs").and_then(Value::as_f64).unwrap_or(0.0)
                            + ev.get("gen_ms").and_then(Value::as_f64).unwrap_or(0.0);
                        m.insert("genMs".into(), json!(ms));
                        if let Some(tps) = ev.get("gen_tps").and_then(Value::as_f64) {
                            m.insert("genTps".into(), json!(tps));
                        }
                        let cost = m.get("genCost").and_then(Value::as_f64).unwrap_or(0.0)
                            + ev.get("gen_cost").and_then(Value::as_f64).unwrap_or(0.0);
                        if cost > 0.0 {
                            m.insert("genCost".into(), json!(cost));
                        }
                        self.dirty = true;
                    }
                }
            }
            "displaynote" => {
                let in_card = self.pending_head.as_ref().is_some_and(|(n, _, _)| n != "ask" && n != "say");
                self.append(&display_note(&text("text"), in_card));
            }
            "contextcompacted" => self.append(&format!("\n{}\n", text("note"))),
            "queuedrained" => {
                for q in ev.get("messages").and_then(Value::as_array).cloned().unwrap_or_default() {
                    let mut u = json!({ "role": "user", "content": q.get("content").and_then(Value::as_str).unwrap_or("") });
                    if let Some(imgs) = q.get("images").filter(|v| v.is_array()) {
                        u["images"] = imgs.clone();
                    }
                    self.messages.push(u);
                    let mut ph = assistant_placeholder(&self.turn_model);
                    if let Some(ws) = &self.workspace {
                        ph["workspace"] = json!(ws);
                    }
                    self.messages.push(ph);
                }
                self.cur = self.messages.len().saturating_sub(1);
                self.dirty = true;
                self.touched = true;
            }
            "toolstart" => {
                let name = text("name");
                let args = ev.get("args").cloned().unwrap_or(Value::Null);
                let raw_args = raw_field(raw, "args").unwrap_or_else(|| args.to_string());
                self.pending_head = Some((name.clone(), args.clone(), raw_args.clone()));
                if name == "ask" || name == "say" {
                    return;
                }
                self.append(&tool_call_head(&name, &args, &raw_args));
            }
            "toolend" => {
                let name = text("name");
                let output = text("output");
                let head = self.pending_head.take();
                if name == "say" {
                    let said = head.as_ref().map(|(_, a, _)| arg_string(a, "text", "")).unwrap_or_default();
                    let said = said.trim().to_string();
                    if !said.is_empty() && !output.starts_with("Error:") {
                        if let Some(m) = self.cur_mut() {
                            let c = m.get("content").and_then(Value::as_str).unwrap_or("").to_string();
                            let sep = if c.trim().is_empty() { "" } else { "\n\n" };
                            m.insert("content".into(), Value::String(format!("{c}{sep}{said}\n\n")));
                            self.dirty = true;
                        }
                    } else {
                        let (args, raw_args) = head.map(|(_, a, r)| (a, r)).unwrap_or((json!({}), "{}".into()));
                        let s = tool_call_head(&name, &args, &raw_args) + &tool_call_tail(&output);
                        self.append(&s);
                    }
                } else if name == "ask" {
                    // the legacy native `ask` tool: its record is written when
                    // the window answers (see record_answer)
                } else {
                    self.append(&tool_call_tail(&output));
                    let recs: Vec<String> = self.pending_ask_records.drain(..).collect();
                    for rec in recs {
                        self.append(&rec);
                    }
                }
            }
            "bgcompleted" => {
                let args = json!({ "command": format!("{} ({})", text("command"), text("job_id")) });
                let raw_args = args.to_string();
                let s = tool_call_head("exec", &args, &raw_args) + &tool_call_tail(&text("output"));
                self.append(&s);
            }
            "askrequest" => self.pending_ask = Some(ev.clone()),
            _ => {}
        }
    }

    /// A window answered an ask card. Agent questions (`ask-*`) get the same
    /// Q/A record the window writes into its own copy; permission asks get
    /// the backend's own Approved/Denied note instead.
    pub fn record_answer(&mut self, ask_id: &str, display: Option<&str>, dismissed: bool) {
        let question = match &self.pending_ask {
            Some(a) if a.get("ask_id").and_then(Value::as_str) == Some(ask_id) => {
                a.get("question").and_then(Value::as_str).unwrap_or("").to_string()
            }
            _ => return,
        };
        self.pending_ask = None;
        if let Some(d) = display {
            if ask_id.starts_with("ask-") {
                let rec = ask_record(&question, d, dismissed);
                let in_card = self.pending_head.as_ref().is_some_and(|(n, _, _)| n != "ask" && n != "say");
                if in_card {
                    self.pending_ask_records.push(rec);
                } else {
                    self.append(&rec);
                }
            }
        }
    }

    /// What a reattaching window needs to continue from: the transcript, the
    /// live index, the in-flight tool call and an unanswered ask card.
    pub fn snapshot(&self) -> Value {
        json!({
            "messages": self.messages,
            "index": self.cur,
            "pendingHead": self.pending_head.as_ref().map(|(n, a, _)| json!({ "name": n, "args": a })),
            "pendingAsk": self.pending_ask,
            "pendingAskRecords": self.pending_ask_records,
        })
    }
}

/// The raw JSON text of a top-level field of an event (`"args"` of a
/// toolstart), without re-serializing it.
fn raw_field(raw: &str, key: &str) -> Option<String> {
    let m: std::collections::HashMap<String, Box<serde_json::value::RawValue>> = serde_json::from_str(raw).ok()?;
    m.get(key).map(|r| r.get().to_string())
}

// ---------- model-visible history for server-started turns ----------

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// ChatApp `stripHarnessMarkup`: tool blocks out, ask records as prose.
pub fn strip_harness_markup(content: &str) -> String {
    let mut out = String::new();
    let mut rest = content;
    const DET: &str = "<details class=\"tool-result\">";
    const ASK: &str = "<div class=\"ask-log\">";
    loop {
        let d = rest.find(DET);
        let a = rest.find(ASK);
        let (pos, is_det) = match (d, a) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some(d), Some(a)) => if d < a { (d, true) } else { (a, false) },
            (Some(d), None) => (d, true),
            (None, Some(a)) => (a, false),
        };
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if is_det {
            match tail.find("</details>") {
                Some(e) => rest = &tail[e + "</details>".len()..],
                None => break, // unterminated block: drop to the end
            }
        } else {
            // <div class="ask-log"><div class="ask-log-q">Q</div><div class="ask-log-a">A</div></div>
            let parse = || -> Option<(String, usize)> {
                let q0 = tail.find("<div class=\"ask-log-q\">")? + "<div class=\"ask-log-q\">".len();
                let q1 = q0 + tail[q0..].find("</div>")?;
                let a0 = q1 + tail[q1..].find("<div class=\"ask-log-a\">")? + "<div class=\"ask-log-a\">".len();
                let a1 = a0 + tail[a0..].find("</div>")?;
                let end = a1 + "</div>".len();
                let close = end + tail[end..].find("</div>")? + "</div>".len();
                let (q, a) = (&tail[q0..q1], &tail[a0..a1]);
                let answer = if a.contains("ask-log-dismissed") {
                    "dismissed the question".to_string()
                } else {
                    let mut plain = String::new();
                    let mut in_tag = false;
                    for c in a.chars() {
                        match c {
                            '<' => in_tag = true,
                            '>' => in_tag = false,
                            c if !in_tag => plain.push(c),
                            _ => {}
                        }
                    }
                    let plain = plain.trim_start();
                    let plain = plain.strip_prefix("Answer:").map(str::trim_start).unwrap_or(plain);
                    format!("answered: {}", decode_entities(plain))
                };
                Some((format!("Asked the user: {} — they {answer}.", decode_entities(q)), close))
            };
            match parse() {
                Some((prose, used)) => {
                    out.push_str(&prose);
                    rest = &tail[used..];
                }
                None => break,
            }
        }
    }
    let mut s = out;
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    s.trim().to_string()
}

/// ChatApp `compactedHistory`: from the latest compaction summary on, with
/// assistant markup stripped (only matters for a fresh engine session's
/// history preamble — engines resume their own session context otherwise).
pub fn compacted_history(messages: &[Value]) -> Vec<Value> {
    let from = messages.iter().rposition(|m| m.get("summary").and_then(Value::as_bool) == Some(true)).unwrap_or(0);
    messages[from..]
        .iter()
        .map(|m| {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = m.get("content").and_then(Value::as_str).unwrap_or("");
            let content = if role == "assistant" { strip_harness_markup(content) } else { content.to_string() };
            let mut o = json!({ "role": role, "content": content });
            if let Some(imgs) = m.get("images").filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
                o["images"] = imgs.clone();
            }
            o
        })
        .collect()
}

// ---------- messages the server sends on its own ----------

/// leadSessions.ts `tailText`.
pub fn tail_text(text: &str, max: usize) -> String {
    let t = text.trim();
    let units: Vec<u16> = t.encode_utf16().collect();
    if units.len() <= max {
        return t.to_string();
    }
    let mut cut = String::from_utf16_lossy(&units[units.len() - max..]);
    if let Some(nl) = cut.find('\n') {
        if cut[..nl].encode_utf16().count() < 200 {
            cut = cut[nl + 1..].to_string();
        }
    }
    format!("…{cut}")
}

/// leadSessions.ts `workerDoneMessage` (`parseWorkerDone` reads it back).
pub fn worker_done_message(id: &str, title: &str, engine: &str, model: Option<&str>, reply: &str, stopped: bool) -> String {
    let how = if stopped { "stopped" } else { "finished its turn" };
    let who = match model {
        Some(m) if !m.is_empty() => format!("{engine}/{m}"),
        _ => engine.to_string(),
    };
    let body = if reply.trim().is_empty() { "(no reply text)".to_string() } else { tail_text(reply, 6000) };
    format!("Worker session finished: \"{title}\" ({id}, {who}) {how}.\nIts last reply:\n\n{body}\n\nUse manage_sessions (send/stop/delete) or read_session for more.")
}

/// backgroundRun.ts `mcpWakeMessage`.
pub fn mcp_wake_message(servers: &[(String, Option<usize>)]) -> String {
    let summary = servers
        .iter()
        .map(|(n, t)| match t {
            Some(t) => format!("`{n}` ({t} tool{})", if *t == 1 { "" } else { "s" }),
            None => format!("`{n}`"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("MCP server(s) now active: {summary}. You can call their tools now.")
}

/// schedule.ts `wakeMessage`.
pub fn wake_message(message: &str) -> String {
    format!("Scheduled wake-up (you asked Vicuna to wake you now): {}", message.trim())
}

// ---------- wake-up times (schedule.ts `resolveWakeTime`, no tz database) ----------

const MIN_MS: i64 = 60_000;
const WAKE_MAX_MS: i64 = 30 * 24 * 60 * MIN_MS;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Wall-clock fields in a fixed UTC offset → epoch ms (None for nonsense).
fn local_ms(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64, offset_min: i64) -> Option<i64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    if civil_from_days(days) != (y, mo, d) {
        return None; // Feb 31 and friends
    }
    Some(((days * 24 + h) * 60 + mi - offset_min) * MIN_MS + s * 1000)
}

fn nums(s: &str) -> Option<Vec<i64>> {
    s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().ok()).collect()
}

/// Resolve a `schedule_wakeup` time. The server has no tz database, so local
/// forms use the UTC offset the user's window last reported.
pub fn resolve_wake_time(at: Option<&str>, in_minutes: Option<f64>, now: i64, offset_min: i64) -> Result<i64, String> {
    let when = if let Some(m) = in_minutes.filter(|m| m.is_finite()) {
        now + (m * MIN_MS as f64).round() as i64
    } else if let Some(raw) = at.map(str::trim).filter(|s| !s.is_empty()) {
        let bad = || format!("can't read \"{raw}\" as a time (use HH:MM or YYYY-MM-DD HH:MM)");
        let b = raw.as_bytes();
        let is_hm = raw.len() <= 5 && raw.contains(':') && raw.chars().all(|c| c.is_ascii_digit() || c == ':');
        if is_hm {
            let n = nums(raw).filter(|n| n.len() == 2).ok_or_else(bad)?;
            if n[0] > 23 || n[1] > 59 {
                return Err(format!("\"{raw}\" is not a valid time"));
            }
            let local_now = now + offset_min * MIN_MS;
            let day = local_now.div_euclid(24 * 60 * MIN_MS);
            let (y, mo, d) = civil_from_days(day);
            let mut t = local_ms(y, mo, d, n[0], n[1], 0, offset_min).ok_or_else(bad)?;
            if t <= now {
                t += 24 * 60 * MIN_MS;
            }
            t
        } else if b.len() >= 16 && b[4] == b'-' && b[7] == b'-' && (b[10] == b' ' || b[10] == b'T') {
            // offset suffix: Z or ±HH:MM (ISO 8601); none = the user's local time
            let (body, off) = if let Some(stripped) = raw.strip_suffix('Z').or_else(|| raw.strip_suffix('z')) {
                (stripped, 0)
            } else if raw.len() > 19 && matches!(b[raw.len() - 6], b'+' | b'-') && b[raw.len() - 3] == b':' {
                let sign = if b[raw.len() - 6] == b'+' { 1 } else { -1 };
                let o = nums(&raw[raw.len() - 5..]).filter(|n| n.len() == 2).ok_or_else(bad)?;
                (&raw[..raw.len() - 6], sign * (o[0] * 60 + o[1]))
            } else {
                (raw, offset_min)
            };
            let body = body.split('.').next().unwrap_or(body); // drop fractional seconds
            let n = nums(body).filter(|n| n.len() == 5 || n.len() == 6).ok_or_else(bad)?;
            local_ms(n[0], n[1], n[2], n[3], n[4], n.get(5).copied().unwrap_or(0), off).ok_or_else(bad)?
        } else {
            return Err(bad());
        }
    } else {
        return Err("no time given".into());
    };
    if when < now + 30_000 {
        return Err("that time is in the past (or less than a minute away)".into());
    }
    if when > now + WAKE_MAX_MS {
        return Err("wake-ups can be at most 30 days ahead".into());
    }
    Ok(when)
}

/// `2026-11-16 21:30 UTC+8 (in 45 min)` for the tool result.
pub fn describe_when(at: i64, now: i64, offset_min: i64) -> String {
    let local = at + offset_min * MIN_MS;
    let (y, mo, d) = civil_from_days(local.div_euclid(24 * 60 * MIN_MS));
    let mins_of_day = local.rem_euclid(24 * 60 * MIN_MS) / MIN_MS;
    let zone = if offset_min == 0 {
        "UTC".to_string()
    } else {
        let (sign, a) = if offset_min > 0 { ('+', offset_min) } else { ('-', -offset_min) };
        if a % 60 == 0 { format!("UTC{sign}{}", a / 60) } else { format!("UTC{sign}{}:{:02}", a / 60, a % 60) }
    };
    let mins = ((at - now) as f64 / MIN_MS as f64).round() as i64;
    let rel = if mins >= 120 { format!("{} h", (mins as f64 / 60.0).round() as i64) } else { format!("{mins} min") };
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02} {zone} (in {rel})", mins_of_day / 60, mins_of_day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(v: Value) -> (Value, String) {
        let raw = v.to_string();
        (v, raw)
    }

    /// Replays the events through the recorder exactly like ChatApp's
    /// handleEvent; the expected strings are what the window writes.
    #[test]
    fn recorder_matches_frontend_format() {
        let msgs = vec![json!({"role": "user", "content": "list files"}), assistant_placeholder("opencode")];
        let mut r = Recorder::new("c1", msgs, 1, None, "opencode");
        let raw_start = r#"{"type":"toolstart","name":"bash","args":{"command":"ls -la","description":"List <files>"}}"#;
        let steps: Vec<(Value, String)> = vec![
            ev(json!({"type": "model", "model": "opencode/deepseek"})),
            ev(json!({"type": "think", "text": "Let me "})),
            ev(json!({"type": "think", "text": "look."})),
            ev(json!({"type": "token", "text": "Listing"})),
            ev(json!({"type": "token", "text": " now."})),
            (serde_json::from_str(raw_start).unwrap(), raw_start.to_string()),
            ev(json!({"type": "toolend", "name": "bash", "output": "a.txt & <b>"})),
            ev(json!({"type": "displaynote", "text": "\n\n_Auto-approved: bash_\n\n"})),
            ev(json!({"type": "toolstart", "name": "say", "args": {"text": "Halfway there"}})),
            ev(json!({"type": "toolend", "name": "say", "output": "ok"})),
            ev(json!({"type": "roundend", "gen_ms": 1200, "gen_tps": 33.5, "gen_cost": 0.002})),
            ev(json!({"type": "roundend", "gen_ms": 800, "gen_tps": null, "gen_cost": null})),
        ];
        for (v, raw) in &steps {
            r.apply(v, raw);
        }
        let m = &r.messages[1];
        assert_eq!(m["model"], "opencode/deepseek");
        assert_eq!(m["thinking"], "Let me look.");
        // key order of the label follows the wire JSON (command before description)
        assert_eq!(
            m["content"],
            "Listing now.\n\n<details class=\"tool-result\"><summary>bash — {&quot;command&quot;:&quot;ls -la&quot;,&quot;description&quot;:&quot;List &lt;files&gt;&quot;}</summary><pre><code>a.txt &amp; &lt;b&gt;</code></pre></details>\n\n_Auto-approved: bash_\n\n\n\nHalfway there\n\n"
        );
        assert_eq!(m["genMs"], 2000.0);
        assert_eq!(m["genTps"], 33.5);
        assert_eq!(m["genCost"], 0.002);
        assert!(r.dirty);

        // queued prompt: user bubble + fresh assistant bubble, index moves
        r.apply(&json!({"type": "queuedrained", "ids": ["q1"], "messages": [{"role": "user", "content": "and now?"}]}), "");
        assert_eq!(r.messages.len(), 4);
        assert_eq!(r.cur, 3);
        assert_eq!(r.messages[2], json!({"role": "user", "content": "and now?"}));
        assert_eq!(r.messages[3], json!({"role": "assistant", "content": "", "thinking": "", "model": "opencode/deepseek"}));
        assert!(r.touched);

        // agent question answered from a window: the window's Q/A record
        r.apply(&json!({"type": "askrequest", "ask_id": "ask-3", "question": "Which  fruit?", "options": ["Kiwi"], "multi": false}), "");
        assert!(r.snapshot()["pendingAsk"].is_object());
        r.record_answer("ask-3", Some("Kiwi <3"), false);
        assert_eq!(
            r.messages[3]["content"],
            "\n\n<div class=\"ask-log\"><div class=\"ask-log-q\">Which fruit?</div><div class=\"ask-log-a\"><strong>Answer:</strong> Kiwi &lt;3</div></div>\n\n"
        );
        assert!(r.snapshot()["pendingAsk"].is_null());
        // permission asks leave no record of their own
        r.apply(&json!({"type": "askrequest", "ask_id": "perm-4", "question": "Run rm?", "options": ["Allow"], "multi": false}), "");
        r.record_answer("perm-4", Some("Allow"), false);
        assert!(r.messages[3]["content"].as_str().unwrap().ends_with("</div>\n\n"));

        // agent question answered while its ask_user tool card is open:
        // the ask-log record must land AFTER </details>, never inside the
        // collapsed tool-result block
        let ask_start = r#"{"type":"toolstart","name":"mcp_vicuna-mcp_ask_user","args":{"question":"Proceed?"}}"#;
        r.apply(&serde_json::from_str(ask_start).unwrap(), ask_start);
        r.apply(&json!({"type": "askrequest", "ask_id": "ask-5", "question": "Proceed?", "options": ["Yes", "No"], "multi": false}), "");
        r.record_answer("ask-5", Some("Yes"), false);
        assert_eq!(r.snapshot()["pendingAskRecords"].as_array().unwrap().len(), 1);
        assert!(!r.messages[3]["content"].as_str().unwrap().contains("Proceed?</div>"), "must wait for </details>");
        r.apply(&json!({"type": "toolend", "name": "mcp_vicuna-mcp_ask_user", "output": "The user answered: Yes"}), "");
        assert!(r.snapshot()["pendingAskRecords"].as_array().unwrap().is_empty());
        assert!(
            r.messages[3]["content"].as_str().unwrap().ends_with(
                "The user answered: Yes</code></pre></details>\n\n<div class=\"ask-log\"><div class=\"ask-log-q\">Proceed?</div><div class=\"ask-log-a\"><strong>Answer:</strong> Yes</div></div>\n\n"
            ),
            "ask-log must sit right after </details>: {}",
            r.messages[3]["content"]
        );

        // background completion block
        r.apply(&json!({"type": "bgcompleted", "job_id": "bg-1", "command": "make", "output": "ok"}), "");
        assert!(r.messages[3]["content"].as_str().unwrap().ends_with(
            "\n\n<details class=\"tool-result\"><summary>exec — make (bg-1)</summary><pre><code>ok</code></pre></details>"
        ));
        assert_eq!(r.last_reply(), r.messages[3]["content"].as_str().unwrap());
    }

    /// The same cases node-tests/serverRecorder.test.ts runs through the real
    /// work.ts / leadSessions.ts / backgroundRun.ts / schedule.ts functions.
    #[test]
    fn formatters_match_the_shared_frontend_fixture() {
        let text = include_str!("../../../node-tests/fixtures/server-format.json");
        let cases: Vec<Value> = serde_json::from_str(text).unwrap();
        // raw args text, in wire order (serde_json's maps sort keys)
        type RawMap = std::collections::HashMap<String, Box<serde_json::value::RawValue>>;
        let raws: Vec<RawMap> = serde_json::from_str(text).unwrap();
        assert!(cases.len() >= 8);
        for (c, raw) in cases.into_iter().zip(raws) {
            let args_raw: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(raw["args"].get()).unwrap();
            // compact, key order kept: what JSON.stringify gives the window
            let raw_arg1 = args_raw.get(1).map(|r| {
                let mut out = String::new();
                compact_keep_order(r.get(), &mut out);
                out
            });
            let a = &c["args"];
            let s = |i: usize| a[i].as_str().unwrap_or("").to_string();
            let got = match c["fn"].as_str().unwrap() {
                "formatToolCallHead" => tool_call_head(&s(0), &a[1], raw_arg1.as_deref().unwrap_or("{}")),
                "formatToolCallTail" => tool_call_tail(&s(0)),
                "formatAskRecord" => ask_record(&s(0), &s(1), a[2].as_bool().unwrap()),
                "workReplyModel" => work_reply_model(a[0].as_str(), a[1].as_str(), &s(2)),
                "workerDoneMessage" => worker_done_message(
                    a[0]["id"].as_str().unwrap(),
                    a[0]["title"].as_str().unwrap(),
                    a[0]["engine"].as_str().unwrap(),
                    a[0]["model"].as_str(),
                    &s(1),
                    a[2].as_bool().unwrap(),
                ),
                "mcpWakeMessage" => mcp_wake_message(
                    &a[0].as_array().unwrap().iter().map(|x| (x["name"].as_str().unwrap().to_string(), x["tools"].as_u64().map(|n| n as usize))).collect::<Vec<_>>(),
                ),
                "wakeMessage" => wake_message(&s(0)),
                "formatDisplayNote" => display_note(&s(0), a[1].as_bool().unwrap()),
                other => panic!("unknown fixture fn {other}"),
            };
            assert_eq!(got, c["expected"].as_str().unwrap(), "{c}");
        }
    }

    /// Strip insignificant whitespace from pretty JSON without reordering.
    fn compact_keep_order(src: &str, out: &mut String) {
        let mut in_str = false;
        let mut esc = false;
        for c in src.chars() {
            if in_str {
                out.push(c);
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
            } else if c == '"' {
                in_str = true;
                out.push(c);
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
    }

    #[test]
    fn js_slices_count_utf16_units() {
        assert_eq!(js_slice("abc", 2), "ab");
        assert_eq!(js_slice("é😀x", 3), "é😀"); // 1 + 2 units
        assert_eq!(js_slice("é😀x", 2), "é"); // never splits the emoji
        let long = "x".repeat(7000);
        assert_eq!(tool_call_tail(&long).len(), 6000 + "</code></pre></details>".len());
        assert_eq!(tool_label("exec", &json!({"command": "y".repeat(200)}), "{}").chars().count(), "exec — ".chars().count() + 120);
    }

    #[test]
    fn history_strips_markup_like_the_frontend() {
        let a = "Done.\n\n<details class=\"tool-result\"><summary>x</summary><pre><code>out</code></pre></details>\n\n\n\nAlso <div class=\"ask-log\"><div class=\"ask-log-q\">Which?</div><div class=\"ask-log-a\"><strong>Answer:</strong> Kiwi &amp; lime</div></div> end";
        assert_eq!(strip_harness_markup(a), "Done.\n\nAlso Asked the user: Which? — they answered: Kiwi & lime. end");
        let h = compacted_history(&[
            json!({"role": "user", "content": "old"}),
            json!({"role": "assistant", "content": "summary", "summary": true}),
            json!({"role": "user", "content": "new", "images": ["x"]}),
        ]);
        assert_eq!(h, vec![json!({"role": "assistant", "content": "summary"}), json!({"role": "user", "content": "new", "images": ["x"]})]);
    }

    #[test]
    fn server_messages_match_frontend_text() {
        let m = worker_done_message("w1", "Build", "claude", Some("opus"), "  all good  ", false);
        assert_eq!(m, "Worker session finished: \"Build\" (w1, claude/opus) finished its turn.\nIts last reply:\n\nall good\n\nUse manage_sessions (send/stop/delete) or read_session for more.");
        assert_eq!(mcp_wake_message(&[("gh".into(), Some(1)), ("x".into(), None)]), "MCP server(s) now active: `gh` (1 tool), `x`. You can call their tools now.");
        assert_eq!(wake_message(" check CI "), "Scheduled wake-up (you asked Vicuna to wake you now): check CI");
        assert_eq!(work_reply_model(None, Some("opus"), "claude"), "claude/opus");
        assert_eq!(work_reply_model(None, None, "opencode"), "opencode");
    }

    #[test]
    fn wake_times_resolve_in_the_reported_offset() {
        // 2026-11-16 12:00:00 UTC
        let now = local_ms(2026, 11, 16, 12, 0, 0, 0).unwrap();
        assert_eq!(resolve_wake_time(None, Some(10.0), now, 0).unwrap(), now + 600_000);
        // 21:30 at UTC+8 is 13:30 UTC, still today
        assert_eq!(resolve_wake_time(Some("21:30"), None, now, 480).unwrap(), local_ms(2026, 11, 16, 13, 30, 0, 0).unwrap());
        // 19:00 at UTC+8 (11:00 UTC) already passed: tomorrow
        assert_eq!(resolve_wake_time(Some("19:00"), None, now, 480).unwrap(), local_ms(2026, 11, 17, 11, 0, 0, 0).unwrap());
        assert_eq!(resolve_wake_time(Some("2026-11-17 09:15"), None, now, -300).unwrap(), local_ms(2026, 11, 17, 14, 15, 0, 0).unwrap());
        assert_eq!(resolve_wake_time(Some("2026-11-17T09:15:00+02:00"), None, now, 480).unwrap(), local_ms(2026, 11, 17, 7, 15, 0, 0).unwrap());
        assert_eq!(resolve_wake_time(Some("2026-11-17T09:15:00.500Z"), None, now, 480).unwrap(), local_ms(2026, 11, 17, 9, 15, 0, 0).unwrap());
        assert!(resolve_wake_time(Some("2026-02-31 09:00"), None, now, 0).is_err());
        assert!(resolve_wake_time(Some("25:00"), None, now, 0).is_err());
        assert!(resolve_wake_time(None, Some(0.1), now, 0).is_err());
        assert!(resolve_wake_time(None, Some(60.0 * 24.0 * 31.0), now, 0).is_err());
        assert!(resolve_wake_time(Some("soon"), None, now, 0).is_err());
        assert_eq!(describe_when(now + 45 * 60_000, now, 480), "2026-11-16 20:45 UTC+8 (in 45 min)");
    }
}
