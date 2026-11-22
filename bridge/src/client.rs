// `vicuna session|schedule|engines|models|mcp|server …` — a thin client of a
// running `vicuna serve` (local by default, or a saved/remote server).
//
// Every command goes through the same token-authed `/invoke` + long-poll
// `/poll/<channel>` routes the desktop's server windows use, so sessions
// started here show up in the app and vice versa, and the turns run (and are
// recorded) on the server: detaching or closing the terminal never stops them.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vicuna_lib::servers;

pub const SESSION_USAGE: &str = "vicuna session — drive Work sessions on a Vicuna server

usage (alias: vicuna s …):
  vicuna session list [--all] [--archived] [-w DIR] [-e HARNESS] [--json]
  vicuna session new -w DIR [-e HARNESS] [-m MODEL] [--mcp a,b|none] [--title T]
                     [-p PROMPT | PROMPT…] [--detach] [--json] [--no-think]
        no prompt on a terminal = interactive chat (empty line / Ctrl-D quits)
  vicuna session send ID [PROMPT…] [--detach] [--json]   continue (queues if running)
  vicuna session chat ID                interactive chat in a session
  vicuna session view ID [--tail N] [--follow] [--json] [--raw]
  vicuna session attach ID              follow the live turn, answer its questions
  vicuna session stop ID
  vicuna session set ID [-e HARNESS] [-m MODEL|default] [-w DIR] [--title T]
                        [--mcp a,b|none|default]
  vicuna session archive|unarchive ID
  vicuna session rename ID TITLE…
  vicuna session pin|unpin|lead|unlead ID     (lead also pins)
  vicuna session fork ID AT                   new session with messages 0..=AT
  vicuna session edit ID INDEX TEXT… [--truncate]   rewrite one message
  vicuna session rm-msg ID INDEX              delete a message (+ its pair)
  vicuna session delete ID [-y]

  ID is a session id or a unique prefix of one. HARNESS: claude, codex,
  opencode, pi, fx, agy. --mcp picks the MCP servers by name (from
  `vicuna mcp list`, enabled or not); default = the app's enabled servers.

  While following a turn: Ctrl-C once detaches after 2 s (the turn keeps
  running on the server), twice stops it. Exit codes: 0 done, 1 error,
  2 stopped, 3 detached.

server selection (every client command):
  --server URL|NAME  or  VICUNA_SERVER  (saved servers: `vicuna server list`);
  default http://127.0.0.1:7421 with ~/vicuna/server/token (or VICUNA_SERVER_TOKEN)";

pub const SCHEDULE_USAGE: &str = "vicuna schedule — scheduled sends (delivered by the server)

usage:
  vicuna schedule list [--json]
  vicuna schedule add ID MESSAGE… (--in 30m|2h|1d | --at HH:MM|\"YYYY-MM-DD HH:MM\"|ISO)
  vicuna schedule add --new -w DIR [-e HARNESS] [-m MODEL] [--mcp a,b] MESSAGE… (--in …|--at …)
  vicuna schedule rm SCHEDULE_ID…";

pub const MCP_USAGE: &str = "vicuna mcp — MCP servers on the Vicuna server (~/vicuna/config/vicuna.json there)

usage:
  vicuna mcp list [--json]
  vicuna mcp add NAME (--url URL [--header K=V]… | --command \"CMD ARGS\" [--env K=V]…) [--disabled]
  vicuna mcp enable|disable|rm NAME
  vicuna mcp tools NAME";

pub const SERVER_USAGE: &str = "vicuna server — saved servers for the client commands

usage:
  vicuna server list
  vicuna server add URL [--token TOKEN]   verify + save (bare *.ts.net = https)
  vicuna server rm URL|NAME";

const EXIT_STOPPED: i32 = 2;
const EXIT_DETACHED: i32 = 3;

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("vicuna: {msg}");
    std::process::exit(1)
}

fn usage_fail(msg: &str, usage: &str) -> ! {
    eprintln!("vicuna: {msg}\n\n{usage}");
    std::process::exit(2)
}

// ---------- args ----------

/// Flags (`--x`, `-x`, with or without a value) plus positionals, in order.
#[derive(Default, Debug)]
pub struct Args {
    pub pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

/// Flags that take a value (others are booleans).
const VALUED: &[&str] = &[
    "-w", "--workspace", "-e", "--engine", "--harness", "-m", "--model", "--mcp", "--title", "-p", "--prompt", "--tail",
    "--server", "--in", "--at", "--url", "--command", "--header", "--env", "--token", "--wait",
];

impl Args {
    pub fn parse(raw: &[String]) -> Args {
        let mut a = Args::default();
        let mut it = raw.iter();
        let mut only_pos = false;
        while let Some(x) = it.next() {
            if only_pos || !x.starts_with('-') || x == "-" {
                a.pos.push(x.clone());
                continue;
            }
            if x == "--" {
                only_pos = true;
                continue;
            }
            if let Some((k, v)) = x.split_once('=').filter(|(k, _)| k.starts_with("--")) {
                a.flags.push((k.to_string(), Some(v.to_string())));
            } else if VALUED.contains(&x.as_str()) {
                let v = it.next().cloned().unwrap_or_else(|| fail(format!("{x} needs a value")));
                a.flags.push((x.clone(), Some(v)));
            } else {
                a.flags.push((x.clone(), None));
            }
        }
        a
    }
    pub fn has(&self, names: &[&str]) -> bool {
        self.flags.iter().any(|(k, _)| names.contains(&k.as_str()))
    }
    pub fn get(&self, names: &[&str]) -> Option<String> {
        self.flags.iter().rev().find(|(k, _)| names.contains(&k.as_str())).and_then(|(_, v)| v.clone())
    }
    pub fn all(&self, names: &[&str]) -> Vec<String> {
        self.flags.iter().filter(|(k, _)| names.contains(&k.as_str())).filter_map(|(_, v)| v.clone()).collect()
    }
    /// Refuse flags a command doesn't know (typos must not be ignored).
    fn only(&self, allowed: &[&str], usage: &str) {
        for (k, _) in &self.flags {
            if !allowed.contains(&k.as_str()) && k != "--server" && k != "--json" {
                usage_fail(&format!("unknown option {k}"), usage);
            }
        }
    }
}

/// `--mcp a,b` → `["a","b"]`; `none` → `[]`; `default` → JSON null.
pub fn mcp_arg(v: &str) -> Value {
    match v.trim() {
        "default" => Value::Null,
        "none" | "" => json!([]),
        s => json!(s.split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>()),
    }
}

// ---------- server connection ----------

pub struct Client {
    url: String,
    token: String,
    http: reqwest::Client,
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into()))
}

/// `--server` / `VICUNA_SERVER`: a saved server (name, id or url), or a url
/// (token from `VICUNA_SERVER_TOKEN` or the url's `#token`). None: the local
/// `vicuna serve` on 7421 with ~/vicuna/server/token.
fn resolve_server(sel: Option<String>) -> Result<(String, String), String> {
    let env_token = std::env::var("VICUNA_SERVER_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let sel = sel.or_else(|| std::env::var("VICUNA_SERVER").ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let Some(sel) = sel else {
        let token = match env_token {
            Some(t) => t,
            None => std::fs::read_to_string(home().join("vicuna").join("server").join("token"))
                .map(|t| t.trim().to_string())
                .map_err(|_| "no local server token (~/vicuna/server/token). Start one with `vicuna start`, or pass --server".to_string())?,
        };
        // the desktop app's embedded server writes its port when 7421 was taken
        let port = std::fs::read_to_string(home().join("vicuna").join("server").join("port"))
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or(7421);
        return Ok((format!("http://127.0.0.1:{port}"), token));
    };
    let saved = servers::load();
    if let Some(s) = saved.iter().find(|s| s.name == sel || s.id == sel || s.url == sel) {
        return Ok((s.url.clone(), env_token.unwrap_or_else(|| s.token.clone())));
    }
    let (url, frag) = servers::parse_server_url(&sel)?;
    if let Some(s) = saved.iter().find(|s| s.url == url) {
        return Ok((url, env_token.or(frag).unwrap_or_else(|| s.token.clone())));
    }
    let token = env_token.or(frag).ok_or_else(|| format!("no token for {url}: run `vicuna server add {url} --token …` or set VICUNA_SERVER_TOKEN"))?;
    Ok((url, token))
}

impl Client {
    pub fn new(a: &Args) -> Client {
        let (url, token) = resolve_server(a.get(&["--server"])).unwrap_or_else(|e| fail(e));
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap_or_else(|e| fail(e));
        Client { url, token, http }
    }

    async fn post(&self, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, String> {
        let full = format!("{}{path}", self.url);
        let rb = match body {
            Some(b) => self.http.post(&full).json(b),
            None => self.http.get(&full),
        };
        let res = rb.bearer_auth(&self.token).timeout(timeout).send().await.map_err(|e| {
            if e.is_connect() {
                if self.url.starts_with("http://127.0.0.1") {
                    format!("no Vicuna server at {} (start one with `vicuna start`)", self.url)
                } else {
                    format!("can't reach {}: {e}", self.url)
                }
            } else {
                format!("{}: {e}", self.url)
            }
        })?;
        if res.status().as_u16() == 401 {
            return Err(format!("{} rejected the token (see `vicuna token` on the server)", self.url));
        }
        res.json().await.map_err(|e| format!("bad reply from {}: {e}", self.url))
    }

    pub async fn invoke(&self, cmd: &str, args: Value) -> Result<Value, String> {
        let v = self.post("/invoke", Some(&json!({ "cmd": cmd, "args": args })), Duration::from_secs(120)).await?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(v.get("data").cloned().unwrap_or(Value::Null));
        }
        let e = v.get("error").and_then(Value::as_str).unwrap_or("request failed").to_string();
        Err(if e.contains("unknown bridge command") {
            format!("{} runs an older `vicuna serve` without this command; upgrade it (`vicuna upgrade`)", self.url)
        } else {
            e
        })
    }

    pub async fn session(&self, action: &str, mut args: Value) -> Result<Value, String> {
        args["action"] = json!(action);
        self.invoke("cli_session", args).await
    }

    async fn poll(&self, channel: &str) -> Result<Vec<Value>, String> {
        let v = self.post(&format!("/poll/{channel}?wait=15000"), None, Duration::from_secs(30)).await?;
        Ok(v.get("events").and_then(Value::as_array).cloned().unwrap_or_default())
    }
}

// ---------- rendering ----------

struct Style {
    tty: bool,
}

impl Style {
    fn dim(&self, s: &str) -> String {
        if self.tty { format!("\x1b[2m{s}\x1b[0m") } else { s.to_string() }
    }
    fn bold(&self, s: &str) -> String {
        if self.tty { format!("\x1b[1m{s}\x1b[0m") } else { s.to_string() }
    }
    fn accent(&self, s: &str) -> String {
        if self.tty { format!("\x1b[36m{s}\x1b[0m") } else { s.to_string() }
    }
    fn red(&self, s: &str) -> String {
        if self.tty { format!("\x1b[31m{s}\x1b[0m") } else { s.to_string() }
    }
}

fn clip(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n { format!("{}…", one.chars().take(n.saturating_sub(1)).collect::<String>()) } else { one }
}

/// The one-line summary of a tool call's input.
pub fn tool_summary(args: &Value) -> String {
    if let Some(s) = args.as_str() {
        return clip(s, 100);
    }
    for k in ["command", "cmd", "file_path", "filePath", "path", "pattern", "query", "url", "question", "description"] {
        if let Some(v) = args.get(k) {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                _ => continue,
            };
            if !s.trim().is_empty() {
                return clip(&s, 100);
            }
        }
    }
    match args {
        Value::Null => String::new(),
        Value::Object(o) if o.is_empty() => String::new(),
        v => clip(&v.to_string(), 100),
    }
}

pub fn fmt_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// A stored assistant message as terminal text: tool blocks become
/// `▸ summary` lines, ask records `? Q → A`, other tags are dropped.
pub fn plain_transcript(content: &str) -> String {
    const DET: &str = "<details class=\"tool-result\">";
    let mut out = String::new();
    let mut rest = content;
    while let Some(p) = rest.find(DET) {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let summary = tail
            .find("<summary>")
            .and_then(|s0| tail[s0 + 9..].find("</summary>").map(|e| &tail[s0 + 9..s0 + 9 + e]))
            .unwrap_or("tool");
        out.push_str(&format!("\n▸ {}\n", clip(&html_unescape(summary), 120)));
        match tail.find("</details>") {
            Some(e) => rest = &tail[e + "</details>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    // ask records + any remaining tags
    let out = out
        .replace("<div class=\"ask-log\"><div class=\"ask-log-q\">", "\n? ")
        .replace("</div><div class=\"ask-log-a\">", " → ");
    let mut plain = String::new();
    let mut in_tag = false;
    for c in out.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let plain = html_unescape(&plain);
    // collapse runs of blank lines
    let mut lines: Vec<&str> = Vec::new();
    for l in plain.lines() {
        if l.trim().is_empty() && lines.last().is_some_and(|p| p.trim().is_empty()) {
            continue;
        }
        lines.push(l);
    }
    lines.join("\n").trim().to_string()
}

/// Follows one event channel and prints it.
struct Renderer {
    st: Style,
    json: bool,
    think: bool,
    /// what was printed last: 't' token, 'h' think, 'n' line
    last: char,
    model: Option<String>,
    /// something was printed since the last round footer
    fresh: bool,
}

impl Renderer {
    fn new(json: bool, think: bool) -> Renderer {
        Renderer { st: Style { tty: std::io::stdout().is_terminal() }, json, think, last: 'n', model: None, fresh: false }
    }
    fn line(&mut self, s: &str) {
        if self.last != 'n' {
            println!();
        }
        println!("{s}");
        self.last = 'n';
    }
    /// Prints one event; returns Some(exit code) on the terminal event.
    fn event(&mut self, ev: &Value) -> Option<Result<bool, String>> {
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        if self.json {
            println!("{ev}");
        }
        let text = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if !matches!(kind, "model" | "roundend" | "status" | "roundstart" | "") {
            self.fresh = true;
        }
        match kind {
            "done" => return Some(Ok(ev.get("stopped").and_then(Value::as_bool) == Some(true))),
            "error" => return Some(Err(text("message"))),
            _ if self.json => {}
            "token" => {
                if self.last == 'h' {
                    println!("\n");
                }
                print!("{}", text("text"));
                self.last = 't';
            }
            "think" if self.think => {
                if self.last == 't' {
                    println!();
                }
                print!("{}", self.st.dim(&text("text")));
                self.last = 'h';
            }
            "model" => self.model = Some(text("model")).filter(|m| !m.is_empty()),
            "toolstart" => {
                let s = tool_summary(ev.get("args").unwrap_or(&Value::Null));
                let name = text("name");
                let l = if s.is_empty() { format!("▸ {name}") } else { format!("▸ {name}: {s}") };
                self.line(&self.st.accent(&l));
            }
            "toolend" => {
                let out = text("output");
                let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                let n = out.lines().count();
                if !first.is_empty() {
                    let more = if n > 1 { format!(" (+{} lines)", n - 1) } else { String::new() };
                    self.line(&self.st.dim(&format!("  ↳ {}{more}", clip(first, 100))));
                }
            }
            "displaynote" => self.line(&self.st.dim(&format!("· {}", clip(&text("text"), 300)))),
            "contextcompacted" => self.line(&self.st.dim(&format!("· context compacted: {}", clip(&text("note"), 200)))),
            "bgcompleted" => self.line(&self.st.dim(&format!("· background job {} finished: {}", text("job_id"), clip(&text("command"), 80)))),
            "queuedrained" => {
                for m in ev.get("messages").and_then(Value::as_array).into_iter().flatten() {
                    let c = m.get("content").and_then(Value::as_str).unwrap_or("");
                    self.line(&self.st.bold(&format!("❯ {}", clip(c, 200))));
                }
            }
            // an empty round (e.g. a resumed engine's no-op result) gets no footer
            "roundend" if !self.fresh => {}
            "roundend" => {
                self.fresh = false;
                let mut parts = vec![self.model.clone().unwrap_or_default()];
                if let Some(ms) = ev.get("gen_ms").and_then(Value::as_u64) {
                    parts.push(fmt_duration(ms));
                }
                if let Some(t) = ev.get("gen_tps").and_then(Value::as_f64) {
                    parts.push(format!("{t:.1} tok/s"));
                }
                if let Some(c) = ev.get("gen_cost").and_then(Value::as_f64).filter(|c| *c > 0.0) {
                    parts.push(format!("${c:.4}"));
                }
                let footer = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
                self.line(&self.st.dim(&format!("— {footer}")));
            }
            _ => {}
        }
        let _ = std::io::stdout().flush();
        None
    }
}

// ---------- asks ----------

fn read_line_blocking(prompt: &str) -> Option<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\n', '\r']).to_string()),
    }
}

async fn read_line(prompt: String) -> Option<String> {
    tokio::task::spawn_blocking(move || read_line_blocking(&prompt)).await.ok().flatten()
}

/// The answer string for an ask card, from what the user typed: a number
/// picks an option (several, comma separated, for multi-select), anything
/// else is a free-text answer.
pub fn ask_answer(input: &str, options: &[String], multi: bool) -> String {
    let t = input.trim();
    let pick = |s: &str| s.trim().parse::<usize>().ok().filter(|n| *n >= 1 && *n <= options.len()).map(|n| options[n - 1].clone());
    if multi {
        let picks: Option<Vec<String>> = t.split(',').map(pick).collect();
        if let Some(p) = picks.filter(|p| !p.is_empty()) {
            return json!(p).to_string();
        }
        return json!([t]).to_string();
    }
    pick(t).unwrap_or_else(|| t.to_string())
}

async fn answer_ask(c: &Client, r: &Renderer, turn_id: &str, ev: &Value) -> Result<(), String> {
    let ask_id = ev.get("ask_id").and_then(Value::as_str).unwrap_or("").to_string();
    let question = ev.get("question").and_then(Value::as_str).unwrap_or("").to_string();
    let options: Vec<String> = ev.get("options").and_then(Value::as_array).into_iter().flatten().filter_map(|o| o.as_str().map(str::to_string)).collect();
    let multi = ev.get("multi").and_then(Value::as_bool) == Some(true);
    let perm = ask_id.starts_with("perm-");
    eprintln!();
    eprintln!("{} {}", r.st.bold(if perm { "Permission:" } else { "Question:" }), question);
    for (i, o) in options.iter().enumerate() {
        eprintln!("  {}. {o}", i + 1);
    }
    let hint = match (perm, multi, options.is_empty()) {
        (true, _, _) => "number",
        (_, _, true) => "answer",
        (_, true, _) => "numbers (1,3) or text",
        _ => "number or text",
    };
    let input = loop {
        match read_line(format!("{} › ", hint)).await {
            None => {
                // EOF (or no stdin): leave the card for the app / `attach`
                eprintln!("{}", r.st.dim("(not answered here; answer it in the app or with `vicuna session attach`)"));
                return Ok(());
            }
            Some(s) if s.trim().is_empty() => continue,
            Some(s) => break s,
        }
    };
    let answer = ask_answer(&input, &options, multi && !perm);
    let display = if multi && !perm {
        serde_json::from_str::<Vec<String>>(&answer).map(|v| v.join("; ")).unwrap_or(answer.clone())
    } else {
        answer.clone()
    };
    let mut args = json!({ "turnId": turn_id, "askId": ask_id, "answer": answer });
    if !perm {
        args["record"] = json!({ "display": format!("**Answer:** {display}"), "dismissed": false });
    }
    c.invoke("work_agent_answer", args).await.map(|_| ())
}

// ---------- following a turn ----------

#[derive(Default)]
struct Ctl {
    /// set by Ctrl-C handling: the turn to stop on a second press
    turn: Mutex<String>,
    armed_at: AtomicI64,
    stop_sent: AtomicBool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Ctrl-C: once = detach after 2 s, twice = stop the turn (and keep
/// following until it settles).
fn spawn_ctrlc(c: Arc<Client>, ctl: Arc<Ctl>, quiet: bool) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            let now = now_ms();
            let prev = ctl.armed_at.swap(now, Ordering::SeqCst);
            let turn = ctl.turn.lock().map(|t| t.clone()).unwrap_or_default();
            if prev != 0 && now - prev < 2000 && !turn.is_empty() {
                ctl.stop_sent.store(true, Ordering::SeqCst);
                eprintln!("\nvicuna: stopping the turn…");
                let _ = c.invoke("work_agent_stop", json!({ "turnId": turn })).await;
                continue;
            }
            if !quiet {
                eprintln!("\nvicuna: press Ctrl-C again to stop the turn; detaching in 2 s (it keeps running on the server)");
            }
            let ctl2 = ctl.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2000)).await;
                if ctl2.armed_at.load(Ordering::SeqCst) == now && !ctl2.stop_sent.load(Ordering::SeqCst) {
                    eprintln!("vicuna: detached");
                    std::process::exit(EXIT_DETACHED);
                }
            });
        }
    });
}

pub enum Outcome {
    Done,
    Stopped,
}

/// Follow a session's live turn(s) until it settles: the given channel, then
/// any follow-up turn the server starts for queued messages.
async fn follow(c: &Arc<Client>, ctl: &Arc<Ctl>, r: &mut Renderer, session: &str, mut turn: String, mut channel: String) -> Result<Outcome, String> {
    let mut strikes = 0;
    let mut gone_since: Option<std::time::Instant> = None;
    loop {
        if let Ok(mut t) = ctl.turn.lock() {
            *t = turn.clone();
        }
        let evs = match c.poll(&channel).await {
            Ok(e) => {
                strikes = 0;
                e
            }
            Err(e) => {
                strikes += 1;
                if strikes > 20 {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut finished = None;
        for ev in &evs {
            if ev.get("type").and_then(Value::as_str) == Some("askrequest") && !r.json {
                r.last = 'n';
                answer_ask(c, r, &turn, ev).await?;
                continue;
            }
            if let Some(f) = r.event(ev) {
                finished = Some(f);
            }
        }
        if evs.is_empty() {
            // The engine finishing comes before the server's final write,
            // and the last events + `done` are released only after that
            // write — so a turn reported "not live" may still have output
            // on its way. Keep draining; give up only when nothing (no
            // `done`) arrives for a while (a vanished turn: server restart).
            let live = c.invoke("work_agent_turn_live", json!({ "turnId": turn })).await.unwrap_or(json!(true));
            if live == json!(false) {
                let since = *gone_since.get_or_insert_with(std::time::Instant::now);
                let row = c.session("get", json!({ "id": session })).await?;
                if row["running"] != json!(true) && since.elapsed() > Duration::from_secs(20) {
                    if r.last != 'n' {
                        println!();
                    }
                    return Ok(Outcome::Done);
                }
            }
        } else {
            gone_since = None;
        }
        let Some(f) = finished else { continue };
        if r.last != 'n' {
            println!();
            r.last = 'n';
        }
        let stopped = match f {
            Err(e) => return Err(e),
            Ok(s) => s,
        };
        if stopped || ctl.stop_sent.load(Ordering::SeqCst) {
            return Ok(Outcome::Stopped);
        }
        // leftover queued messages start a follow-up turn right after
        tokio::time::sleep(Duration::from_millis(600)).await;
        let row = c.session("get", json!({ "id": session })).await?;
        // `channel` may be a reader queue (`<channel>~<reader>`, from attach)
        let reader = channel.contains('~');
        let base = channel.split('~').next().unwrap_or("").to_string();
        match (row["running"].as_bool(), row["channel"].as_str(), row["turnId"].as_str()) {
            (Some(true), Some(ch), Some(t)) if ch != base => {
                channel = ch.to_string();
                turn = t.to_string();
                if reader {
                    // follow the next turn through a reader too, so this
                    // client never takes events from whoever owns it
                    let at = c.invoke("work_agent_attach", json!({ "sessionId": session })).await?;
                    if at["turnId"].as_str() == Some(t) {
                        if let Some(ch2) = at["channel"].as_str() {
                            channel = ch2.to_string();
                            print_reply_so_far(r, &at);
                        }
                    }
                }
            }
            _ => return Ok(Outcome::Done),
        }
    }
}

/// The current reply as an attach snapshot holds it (what was said before
/// this client started reading the turn).
fn print_reply_so_far(r: &mut Renderer, at: &Value) {
    if r.json {
        return;
    }
    let snap = &at["snapshot"];
    if let Some(m) = snap["messages"].as_array().and_then(|m| m.get(snap["index"].as_u64().unwrap_or(0) as usize)) {
        let so_far = plain_transcript(m["content"].as_str().unwrap_or(""));
        if !so_far.is_empty() {
            print!("{so_far}");
            r.last = 't';
        }
    }
}

fn exit_for(o: Result<Outcome, String>, st: &Style) -> i32 {
    match o {
        Ok(Outcome::Done) => 0,
        Ok(Outcome::Stopped) => {
            eprintln!("{}", st.dim("(stopped)"));
            EXIT_STOPPED
        }
        Err(e) => {
            eprintln!("{}", st.red(&format!("vicuna: {e}")));
            1
        }
    }
}

// ---------- session commands ----------

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn ago(ms: i64) -> String {
    let d = (now_ms() - ms).max(0) / 1000;
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86_399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86_400),
    }
}

fn harness_label(row: &Value) -> String {
    let e = row["engine"].as_str().unwrap_or("default");
    match row["model"].as_str().filter(|m| !m.is_empty()) {
        Some(m) => format!("{e}/{m}"),
        None => e.to_string(),
    }
}

fn print_row_detail(row: &Value) {
    println!("session  {}", row["id"].as_str().unwrap_or(""));
    println!("title    {}", row["title"].as_str().unwrap_or(""));
    println!("folder   {}", row["workspace"].as_str().unwrap_or(""));
    println!("harness  {}", harness_label(row));
    if let Some(m) = row["mcp"].as_array() {
        let names: Vec<&str> = m.iter().filter_map(Value::as_str).collect();
        println!("mcp      {}", if names.is_empty() { "none".to_string() } else { names.join(", ") });
    }
    println!("state    {}", if row["running"] == json!(true) { "running" } else { "idle" });
}

async fn session_list(c: &Client, a: &Args) -> i32 {
    a.only(&["--all", "--archived", "-w", "--workspace", "-e", "--engine", "--harness"], SESSION_USAGE);
    let rows = c.session("list", json!({})).await.unwrap_or_else(|e| fail(e));
    let ws = a.get(&["-w", "--workspace"]).map(|w| std::fs::canonicalize(&w).map(|p| p.to_string_lossy().to_string()).unwrap_or(w));
    let eng = a.get(&["-e", "--engine", "--harness"]);
    let mut rows: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| a.has(&["--all"]) || r["mode"] == "work")
        .filter(|r| a.has(&["--archived"]) || r["archived"] != json!(true))
        .filter(|r| ws.as_deref().is_none_or(|w| r["workspace"].as_str() == Some(w)))
        .filter(|r| eng.as_deref().is_none_or(|e| r["engine"].as_str() == Some(e)))
        .collect();
    rows.sort_by_key(|r| -r["updatedAt"].as_i64().unwrap_or(0));
    if a.has(&["--json"]) {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
        return 0;
    }
    if rows.is_empty() {
        println!("No sessions.");
        return 0;
    }
    let st = Style { tty: std::io::stdout().is_terminal() };
    for r in &rows {
        let id = r["id"].as_str().unwrap_or("");
        let state = if r["running"] == json!(true) { st.accent("● running") } else { st.dim("○ idle   ") };
        let folder = r["workspace"].as_str().map(|w| w.rsplit(['/', '\\']).next().unwrap_or(w).to_string()).unwrap_or_default();
        let arch = if r["archived"] == json!(true) { st.dim(" [archived]") } else { String::new() };
        let mode = if r["mode"] == "work" { harness_label(r) } else { "chat".into() };
        println!(
            "{}  {state}  {:<32}  {:<26}  {:<16}  {}{arch}",
            st.bold(short(id)),
            clip(r["title"].as_str().unwrap_or(""), 32),
            clip(&mode, 26),
            clip(&folder, 16),
            st.dim(&ago(r["updatedAt"].as_i64().unwrap_or(0)))
        );
    }
    0
}

fn prompt_from(a: &Args, skip: usize) -> String {
    let mut p = a.get(&["-p", "--prompt"]).unwrap_or_default();
    let rest = a.pos.iter().skip(skip).cloned().collect::<Vec<_>>().join(" ");
    if !rest.is_empty() {
        p = if p.is_empty() { rest } else { format!("{p} {rest}") };
    }
    if p == "-" {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
        p = s;
    }
    p.trim().to_string()
}

/// Run (or queue) one prompt and follow it unless detached.
async fn run_prompt(c: &Arc<Client>, ctl: &Arc<Ctl>, a: &Args, session: &str, row: Value) -> i32 {
    let r_json = a.has(&["--json"]);
    let (Some(turn), Some(ch)) = (row["turnId"].as_str(), row["channel"].as_str()) else {
        fail("the server did not start a turn");
    };
    if a.has(&["--detach", "-d"]) {
        if r_json {
            println!("{row}");
        } else {
            println!("{} {}", if row["queued"] == json!(true) { "Queued into" } else { "Started" }, session);
        }
        return 0;
    }
    if row["queued"] == json!(true) && !r_json {
        eprintln!("{}", Style { tty: std::io::stderr().is_terminal() }.dim("(queued into the running turn)"));
    }
    let mut r = Renderer::new(r_json, !a.has(&["--no-think"]));
    r.model = row["label"].as_str().map(str::to_string);
    let o = follow(c, ctl, &mut r, session, turn.to_string(), ch.to_string()).await;
    exit_for(o, &r.st)
}

async fn interactive(c: &Arc<Client>, ctl: &Arc<Ctl>, a: &Args, mut session: Option<String>, create: Value) -> i32 {
    let st = Style { tty: true };
    eprintln!("{}", st.dim("Interactive session. Empty line or Ctrl-D to quit; Ctrl-C twice stops a running turn."));
    loop {
        let Some(text) = read_line(st.bold("you › ")).await else { break };
        let text = text.trim().to_string();
        if text.is_empty() {
            break;
        }
        let row = match &session {
            None => {
                let mut args = create.clone();
                args["prompt"] = json!(text);
                match c.session("create", args).await {
                    Ok(r) => {
                        let id = r["id"].as_str().unwrap_or("").to_string();
                        eprintln!("{}", st.dim(&format!("session {id} ({})", harness_label(&r))));
                        session = Some(id);
                        r
                    }
                    Err(e) => fail(e),
                }
            }
            Some(id) => match c.session("send", json!({ "id": id, "text": text })).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}", st.red(&format!("vicuna: {e}")));
                    continue;
                }
            },
        };
        let id = session.clone().unwrap_or_default();
        let code = run_prompt(c, ctl, a, &id, row).await;
        if let Ok(mut t) = ctl.turn.lock() {
            t.clear();
        }
        ctl.stop_sent.store(false, Ordering::SeqCst);
        ctl.armed_at.store(0, Ordering::SeqCst);
        if code == 1 {
            eprintln!();
        }
    }
    0
}

fn create_args(a: &Args) -> Value {
    let ws = a.get(&["-w", "--workspace"]).unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
    // a relative path means the caller's cwd on the local server only
    let ws = if std::path::Path::new(&ws).is_relative() && !a.has(&["--server"]) {
        std::fs::canonicalize(&ws).map(|p| p.to_string_lossy().to_string()).unwrap_or(ws)
    } else {
        ws
    };
    let mut args = json!({ "workspace": ws });
    for (k, f) in [("engine", &["-e", "--engine", "--harness"][..]), ("model", &["-m", "--model"]), ("title", &["--title"])] {
        if let Some(v) = a.get(f) {
            args[k] = json!(v);
        }
    }
    if let Some(m) = a.get(&["--mcp"]) {
        args["mcp"] = mcp_arg(&m);
    }
    args
}

async fn session_view(c: &Arc<Client>, a: &Args, id: &str) -> i32 {
    a.only(&["--tail", "--follow", "-f", "--raw", "--no-think"], SESSION_USAGE);
    let row = c.session("get", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
    if a.has(&["--json"]) && !a.has(&["--follow", "-f"]) {
        println!("{}", serde_json::to_string_pretty(&row).unwrap_or_default());
        return 0;
    }
    let st = Style { tty: std::io::stdout().is_terminal() };
    let msgs = row["messages"].as_array().cloned().unwrap_or_default();
    let tail: usize = a.get(&["--tail"]).map(|t| t.parse().unwrap_or_else(|_| fail("bad --tail"))).unwrap_or(usize::MAX);
    let start = msgs.len().saturating_sub(tail);
    if !a.has(&["--json"]) {
        print_row_detail(&row);
        if start > 0 {
            println!("{}", st.dim(&format!("… {start} earlier messages")));
        }
        let live_idx = if row["running"] == json!(true) { msgs.len().checked_sub(1) } else { None };
        for (i, m) in msgs.iter().enumerate().skip(start) {
            let content = m["content"].as_str().unwrap_or("");
            println!();
            if m["role"] == "user" {
                println!("{}", st.bold("❯ you"));
                println!("{}", content.trim());
            } else {
                let mut label = m["model"].as_str().unwrap_or("assistant").to_string();
                if let Some(ms) = m["genMs"].as_f64().map(|v| v as u64) {
                    label.push_str(&format!(" {}", fmt_duration(ms)));
                }
                if let Some(t) = m["genTps"].as_f64() {
                    label.push_str(&format!(" {t:.1} tok/s"));
                }
                if Some(i) == live_idx {
                    label.push_str(" (live)");
                }
                println!("{}", st.accent(&format!("◆ {label}")));
                println!("{}", if a.has(&["--raw"]) { content.trim().to_string() } else { plain_transcript(content) });
            }
        }
    }
    if a.has(&["--follow", "-f"]) && row["running"] == json!(true) {
        return attach(c, a, id, false).await;
    }
    0
}

/// Follow a session's live turn from now on (printing what it has said so
/// far in the current reply first).
async fn attach(c: &Arc<Client>, a: &Args, id: &str, print_head: bool) -> i32 {
    let row = c.session("get", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
    let sid = row["id"].as_str().unwrap_or(id).to_string();
    if row["running"] != json!(true) {
        println!("Session {} is idle.", short(&sid));
        return 0;
    }
    let at = c.invoke("work_agent_attach", json!({ "sessionId": sid })).await.unwrap_or_else(|e| fail(e));
    let (Some(turn), Some(ch)) = (at["turnId"].as_str(), at["channel"].as_str()) else {
        println!("Session {} is idle.", short(&sid));
        return 0;
    };
    let mut r = Renderer::new(a.has(&["--json"]), !a.has(&["--no-think"]));
    r.model = Some(harness_label(&row));
    if print_head && !r.json {
        print_row_detail(&row);
        let snap = &at["snapshot"];
        if let Some(m) = snap["messages"].as_array().and_then(|m| m.get(snap["index"].as_u64().unwrap_or(0) as usize)) {
            let so_far = plain_transcript(m["content"].as_str().unwrap_or(""));
            println!();
            if !so_far.is_empty() {
                print!("{so_far}");
                r.last = 't';
            }
        }
    }
    let ctl = Arc::new(Ctl::default());
    spawn_ctrlc(c.clone(), ctl.clone(), false);
    // an unanswered question is held in the snapshot, not the channel
    if let Some(ask) = at["snapshot"]["pendingAsk"].as_object() {
        let mut ev = Value::Object(ask.clone());
        ev["type"] = json!("askrequest");
        r.last = 'n';
        if let Err(e) = answer_ask(c, &r, turn, &ev).await {
            eprintln!("vicuna: {e}");
        }
    }
    let o = follow(c, &ctl, &mut r, &sid, turn.to_string(), ch.to_string()).await;
    exit_for(o, &r.st)
}

pub async fn session_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    let Some(sub) = a.pos.first().cloned() else {
        println!("{SESSION_USAGE}");
        return 0;
    };
    if sub == "help" || a.has(&["-h", "--help"]) {
        println!("{SESSION_USAGE}");
        return 0;
    }
    let c = Arc::new(Client::new(&a));
    let id = || a.pos.get(1).cloned().unwrap_or_else(|| usage_fail(&format!("session {sub} needs a session id"), SESSION_USAGE));
    let follow_flags = ["--detach", "-d", "--no-think", "-p", "--prompt"];
    match sub.as_str() {
        "list" | "ls" => session_list(&c, &a).await,
        "new" | "start" => {
            let mut allowed = vec!["-w", "--workspace", "-e", "--engine", "--harness", "-m", "--model", "--mcp", "--title"];
            allowed.extend(follow_flags);
            a.only(&allowed, SESSION_USAGE);
            let mut args = create_args(&a);
            let prompt = prompt_from(&a, 1);
            let ctl = Arc::new(Ctl::default());
            if prompt.is_empty() {
                if !std::io::stdin().is_terminal() {
                    fail("no prompt (pass -p PROMPT, words after the options, or `-p -` for stdin)");
                }
                spawn_ctrlc(c.clone(), ctl.clone(), false);
                return interactive(&c, &ctl, &a, None, args).await;
            }
            args["prompt"] = json!(prompt);
            let row = c.session("create", args).await.unwrap_or_else(|e| fail(e));
            let sid = row["id"].as_str().unwrap_or("").to_string();
            if !a.has(&["--json"]) && !a.has(&["--detach", "-d"]) {
                eprintln!("{}", Style { tty: std::io::stderr().is_terminal() }.dim(&format!("session {sid} · {} · {}", harness_label(&row), row["workspace"].as_str().unwrap_or(""))));
            }
            if a.has(&["--detach", "-d"]) {
                if a.has(&["--json"]) { println!("{row}") } else { println!("{sid}") }
                return 0;
            }
            spawn_ctrlc(c.clone(), ctl.clone(), false);
            let mut row = row;
            row["label"] = json!(harness_label(&row));
            run_prompt(&c, &ctl, &a, &sid, row).await
        }
        "send" | "continue" => {
            a.only(&follow_flags, SESSION_USAGE);
            let id = id();
            let prompt = prompt_from(&a, 2);
            let ctl = Arc::new(Ctl::default());
            spawn_ctrlc(c.clone(), ctl.clone(), false);
            if prompt.is_empty() {
                if !std::io::stdin().is_terminal() {
                    fail("no prompt");
                }
                let row = c.session("get", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
                return interactive(&c, &ctl, &a, row["id"].as_str().map(str::to_string), json!({})).await;
            }
            let row = c.session("send", json!({ "id": id, "text": prompt })).await.unwrap_or_else(|e| fail(e));
            let sid = row["id"].as_str().unwrap_or(&id).to_string();
            run_prompt(&c, &ctl, &a, &sid, row).await
        }
        "chat" => {
            a.only(&["--no-think"], SESSION_USAGE);
            let row = c.session("get", json!({ "id": id() })).await.unwrap_or_else(|e| fail(e));
            let sid = row["id"].as_str().unwrap_or("").to_string();
            if row["running"] == json!(true) {
                let code = attach(&c, &a, &sid, true).await;
                if code != 0 {
                    return code;
                }
            } else {
                print_row_detail(&row);
            }
            let ctl = Arc::new(Ctl::default());
            spawn_ctrlc(c.clone(), ctl.clone(), true);
            interactive(&c, &ctl, &a, Some(sid), json!({})).await
        }
        "view" | "show" | "log" => session_view(&c, &a, &id()).await,
        "attach" | "follow" => {
            a.only(&["--no-think"], SESSION_USAGE);
            attach(&c, &a, &id(), true).await
        }
        "stop" => {
            a.only(&[], SESSION_USAGE);
            let r = c.session("stop", json!({ "id": id() })).await.unwrap_or_else(|e| fail(e));
            let sid = r["id"].as_str().unwrap_or("");
            if r["stopped"] == json!(true) {
                // wait for it to settle so `stop && send` works
                for _ in 0..120 {
                    let row = c.session("get", json!({ "id": sid })).await.unwrap_or_default();
                    if row["running"] != json!(true) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                println!("Stopped {sid}.");
            } else {
                println!("Session {sid} was not running.");
            }
            0
        }
        "set" | "update" => {
            a.only(&["-e", "--engine", "--harness", "-m", "--model", "-w", "--workspace", "--title", "--mcp"], SESSION_USAGE);
            let mut args = json!({ "id": id() });
            for (k, f) in [("engine", &["-e", "--engine", "--harness"][..]), ("model", &["-m", "--model"]), ("title", &["--title"]), ("workspace", &["-w", "--workspace"])] {
                if let Some(v) = a.get(f) {
                    args[k] = json!(v);
                }
            }
            if let Some(m) = a.get(&["--mcp"]) {
                args["mcp"] = mcp_arg(&m);
            }
            if args.as_object().is_some_and(|o| o.len() == 1) {
                usage_fail("session set needs at least one of -e, -m, -w, --title, --mcp", SESSION_USAGE);
            }
            let row = c.session("update", args).await.unwrap_or_else(|e| fail(e));
            if a.has(&["--json"]) { println!("{row}") } else { print_row_detail(&row) }
            0
        }
        "rename" | "pin" | "unpin" | "lead" | "unlead" | "fork" | "edit" | "rm-msg" => {
            a.only(&["--truncate"], SESSION_USAGE);
            let row = c.session("get", json!({ "id": id() })).await.unwrap_or_else(|e| fail(e));
            let sid = row["id"].as_str().unwrap_or("").to_string();
            let num = |i: usize, what: &str| -> u64 {
                a.pos.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage_fail(&format!("session {sub} needs {what}"), SESSION_USAGE))
            };
            let rest = |i: usize| a.pos.iter().skip(i).cloned().collect::<Vec<_>>().join(" ");
            let (action, args) = match sub.as_str() {
                "rename" => ("session_update", json!({ "id": sid, "title": rest(2) })),
                "pin" | "unpin" => ("session_update", json!({ "id": sid, "pinned": sub == "pin" })),
                "lead" | "unlead" => ("session_update", json!({ "id": sid, "role": if sub == "lead" { json!("lead") } else { Value::Null } })),
                "fork" => ("session_fork", json!({ "id": sid, "at": num(2, "a message index") })),
                "edit" => ("message_edit", json!({ "id": sid, "index": num(2, "a message index"), "content": rest(3), "truncate": a.has(&["--truncate"]) })),
                _ => ("message_delete", json!({ "id": sid, "index": num(2, "a message index") })),
            };
            let out = c.invoke(action, args).await.unwrap_or_else(|e| fail(e));
            if a.has(&["--json"]) {
                println!("{out}");
            } else if sub == "fork" {
                println!("Forked into {}.", out["id"].as_str().unwrap_or(""));
            } else {
                let n = out["messages"].as_array().map(Vec::len).unwrap_or(0);
                println!("{} · \"{}\" · {n} messages", sid, out["title"].as_str().unwrap_or(""));
            }
            0
        }
        "archive" | "unarchive" => {
            a.only(&[], SESSION_USAGE);
            let row = c.session(&sub, json!({ "id": id() })).await.unwrap_or_else(|e| fail(e));
            println!("{} {}.", if sub == "archive" { "Archived" } else { "Unarchived" }, row["id"].as_str().unwrap_or(""));
            0
        }
        "delete" | "rm" => {
            a.only(&["-y", "--yes"], SESSION_USAGE);
            let row = c.session("get", json!({ "id": id() })).await.unwrap_or_else(|e| fail(e));
            let sid = row["id"].as_str().unwrap_or("").to_string();
            if !a.has(&["-y", "--yes"]) {
                if !std::io::stdin().is_terminal() {
                    fail("refusing to delete without -y (no terminal to confirm on)");
                }
                let n = row["messages"].as_array().map(Vec::len).unwrap_or(0);
                let q = format!("Delete \"{}\" ({n} messages{})? [y/N] ", row["title"].as_str().unwrap_or(""), if row["running"] == json!(true) { ", running — it will be stopped" } else { "" });
                if !read_line(q).await.is_some_and(|s| matches!(s.trim(), "y" | "Y" | "yes")) {
                    println!("Kept.");
                    return 0;
                }
            }
            c.session("delete", json!({ "id": sid })).await.unwrap_or_else(|e| fail(e));
            println!("Deleted {sid}.");
            0
        }
        other => usage_fail(&format!("unknown session command {other}"), SESSION_USAGE),
    }
}

// ---------- schedule ----------

/// The local zone's offset from UTC in minutes (`date +%z`; 0 if unknown).
fn local_offset_min() -> i64 {
    if let Ok(v) = std::env::var("VICUNA_TZ_OFFSET_MIN") {
        if let Ok(n) = v.trim().parse() {
            return n;
        }
    }
    let out = std::process::Command::new("date").arg("+%z").output().ok();
    let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    parse_z_offset(&s).unwrap_or(0)
}

pub fn parse_z_offset(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 5 || !(b[0] == b'+' || b[0] == b'-') {
        return None;
    }
    let h: i64 = s[1..3].parse().ok()?;
    let m: i64 = s[3..5].parse().ok()?;
    Some(if b[0] == b'-' { -(h * 60 + m) } else { h * 60 + m })
}

/// `30m`, `2h`, `1d`, `90` (minutes), `1h30m` → minutes.
pub fn parse_in(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    if let Ok(n) = s.parse::<f64>() {
        return Some(n);
    }
    let mut total = 0.0;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
            continue;
        }
        let n: f64 = num.parse().ok()?;
        num.clear();
        total += n * match ch {
            's' => 1.0 / 60.0,
            'm' => 1.0,
            'h' => 60.0,
            'd' => 1440.0,
            _ => return None,
        };
    }
    if !num.is_empty() {
        return None;
    }
    (total > 0.0).then_some(total)
}

fn fmt_local(ms: i64, offset: i64) -> String {
    // civil-from-days (Howard Hinnant), in the local offset
    let secs = ms / 1000 + offset * 60;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", tod / 3600, (tod % 3600) / 60)
}

pub async fn schedule_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    let Some(sub) = a.pos.first().cloned() else {
        println!("{SCHEDULE_USAGE}");
        return 0;
    };
    if sub == "help" || a.has(&["-h", "--help"]) {
        println!("{SCHEDULE_USAGE}");
        return 0;
    }
    let c = Client::new(&a);
    let offset = local_offset_min();
    match sub.as_str() {
        "list" | "ls" => {
            a.only(&[], SCHEDULE_USAGE);
            let list = c.invoke("schedule_list", json!({})).await.unwrap_or_else(|e| fail(e));
            if a.has(&["--json"]) {
                println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
                return 0;
            }
            let mut items = list.as_array().cloned().unwrap_or_default();
            items.sort_by_key(|i| i["at"].as_i64().unwrap_or(0));
            if items.is_empty() {
                println!("Nothing scheduled.");
            }
            for it in items {
                let target = match it["chatId"].as_str().filter(|s| !s.is_empty()) {
                    Some(cid) => short(cid).to_string(),
                    None => format!("new in {}", it["workspace"].as_str().unwrap_or("?")),
                };
                let flag = if it["missed"] == json!(true) {
                    format!("  NOT SENT{}", it["error"].as_str().map(|e| format!(": {e}")).unwrap_or_default())
                } else {
                    String::new()
                };
                println!("{}  {}  → {:<12}  {}{flag}", short(it["id"].as_str().unwrap_or("")), fmt_local(it["at"].as_i64().unwrap_or(0), offset), target, clip(it["text"].as_str().unwrap_or(""), 60));
            }
            0
        }
        "add" => {
            a.only(&["--in", "--at", "--new", "-w", "--workspace", "-e", "--engine", "--harness", "-m", "--model", "--mcp"], SCHEDULE_USAGE);
            let new = a.has(&["--new"]);
            let skip = if new { 1 } else { 2 };
            let text = a.pos.iter().skip(skip).cloned().collect::<Vec<_>>().join(" ");
            if text.trim().is_empty() {
                usage_fail("schedule add needs a message", SCHEDULE_USAGE);
            }
            let now = now_ms();
            let in_min = a.get(&["--in"]).map(|s| parse_in(&s).unwrap_or_else(|| fail(format!("can't read --in {s} (use 30m, 2h, 1d)"))));
            let at = a.get(&["--at"]);
            if in_min.is_none() && at.is_none() {
                usage_fail("schedule add needs --in or --at", SCHEDULE_USAGE);
            }
            let when = vicuna_lib::server::persist::resolve_wake_time(at.as_deref(), in_min, now, offset).unwrap_or_else(|e| fail(e));
            let mut item = json!({ "id": vicuna_lib::server::turns::uuid(), "text": text.trim(), "at": when, "createdAt": now });
            if new {
                let args = create_args(&a);
                let ws = c.invoke("work_validate_root", json!({ "path": args["workspace"] })).await.unwrap_or_else(|e| fail(e));
                item["chatId"] = Value::Null;
                item["workspace"] = ws;
                item["title"] = json!(clip(text.trim(), 32));
                for k in ["engine", "model", "mcp"] {
                    if !args[k].is_null() {
                        item[k] = args[k].clone();
                    }
                }
            } else {
                let id = a.pos.get(1).cloned().unwrap_or_else(|| usage_fail("schedule add needs a session id (or --new)", SCHEDULE_USAGE));
                let row = c.session("get", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
                item["chatId"] = row["id"].clone();
                item["title"] = row["title"].clone();
                item["workspace"] = row["workspace"].clone();
            }
            c.invoke("schedule_put", json!({ "items": [item.clone()] })).await.unwrap_or_else(|e| fail(e));
            let mins = ((when - now) as f64 / 60_000.0).round() as i64;
            println!("Scheduled {} for {} (in {}).", short(item["id"].as_str().unwrap_or("")), fmt_local(when, offset), fmt_duration((mins.max(0) * 60_000) as u64));
            println!("The server sends it then, with or without the app open.");
            0
        }
        "rm" | "remove" | "delete" => {
            a.only(&[], SCHEDULE_USAGE);
            if a.pos.len() < 2 {
                usage_fail("schedule rm needs an id", SCHEDULE_USAGE);
            }
            let list = c.invoke("schedule_list", json!({})).await.unwrap_or_else(|e| fail(e));
            let all: Vec<String> = list.as_array().into_iter().flatten().filter_map(|i| i["id"].as_str().map(str::to_string)).collect();
            let mut ids = Vec::new();
            for p in &a.pos[1..] {
                let hits: Vec<&String> = all.iter().filter(|i| i.starts_with(p.as_str())).collect();
                match hits.as_slice() {
                    [one] => ids.push((*one).clone()),
                    [] => fail(format!("no scheduled send matches {p}")),
                    _ => fail(format!("{p} is ambiguous")),
                }
            }
            c.invoke("schedule_remove", json!({ "ids": ids })).await.unwrap_or_else(|e| fail(e));
            println!("Removed {}.", ids.iter().map(|i| short(i)).collect::<Vec<_>>().join(", "));
            0
        }
        other => usage_fail(&format!("unknown schedule command {other}"), SCHEDULE_USAGE),
    }
}

// ---------- engines / models ----------

pub async fn engines_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    a.only(&[], "usage: vicuna engines [--json]");
    let c = Client::new(&a);
    let info = c.invoke("work_engine_info", json!({})).await.unwrap_or_else(|e| fail(e));
    if a.has(&["--json"]) {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
        return 0;
    }
    let current = info["id"].as_str().unwrap_or("");
    for e in info["engines"].as_array().into_iter().flatten() {
        let id = e["id"].as_str().unwrap_or("");
        let mark = if id == current { "*" } else { " " };
        let ver = e["version"].as_str().unwrap_or("");
        if e["installed"] == json!(true) {
            println!("{mark} {id:<9} {:<14} {ver}", e["label"].as_str().unwrap_or(""));
        } else {
            println!("{mark} {id:<9} {:<14} not installed", e["label"].as_str().unwrap_or(""));
        }
    }
    println!(
        "\n* default harness: {current} ({}), model {}",
        info["path"].as_str().unwrap_or("?"),
        info["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("default")
    );
    if let Some(w) = info["warning"].as_str().filter(|w| !w.is_empty()) {
        println!("  ! {w}");
    }
    0
}

pub async fn models_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    a.only(&["-e", "--engine", "--harness"], "usage: vicuna models [-e HARNESS] [--json]");
    let c = Client::new(&a);
    let engine = a.get(&["-e", "--engine", "--harness"]).or_else(|| a.pos.first().cloned());
    let mut args = json!({});
    if let Some(e) = &engine {
        args["engine"] = json!(e);
    }
    let models = c.invoke("work_models", args).await.unwrap_or_else(|e| fail(e));
    if a.has(&["--json"]) {
        println!("{}", serde_json::to_string_pretty(&models).unwrap_or_default());
        return 0;
    }
    let list = models.get("models").and_then(Value::as_array).or_else(|| models.as_array()).cloned().unwrap_or_default();
    let saved = models.get("current").and_then(Value::as_str).unwrap_or("");
    for m in list {
        let (id, name) = match &m {
            Value::String(s) => (s.clone(), String::new()),
            o => (
                o.get("id").or_else(|| o.get("value")).and_then(Value::as_str).unwrap_or("").to_string(),
                o.get("name").or_else(|| o.get("displayName")).and_then(Value::as_str).unwrap_or("").to_string(),
            ),
        };
        let mark = if !saved.is_empty() && id == saved { "*" } else { " " };
        if name.is_empty() || name == id { println!("{mark} {id}") } else { println!("{mark} {id:<40} {name}") }
    }
    0
}

// ---------- mcp ----------

pub async fn mcp_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    let Some(sub) = a.pos.first().cloned() else {
        println!("{MCP_USAGE}");
        return 0;
    };
    if sub == "help" || a.has(&["-h", "--help"]) {
        println!("{MCP_USAGE}");
        return 0;
    }
    let c = Client::new(&a);
    let list = || async { c.invoke("mcp_list_servers", json!({})).await.unwrap_or_else(|e| fail(e)) };
    let find = |l: &Value, name: &str| -> Value {
        l.as_array().into_iter().flatten().find(|s| s["name"].as_str() == Some(name) || s["id"].as_str() == Some(name)).cloned().unwrap_or_else(|| fail(format!("no MCP server named {name} (see `vicuna mcp list`)")))
    };
    let name = || a.pos.get(1).cloned().unwrap_or_else(|| usage_fail(&format!("mcp {sub} needs a server name"), MCP_USAGE));
    match sub.as_str() {
        "list" | "ls" => {
            a.only(&[], MCP_USAGE);
            let l = list().await;
            if a.has(&["--json"]) {
                println!("{}", serde_json::to_string_pretty(&l).unwrap_or_default());
                return 0;
            }
            let arr = l.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                println!("No MCP servers configured on the server.");
            }
            for s in arr {
                let endpoint = s["url"].as_str().map(str::to_string).unwrap_or_else(|| s["command"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default());
                println!("{} {:<20} {}", if s["enabled"] == json!(false) { "○" } else { "●" }, s["name"].as_str().unwrap_or(""), clip(&endpoint, 70));
            }
            println!("(● enabled = handed to every session without --mcp; vicuna-mcp is always added)");
            0
        }
        "add" => {
            a.only(&["--url", "--command", "--header", "--env", "--disabled"], MCP_USAGE);
            let n = name();
            let mut entry = json!({ "id": n, "name": n, "transport": "stdio", "enabled": !a.has(&["--disabled"]) });
            let kv = |list: Vec<String>, sep: char| -> Value {
                let mut m = serde_json::Map::new();
                for x in list {
                    let (k, v) = x.split_once(sep).or_else(|| x.split_once('=')).unwrap_or_else(|| fail(format!("expected K=V, got {x}")));
                    m.insert(k.trim().to_string(), json!(v.trim()));
                }
                Value::Object(m)
            };
            match (a.get(&["--url"]), a.get(&["--command"])) {
                (Some(u), None) => {
                    entry["transport"] = json!("http");
                    entry["url"] = json!(u);
                    let h = a.all(&["--header"]);
                    if !h.is_empty() {
                        entry["headers"] = kv(h, ':');
                    }
                }
                (None, Some(cmd)) => {
                    entry["transport"] = json!("stdio");
                    entry["command"] = json!(cmd.split_whitespace().collect::<Vec<_>>());
                    let e = a.all(&["--env"]);
                    if !e.is_empty() {
                        entry["env"] = kv(e, '=');
                    }
                }
                _ => usage_fail("mcp add needs exactly one of --url or --command", MCP_USAGE),
            }
            if entry["enabled"] == json!(true) {
                // verify before saving: a mistyped endpoint persists nothing
                match c.invoke("mcp_connect", json!({ "config": entry })).await {
                    Ok(v) => {
                        c.invoke("mcp_save_server", json!({ "config": entry })).await.unwrap_or_else(|e| fail(e));
                        println!("Added {n} ({} tools).", v["tools"].as_array().map(Vec::len).unwrap_or(0))
                    }
                    Err(e) => fail(format!("{n} did not connect, nothing saved: {e}")),
                }
            } else {
                c.invoke("mcp_save_server", json!({ "config": entry })).await.unwrap_or_else(|e| fail(e));
                println!("Added {n} (disabled).");
            }
            0
        }
        "enable" | "disable" => {
            a.only(&[], MCP_USAGE);
            let n = name();
            let mut s = find(&list().await, &n);
            s["enabled"] = json!(sub == "enable");
            let id = s["id"].as_str().unwrap_or(&n).to_string();
            c.invoke("mcp_save_server", json!({ "config": s })).await.unwrap_or_else(|e| fail(e));
            if sub == "enable" {
                if let Err(e) = c.invoke("mcp_connect", json!({ "config": s })).await {
                    println!("Enabled {n}, but connecting failed: {e}");
                    return 0;
                }
            } else {
                let _ = c.invoke("mcp_disconnect", json!({ "id": id })).await;
            }
            println!("{} {n}.", if sub == "enable" { "Enabled" } else { "Disabled" });
            0
        }
        "rm" | "remove" | "delete" => {
            a.only(&[], MCP_USAGE);
            let n = name();
            let s = find(&list().await, &n);
            let id = s["id"].as_str().unwrap_or(&n).to_string();
            let _ = c.invoke("mcp_disconnect", json!({ "id": id })).await;
            c.invoke("mcp_delete_server", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
            println!("Removed {n}.");
            0
        }
        "tools" => {
            a.only(&[], MCP_USAGE);
            let n = name();
            let s = find(&list().await, &n);
            let id = s["id"].as_str().unwrap_or(&n).to_string();
            if let Err(e) = c.invoke("mcp_connect", json!({ "config": s })).await {
                fail(format!("{n} did not connect: {e}"));
            }
            let tools = c.invoke("mcp_list_tools", json!({ "id": id })).await.unwrap_or_else(|e| fail(e));
            for t in tools.as_array().into_iter().flatten() {
                println!("{:<32} {}", t["name"].as_str().unwrap_or(""), clip(t["description"].as_str().unwrap_or(""), 80));
            }
            0
        }
        other => usage_fail(&format!("unknown mcp command {other}"), MCP_USAGE),
    }
}

// ---------- saved servers ----------

pub async fn server_main(raw: &[String]) -> i32 {
    let a = Args::parse(raw);
    match a.pos.first().map(String::as_str) {
        None | Some("list") | Some("ls") => {
            let l = servers::list();
            if l.is_empty() {
                println!("No saved servers (the local `vicuna serve` is the default).");
            }
            for s in l {
                println!("{:<24} {}", s.name, s.url);
            }
            0
        }
        Some("add") => {
            let url = a.pos.get(1).cloned().unwrap_or_else(|| usage_fail("server add needs a URL", SERVER_USAGE));
            match servers::connect(&url, a.get(&["--token"]), a.get(&["--name"])).await {
                Ok(s) => {
                    println!("Saved {} ({}). Use it with --server {}.", s.name, s.url, s.name);
                    0
                }
                Err(e) => fail(e),
            }
        }
        Some("rm") | Some("remove") => {
            let sel = a.pos.get(1).cloned().unwrap_or_else(|| usage_fail("server rm needs a URL or name", SERVER_USAGE));
            let s = servers::load().into_iter().find(|s| s.url == sel || s.name == sel || s.id == sel).unwrap_or_else(|| fail(format!("no saved server {sel}")));
            servers::remove(&s.id).unwrap_or_else(|e| fail(e));
            println!("Removed {}.", s.url);
            0
        }
        Some("help") => {
            println!("{SERVER_USAGE}");
            0
        }
        Some(o) => usage_fail(&format!("unknown server command {o}"), SERVER_USAGE),
    }
}

// ---------- raw API ----------

pub const API_USAGE: &str = "vicuna api — call any server action directly (headless testing)

usage:
  vicuna api list                          every action the server answers
  vicuna api ACTION ['{json args}']        print the result as JSON
  vicuna api ACTION key=value …            same; values parse as JSON when they can
  Wherever an argument is the string \"@channel\" (e.g. onEvent=@channel), a
  channel is opened and its events stream to stdout as NDJSON until a
  done/error/exit event (or --wait SECONDS of silence, default 600).
  Server selection: --server / VICUNA_SERVER, as for every client command.

Chat example:
  vicuna api chat_turn_start id=my-chat operation=send model=qwen3 content=Hello onEvent=@channel
  vicuna api chat_turn_attach id=my-chat onEvent=@channel
  vicuna api chat_turn_stop id=my-chat";

/// `k=v` pairs → JSON object (values parsed as JSON when valid, else strings).
pub fn kv_args(pairs: &[String]) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for p in pairs {
        let (k, v) = p.split_once('=').ok_or_else(|| format!("expected key=value, got {p}"))?;
        m.insert(k.to_string(), serde_json::from_str(v).unwrap_or_else(|_| json!(v)));
    }
    Ok(Value::Object(m))
}

/// Replace every "@channel" string with a fresh channel marker; returns ids.
pub fn wire_channels(v: &mut Value, ids: &mut Vec<String>) {
    match v {
        Value::String(s) if s == "@channel" => {
            let id = vicuna_lib::server::turns::uuid();
            *v = json!({ "__bridge_channel": id });
            ids.push(id);
        }
        Value::Array(a) => a.iter_mut().for_each(|x| wire_channels(x, ids)),
        Value::Object(o) => o.values_mut().for_each(|x| wire_channels(x, ids)),
        _ => {}
    }
}

pub async fn api_main(raw: &[String]) -> i32 {
    let mut a = Args::parse(raw);
    let Some(cmd) = a.pos.first().cloned() else {
        println!("{API_USAGE}");
        return 0;
    };
    if cmd == "help" || a.has(&["-h", "--help"]) {
        println!("{API_USAGE}");
        return 0;
    }
    let wait = a.get(&["--wait"]).map(|w| w.parse::<u64>().unwrap_or_else(|_| fail("bad --wait"))).unwrap_or(600);
    let c = Client::new(&a);
    if cmd == "list" {
        let v = c.invoke("api_actions", json!({})).await.unwrap_or_else(|e| fail(e));
        for n in v.as_array().into_iter().flatten() {
            println!("{}", n.as_str().unwrap_or(""));
        }
        return 0;
    }
    let rest: Vec<String> = a.pos.drain(1..).collect();
    let mut args = match rest.as_slice() {
        [] => json!({}),
        [one] if one.trim_start().starts_with('{') => serde_json::from_str(one).unwrap_or_else(|e| fail(format!("bad JSON args: {e}"))),
        many => kv_args(many).unwrap_or_else(|e| fail(e)),
    };
    let mut channels = Vec::new();
    wire_channels(&mut args, &mut channels);
    let out = c.invoke(&cmd, args).await;
    match &out {
        Ok(v) => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
        Err(e) => {
            eprintln!("vicuna api {cmd}: {e}");
            return 1;
        }
    }
    let Some(ch) = channels.first() else { return 0 };
    let mut idle = std::time::Instant::now();
    loop {
        let evs = c.poll(ch).await.unwrap_or_default();
        for ev in &evs {
            println!("{ev}");
            if matches!(ev.get("type").and_then(Value::as_str), Some("done" | "error" | "exit")) {
                return if ev["type"] == "done" { 0 } else { 1 };
            }
        }
        if !evs.is_empty() {
            idle = std::time::Instant::now();
        } else if idle.elapsed() > Duration::from_secs(wait) {
            eprintln!("vicuna api: no events for {wait}s");
            return 1;
        }
    }
}

pub fn run_async(f: impl std::future::Future<Output = i32>) -> ! {
    // `vicuna session list | head`: a closed stdout ends the command quietly
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.payload().downcast_ref::<String>().map(String::as_str).or_else(|| info.payload().downcast_ref::<&str>().copied()).unwrap_or("");
        if msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        default_hook(info);
    }));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap_or_else(|e| fail(e));
    let code = rt.block_on(f);
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args_parse_flags_values_and_positionals() {
        let a = Args::parse(&s(&["new", "-w", "/tmp", "--mcp=a,b", "-e", "claude", "fix", "it", "--detach"]));
        assert_eq!(a.pos, s(&["new", "fix", "it"]));
        assert_eq!(a.get(&["-w"]).as_deref(), Some("/tmp"));
        assert_eq!(a.get(&["--mcp"]).as_deref(), Some("a,b"));
        assert!(a.has(&["--detach"]));
        let a = Args::parse(&s(&["send", "abc", "--", "-not-a-flag"]));
        assert_eq!(a.pos, s(&["send", "abc", "-not-a-flag"]));
    }

    #[test]
    fn api_args_and_channels() {
        let v = kv_args(&s(&["id=abc", "n=3", "flag=true", "list=[1,2]", "text=hello world"])).unwrap();
        assert_eq!(v, json!({"id":"abc","n":3,"flag":true,"list":[1,2],"text":"hello world"}));
        assert!(kv_args(&s(&["nokey"])).is_err());
        let mut a = json!({"onEvent": "@channel", "x": ["@channel", "keep"]});
        let mut ids = Vec::new();
        wire_channels(&mut a, &mut ids);
        assert_eq!(ids.len(), 2);
        assert_eq!(a["onEvent"]["__bridge_channel"], json!(ids[0]));
        assert_eq!(a["x"][1], json!("keep"));
    }

    #[test]
    fn mcp_arg_forms() {
        assert_eq!(mcp_arg("none"), json!([]));
        assert_eq!(mcp_arg("default"), Value::Null);
        assert_eq!(mcp_arg(" a , b ,"), json!(["a", "b"]));
    }

    #[test]
    fn ask_answers() {
        let o = s(&["Mango", "Kiwi", "Papaya"]);
        assert_eq!(ask_answer("2", &o, false), "Kiwi");
        assert_eq!(ask_answer("lime please", &o, false), "lime please");
        assert_eq!(ask_answer("9", &o, false), "9");
        assert_eq!(ask_answer("1,3", &o, true), r#"["Mango","Papaya"]"#);
        assert_eq!(ask_answer("other", &o, true), r#"["other"]"#);
    }

    #[test]
    fn durations_offsets_and_in() {
        assert_eq!(parse_in("30m"), Some(30.0));
        assert_eq!(parse_in("1h30m"), Some(90.0));
        assert_eq!(parse_in("2"), Some(2.0));
        assert_eq!(parse_in("1d"), Some(1440.0));
        assert_eq!(parse_in("soon"), None);
        assert_eq!(parse_z_offset("+0800"), Some(480));
        assert_eq!(parse_z_offset("-0330"), Some(-210));
        assert_eq!(parse_z_offset("UTC"), None);
        assert_eq!(fmt_duration(1500), "1.5s");
        assert_eq!(fmt_duration(125_000), "2m 5s");
        // 2026-11-16 00:00 UTC, shown at UTC+8
        assert_eq!(fmt_local(1_794_787_200_000, 480), "2026-11-16 08:00");
    }

    #[test]
    fn tool_summaries() {
        assert_eq!(tool_summary(&json!({"command": "ls -la", "description": "x"})), "ls -la");
        assert_eq!(tool_summary(&json!({"file_path": "/a/b.rs"})), "/a/b.rs");
        assert_eq!(tool_summary(&json!({})), "");
        assert_eq!(tool_summary(&json!({"n": 1})), r#"{"n":1}"#);
    }

    #[test]
    fn transcript_strips_harness_markup() {
        let c = "Listing.\n\n<details class=\"tool-result\"><summary>bash — ls &amp; more</summary><pre><code>a\nb</code></pre></details>\n\n\n\nDone <b>ok</b>\n\n<div class=\"ask-log\"><div class=\"ask-log-q\">Which?</div><div class=\"ask-log-a\"><strong>Answer:</strong> Kiwi</div></div>";
        let p = plain_transcript(c);
        assert!(p.contains("▸ bash — ls & more"), "{p}");
        assert!(!p.contains("<"), "{p}");
        assert!(p.contains("Done ok"), "{p}");
        assert!(p.contains("? Which? → Answer: Kiwi"), "{p}");
        assert!(!p.contains("\n\n\n"), "{p}");
    }
}
