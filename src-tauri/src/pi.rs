// pi (https://pi.dev, `@earendil-works/pi-coding-agent`) as a Work engine.
//
// No adapter: each Work turn runs the user's own `pi` in RPC mode
//   pi --mode rpc --no-approve [--model provider/id] [--session ID]
//      -e ~/vicuna/config/pi/vicuna-bridge.ts [-e <plugin>]...
// in the workspace, with the user's own ~/.pi login, models and settings.
// The stdin/stdout records (JSONL) map to the Work UI:
//   message_update text_delta / thinking_delta     -> Token / Think
//   tool_execution_start (complete args)           -> ToolStart
//   tool_execution_end (result, isError)           -> ToolEnd
//   assistant message_end stopReason "error"       -> readable Error
//   agent_settled                                  -> RoundEnd; queued messages
//                                                     run as the next prompt
//   extension_ui_request select "vicuna-approve:"  -> ask card (Allow /
//                                                     Always allow / Deny)
// Stop sends `abort`, then closes stdin; pi runs in its own process group,
// which is ended when the turn finishes so no pi or tool process outlives it.
//
// pi has no permission prompts or MCP flags of its own; both come from
// Vicuna's bridge extension (pi_bridge.ts): MCP servers are registered with
// `pi.registerMcpServer` for that session only, and approvals gate the
// extension `tool_call` event. Plugins: every `.ts`/`.js` file (or folder)
// in ~/vicuna/config/pi/extensions is loaded with `-e` on each turn.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::acp::{self, AcpState, McpSnapshot};
use crate::opencode::{
    common_dirs, find_exe_in, home_dir, parse_version, read_model_from, write_model_to, OpencodeModel, WorkEngineInfo,
};
use crate::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use crate::work_engine::Engine;
use crate::ChatMessage;

pub const CLI_INSTALL_URL: &str = "https://pi.dev";
/// Exclusive override (absolute path), like VICUNA_OPENCODE_BIN.
pub const BIN_ENV: &str = "VICUNA_PI_BIN";
const MODELS_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_GRACE: Duration = Duration::from_secs(15);
const OUTPUT_CAP: usize = 32 * 1024;
const APPROVE_PREFIX: &str = "vicuna-approve:";
const BRIDGE_SOURCE: &str = include_str!("pi_bridge.ts");

#[cfg(windows)]
const CLI_NAMES: &[&str] = &["pi.exe", "pi.cmd", "pi"];
#[cfg(not(windows))]
const CLI_NAMES: &[&str] = &["pi"];

// ---------- detection ----------

fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".pi").join("agent").join("bin"));
    }
    dirs.extend(common_dirs(home));
    dirs
}

pub fn find_pi_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(CLI_NAMES, override_bin, path_var, &extra_dirs(home))
}

pub fn resolve_pi() -> Option<crate::exe_resolve::Resolved> {
    crate::exe_resolve::resolve(CLI_NAMES, std::env::var(BIN_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
}

pub fn find_pi() -> Option<PathBuf> {
    resolve_pi().map(|r| r.path)
}

pub fn missing_message() -> String {
    format!("pi CLI not found, install it: {CLI_INSTALL_URL}")
}

fn describe(p: &Path) -> (Option<String>, String) {
    // `pi --version` prints "1.5.0"
    let v = crate::exe_resolve::version_of(p).and_then(|o| parse_version(&o));
    let label = match &v {
        Some(v) => format!("{} ({v})", p.display()),
        None => p.display().to_string(),
    };
    (v, label)
}

pub async fn engine_info() -> WorkEngineInfo {
    let (cli, version, other_labels) = tokio::task::spawn_blocking(|| {
        let found = resolve_pi();
        let version = found.as_ref().and_then(|f| describe(&f.path).0);
        let others: Vec<String> = found.as_ref().map(|f| f.others.iter().map(|o| describe(o).1).collect()).unwrap_or_default();
        (found.map(|f| f.path), version, others)
    })
    .await
    .unwrap_or((None, None, Vec::new()));
    let installed = cli.is_some();
    WorkEngineInfo {
        id: "pi".to_string(),
        label: "pi".to_string(),
        installed,
        missing: (!installed).then(missing_message),
        engines: Vec::new(),
        path: cli.map(|c| c.display().to_string()),
        version,
        install_url: CLI_INSTALL_URL.to_string(),
        model: saved_model(),
        config_path: prefs_path().map(|p| p.display().to_string()),
        others: other_labels,
        warning: None,
        ask_permission: false,
    }
}

// ---------- models (`pi --list-models`) ----------

static MODELS: Mutex<Option<(PathBuf, Vec<OpencodeModel>)>> = Mutex::new(None);

/// `pi --list-models` prints a table: `provider  model  context ...` with a
/// header row. Ids become `provider/model` (what `--model` accepts).
pub fn parse_models_output(out: &str) -> Vec<OpencodeModel> {
    let mut models: Vec<OpencodeModel> = Vec::new();
    for line in out.lines() {
        let mut cols = line.split_whitespace();
        let (Some(provider), Some(model)) = (cols.next(), cols.next()) else { continue };
        if provider == "provider" && model == "model" {
            continue;
        }
        let id = format!("{provider}/{model}");
        if models.iter().any(|m| m.id == id) {
            continue;
        }
        models.push(OpencodeModel { id, provider: provider.to_string(), name: None });
    }
    models
}

pub async fn list_models() -> Result<Vec<OpencodeModel>, String> {
    let cli = find_pi().ok_or_else(missing_message)?;
    if let Some((p, cached)) = MODELS.lock().ok().and_then(|g| g.clone()) {
        if p == cli {
            return Ok(cached);
        }
    }
    let mut cmd = base_command(&cli);
    cmd.args(["--offline", "--list-models"]).stdin(std::process::Stdio::null());
    if let Some(h) = home_dir() {
        cmd.current_dir(h);
    }
    let out = tokio::time::timeout(MODELS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "pi --list-models timed out".to_string())?
        .map_err(|e| format!("cannot run pi ({}): {e}", cli.display()))?;
    let models = parse_models_output(&String::from_utf8_lossy(&out.stdout));
    if models.is_empty() {
        let err = crate::exe_resolve::strip_ansi(String::from_utf8_lossy(&out.stderr).trim());
        return Err(format!(
            "pi lists no models: add a provider (run `pi` and /login, or set an API key){}",
            if err.is_empty() { String::new() } else { format!(": {err}") }
        ));
    }
    if let Ok(mut g) = MODELS.lock() {
        *g = Some((cli, models.clone()));
    }
    Ok(models)
}

// ---------- persisted model choice + Vicuna-owned pi folder ----------

pub fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("pi-work.json"))
}

pub fn saved_model() -> Option<String> {
    read_model_from(&prefs_path()?)
}

pub fn save_model(model: Option<&str>) -> Result<(), String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_model_to(&path, model)
}

/// ~/vicuna/config/pi: the bridge extension, per-turn MCP files and the
/// user's plugins (`extensions/`).
pub fn pi_dir() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("pi"))
}

/// Plugins Vicuna loads into every pi turn: the `.ts`/`.js`/`.mjs` files and
/// the folders (pi loads a folder's index) directly in `dir`, sorted.
pub fn plugin_paths(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            !hidden
                && (p.is_dir()
                    || matches!(p.extension().and_then(|e| e.to_str()), Some("ts") | Some("js") | Some("mjs")))
        })
        .collect();
    out.sort();
    out
}

fn write_bridge(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir.join("extensions")).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("vicuna-bridge.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(BRIDGE_SOURCE) {
        std::fs::write(&path, BRIDGE_SOURCE).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(path)
}

// ---------- MCP: ACP-shaped entries -> pi mcpServers entries ----------

fn name_values(v: Option<&Value>) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for e in v.and_then(|x| x.as_array()).into_iter().flatten() {
        if let (Some(n), Some(val)) = (e.get("name").and_then(|x| x.as_str()), e.get("value").and_then(|x| x.as_str())) {
            out.insert(n.to_string(), json!(val));
        }
    }
    out
}

/// pi server names allow only letters, digits, `_` and `-`.
fn server_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// `{name: {command,args,env} | {url,headers}}` for the bridge extension.
pub fn mcp_servers(entries: &[Value]) -> Option<Value> {
    let mut servers = serde_json::Map::new();
    for e in entries {
        let Some(name) = e.get("name").and_then(|n| n.as_str()) else { continue };
        let cfg = if let Some(url) = e.get("url").and_then(|u| u.as_str()) {
            json!({ "url": url, "headers": name_values(e.get("headers")) })
        } else if let Some(cmd) = e.get("command").and_then(|c| c.as_str()) {
            json!({ "command": cmd, "args": e.get("args").cloned().unwrap_or(json!([])), "env": name_values(e.get("env")) })
        } else {
            continue;
        };
        servers.insert(server_name(name), cfg);
    }
    (!servers.is_empty()).then_some(Value::Object(servers))
}

/// pi's id for vicuna-mcp's ask_user (`mcp__<server>__<tool>`, every char
/// other than letters, digits and `_` replaced by `_`): pre-approved so "ask
/// before running tools" never gates the agent's own question.
pub const ASK_TOOL_ID: &str = "mcp__vicuna_mcp__ask_user";

// ---------- tool cards ----------

/// pi tool names -> Vicuna's card names/args (unknown/MCP/plugin: as-is).
pub fn tool_card(name: &str, input: &Value) -> (String, Value) {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "bash" | "powershell" => ("exec".into(), json!({ "command": s("command") })),
        "read" => ("read".into(), json!({ "path": s("path") })),
        "write" | "edit" => ("write".into(), json!({ "path": s("path") })),
        "grep" | "find" => ("grep".into(), json!({ "pattern": s("pattern"), "path": s("path") })),
        "ls" => ("exec".into(), json!({ "command": format!("ls {}", s("path")).trim_end().to_string() })),
        _ => (name.to_string(), input.clone()),
    }
}

fn result_text(result: &Value) -> String {
    let mut out = match result.get("content") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i.get("type").and_then(|t| t.as_str()) {
                Some("text") => i.get("text").and_then(|t| t.as_str()).map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    if out.chars().count() > OUTPUT_CAP {
        out = out.chars().take(OUTPUT_CAP).collect::<String>();
        out.push_str("\n[output truncated]");
    }
    out
}

/// Per-process translation state.
#[derive(Default)]
pub struct Translator {
    /// toolCallId -> card name (ToolEnd needs the name ToolStart used)
    tools: HashMap<String, String>,
    pub output_chars: usize,
    pub output_tokens: f64,
    pub cost: f64,
    /// errorMessage of the last assistant message that ended in error
    pub error: Option<String>,
    pub model: Option<String>,
}

impl Translator {
    /// One RPC stdout record -> UI events.
    pub fn translate(&mut self, msg: &Value) -> Vec<WorkAgentEvent> {
        let mut out = Vec::new();
        match msg.get("type").and_then(|t| t.as_str()) {
            Some("message_update") => {
                let ev = msg.get("assistantMessageEvent").unwrap_or(&Value::Null);
                let delta = ev.get("delta").and_then(|d| d.as_str()).filter(|d| !d.is_empty());
                match (ev.get("type").and_then(|t| t.as_str()), delta) {
                    (Some("text_delta"), Some(d)) => {
                        self.output_chars += d.chars().count();
                        out.push(WorkAgentEvent::Token { text: d.to_string() });
                    }
                    (Some("thinking_delta"), Some(d)) => out.push(WorkAgentEvent::Think { text: d.to_string() }),
                    _ => {}
                }
            }
            Some("message_end") => {
                let m = msg.get("message").unwrap_or(&Value::Null);
                if m.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(t) = m.pointer("/usage/output").and_then(|t| t.as_f64()) {
                        self.output_tokens += t;
                    }
                    if let Some(c) = m.pointer("/usage/cost/total").and_then(|t| t.as_f64()) {
                        self.cost += c;
                    }
                    if let (Some(p), Some(id)) = (m.get("provider").and_then(|p| p.as_str()), m.get("model").and_then(|p| p.as_str())) {
                        self.model = Some(format!("{p}/{id}"));
                    }
                    if m.get("stopReason").and_then(|s| s.as_str()) == Some("error") {
                        let e = m.get("errorMessage").and_then(|e| e.as_str()).unwrap_or("the model request failed");
                        self.error = Some(crate::exe_resolve::strip_ansi(e));
                    }
                }
            }
            Some("tool_execution_start") => {
                let id = msg.get("toolCallId").and_then(|i| i.as_str()).unwrap_or("").to_string();
                if self.tools.contains_key(&id) {
                    return out;
                }
                let name = msg.get("toolName").and_then(|n| n.as_str()).unwrap_or("tool");
                let (card, args) = tool_card(name, msg.get("args").unwrap_or(&json!({})));
                self.tools.insert(id, card.clone());
                out.push(WorkAgentEvent::ToolStart { name: card, args });
            }
            Some("tool_execution_end") => {
                let id = msg.get("toolCallId").and_then(|i| i.as_str()).unwrap_or("");
                let name = self.tools.remove(id).unwrap_or_else(|| {
                    msg.get("toolName").and_then(|n| n.as_str()).unwrap_or("tool").to_string()
                });
                let mut output = result_text(msg.get("result").unwrap_or(&Value::Null));
                if msg.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) && !output.starts_with("Error:") {
                    output = format!("Error: {output}");
                }
                if output.trim().is_empty() {
                    output = "[completed]".to_string();
                }
                out.push(WorkAgentEvent::ToolEnd { name, output });
            }
            Some("auto_retry_start") => {
                let e = msg.get("errorMessage").and_then(|e| e.as_str()).unwrap_or("");
                out.push(WorkAgentEvent::DisplayNote { text: format!("\n> pi is retrying the model request: {e}\n") });
            }
            Some("compaction_start") => out.push(WorkAgentEvent::Status { label: Some("Compacting context".into()) }),
            Some("compaction_end") => out.push(WorkAgentEvent::Status { label: None }),
            _ => {}
        }
        out
    }
}

// ---------- approvals and extension dialogs ----------

/// `chatKey|tool` pairs answered "Always allow" (pi gets a new process each
/// turn, so the memory lives here, for the app's lifetime).
static ALWAYS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn always_has(k: &str) -> bool {
    ALWAYS.lock().ok().and_then(|g| g.as_ref().map(|s| s.contains(k))).unwrap_or(false)
}

fn always_add(k: String) {
    if let Ok(mut g) = ALWAYS.lock() {
        g.get_or_insert_with(HashSet::new).insert(k);
    }
}

/// The bridge's approval request: (tool, input) from the select title.
pub fn parse_approval(title: &str) -> Option<(String, Value)> {
    let v: Value = serde_json::from_str(title.strip_prefix(APPROVE_PREFIX)?).ok()?;
    Some((v.get("tool")?.as_str()?.to_string(), v.get("input").cloned().unwrap_or(json!({}))))
}

pub fn approval_title(tool: &str, input: &Value) -> String {
    let (card, args) = tool_card(tool, input);
    let detail = ["command", "path", "url", "pattern"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
        .map(|d| format!(": {d}"))
        .unwrap_or_else(|| if input.as_object().is_some_and(|o| !o.is_empty()) { format!(" {input}") } else { String::new() });
    let shown = if card == "exec" || card == "read" || card == "write" || card == "grep" { tool.to_string() } else { card };
    format!("{shown}{detail}")
}

fn ui_response(id: &str, value: Value) -> String {
    let mut v = json!({ "type": "extension_ui_response", "id": id });
    if let (Some(o), Some(extra)) = (v.as_object_mut(), value.as_object()) {
        for (k, x) in extra {
            o.insert(k.clone(), x.clone());
        }
    }
    line(&v)
}

/// extension_ui_request -> answer. Approvals (and other plugins' select /
/// confirm dialogs) become the chat's ask card; notify becomes a transcript
/// note; text-entry dialogs are cancelled (the Work UI has no editor card).
fn handle_ui_request(
    msg: &Value,
    turn: &Arc<TurnState>,
    on_event: &Channel<WorkAgentEvent>,
    writer: &mpsc::UnboundedSender<String>,
    chat_key: &str,
) {
    let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let title = msg.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
    match method {
        "notify" => {
            if let Some(m) = msg.get("message").and_then(|m| m.as_str()).filter(|m| !m.trim().is_empty()) {
                emit_ev(on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> {m}\n") });
            }
        }
        "select" | "confirm" => {
            let approval = parse_approval(&title);
            let (question, options, always_key) = match (&approval, method) {
                (Some((tool, input)), _) => {
                    let key = format!("{chat_key}|{tool}");
                    let t = approval_title(tool, input);
                    if always_has(&key) {
                        let _ = writer.send(ui_response(&id, json!({ "value": "Allow" })));
                        emit_ev(on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> Auto-approved: {t} (always allowed)\n") });
                        return;
                    }
                    (format!("Allow {t}?"), vec!["Allow".to_string(), acp::ALWAYS_LABEL.to_string(), "Deny".to_string()], Some((key, t)))
                }
                (None, "confirm") => {
                    let m = msg.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    (format!("{title}{}", if m.is_empty() { String::new() } else { format!("\n{m}") }), vec!["Yes".into(), "No".into()], None)
                }
                _ => {
                    let opts: Vec<String> = msg.get("options").and_then(|o| o.as_array()).into_iter().flatten()
                        .filter_map(|o| o.as_str().map(str::to_string)).collect();
                    (title.clone(), opts, None)
                }
            };
            let ask_id = format!("perm-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = oneshot::channel::<String>();
            if let Ok(mut m) = turn.ask_pending.lock() {
                m.insert(ask_id.clone(), tx);
            }
            emit_ev(on_event, &turn.stop, WorkAgentEvent::AskRequest { ask_id, question, options: options.clone(), multi: false });
            let (writer, on_event, turn, method) = (writer.clone(), on_event.clone(), turn.clone(), method.to_string());
            tokio::spawn(async move {
                // dismissed / Stop: the sender is dropped -> cancelled
                let answer = rx.await.ok();
                let picked = answer.as_deref().map(str::trim).and_then(|a| options.iter().find(|o| o.eq_ignore_ascii_case(a)).cloned());
                let reply = match (&picked, method.as_str()) {
                    (None, _) => json!({ "cancelled": true }),
                    (Some(p), "confirm") => json!({ "confirmed": p == "Yes" }),
                    (Some(p), _) => json!({ "value": p }),
                };
                let _ = writer.send(ui_response(&id, reply));
                if let Some((key, t)) = always_key {
                    let verdict = match picked.as_deref() {
                        Some(p) if p == acp::ALWAYS_LABEL => {
                            always_add(key);
                            "Always allowed"
                        }
                        Some("Allow") => "Approved",
                        _ => "Denied",
                    };
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> {verdict}: {t}\n") });
                }
            });
        }
        "input" | "editor" => {
            let _ = writer.send(ui_response(&id, json!({ "cancelled": true })));
        }
        _ => {}
    }
}

// ---------- process ----------

fn base_command(cli: &Path) -> Command {
    let mut cmd = Command::new(cli);
    // the terminal's PATH (pi's own dir first: its launcher needs `node`
    // next to it on some installs), so MCP servers resolve like in a shell
    if let Some(p) = crate::exe_resolve::child_path(cli.parent()) {
        cmd.env("PATH", p);
    }
    cmd.env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

/// The fixed RPC arguments of a Work turn.
pub fn rpc_args(model: Option<&str>, resume: Option<&str>, bridge: &Path, plugins: &[PathBuf]) -> Vec<String> {
    let mut a: Vec<String> = ["--mode", "rpc", "--no-approve"].iter().map(|s| s.to_string()).collect();
    if let Some(m) = model {
        a.extend(["--model".into(), m.to_string()]);
    }
    if let Some(r) = resume {
        a.extend(["--session".into(), r.to_string()]);
    }
    a.extend(["-e".into(), bridge.display().to_string()]);
    for p in plugins {
        a.extend(["-e".into(), p.display().to_string()]);
    }
    a
}

fn line(v: &Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

/// `prompt` record; images are data URLs.
pub fn prompt_record(id: &str, text: &str, images: &Option<Vec<String>>) -> Value {
    let mut rec = json!({ "id": id, "type": "prompt", "message": text });
    let imgs: Vec<Value> = images
        .iter()
        .flatten()
        .filter_map(|url| {
            let (meta, data) = url.split_once(',')?;
            let mime = meta.strip_prefix("data:")?.split(';').next()?;
            Some(json!({ "type": "image", "data": data, "mimeType": mime }))
        })
        .collect();
    if !imgs.is_empty() {
        rec["images"] = Value::Array(imgs);
    }
    rec
}

/// End pi's process group (pi and every command it started).
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.filter(|p| *p > 1) {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = pid;
}

// ---------- one Work turn ----------

#[allow(clippy::too_many_arguments)]
pub async fn run_turn_inner(
    turn: &Arc<TurnState>,
    model: Option<&str>,
    workspace: &str,
    messages: &[ChatMessage],
    session_id: Option<&str>,
    goal_text: Option<&str>,
    fresh_session: bool,
    on_event: &Channel<WorkAgentEvent>,
    state: &AcpState,
    mcp_snapshot: &McpSnapshot,
    ask_url: Option<&str>,
) -> Result<(), String> {
    let stop = &turn.stop;
    let cli = find_pi().ok_or_else(|| Engine::Pi.missing_message())?;
    if !Path::new(workspace).is_dir() {
        return Err(format!("workspace folder not found: {workspace} (pick an existing folder for this Work chat)"));
    }
    let last_user = acp::last_user_message(messages).ok_or_else(|| "Work mode: no user message to send".to_string())?;
    let key = acp::session_key(Engine::Pi, session_id, workspace);
    let saved = if fresh_session { None } else { key.as_deref().and_then(|k| state.get(k)) };

    let dir = pi_dir().ok_or("cannot locate home folder")?;
    let bridge = write_bridge(&dir)?;
    let plugins = plugin_paths(&dir.join("extensions"));

    let (mcp, mcp_note) = acp::mcp_servers_for_turn(mcp_snapshot).await;
    if let Some(note) = mcp_note {
        emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n\n> {note}\n\n") });
    }
    let mcp = crate::vicuna_mcp::with_ask_server(mcp, ask_url);
    let mcp_file = match mcp_servers(&mcp) {
        Some(servers) => {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let p = dir.join(format!("mcp-{}-{n}.json", std::process::id()));
            std::fs::write(&p, servers.to_string()).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
            Some(McpFile(p))
        }
        None => None,
    };

    let picked = acp::effective_model(Engine::Pi, model);
    if let Some(m) = &picked {
        emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Pi.reply_label(m) });
    }
    let goal_note = goal_text
        .filter(|g| !g.trim().is_empty())
        .map(|g| format!("\n\n[Session goal: {g} — keep working until it is fully achieved.]"))
        .unwrap_or_default();
    let base_text = format!("{}{}", last_user.content, goal_note);
    let fresh_text = format!(
        "{}\n\n---\n{}{}",
        acp::system_prompt(workspace, None, session_id).trim(),
        acp::history_preamble(messages, last_user),
        base_text
    );
    let attempts: Vec<(Option<String>, String)> = match saved {
        Some(s) => vec![(Some(s), base_text.clone()), (None, fresh_text.clone())],
        None => vec![(None, fresh_text.clone())],
    };
    let chat_key = key.clone().unwrap_or_else(|| format!("pi|{workspace}"));
    let spec = TurnSpec {
        cli: &cli,
        workspace,
        model: picked.as_deref(),
        bridge: &bridge,
        plugins: &plugins,
        mcp_file: mcp_file.as_ref().map(|f| f.0.as_path()),
        chat_key: &chat_key,
    };
    let mut last_err = String::new();
    for (resume, text) in attempts {
        match run_process(&spec, resume.as_deref(), &text, last_user, turn, on_event, state, key.as_deref()).await {
            Ok(()) => {
                emit_ev(on_event, stop, WorkAgentEvent::Done { stopped: stop.load(Ordering::Relaxed) });
                return Ok(());
            }
            Err(RunError::ResumeMissing(e)) => last_err = e,
            Err(RunError::Fatal(e)) => return Err(e),
        }
    }
    Err(last_err)
}

/// The per-turn MCP file, removed when the turn ends.
struct McpFile(PathBuf);

impl Drop for McpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct TurnSpec<'a> {
    cli: &'a Path,
    workspace: &'a str,
    model: Option<&'a str>,
    bridge: &'a Path,
    plugins: &'a [PathBuf],
    mcp_file: Option<&'a Path>,
    chat_key: &'a str,
}

enum RunError {
    /// `--session` named a session pi no longer has (before any output).
    ResumeMissing(String),
    Fatal(String),
}

#[allow(clippy::too_many_arguments)]
async fn run_process(
    spec: &TurnSpec<'_>,
    resume: Option<&str>,
    first_text: &str,
    last_user: &ChatMessage,
    turn: &Arc<TurnState>,
    on_event: &Channel<WorkAgentEvent>,
    state: &AcpState,
    key: Option<&str>,
) -> Result<(), RunError> {
    let stop = &turn.stop;
    let mut cmd = base_command(spec.cli);
    cmd.args(rpc_args(spec.model, resume, spec.bridge, spec.plugins))
        .current_dir(spec.workspace)
        .stdin(std::process::Stdio::piped())
        .env("VICUNA_PI_ASK", if crate::work_engine::approval_ask() { "1" } else { "0" })
        .env("VICUNA_PI_ALLOW", ASK_TOOL_ID);
    if let Some(f) = spec.mcp_file {
        cmd.env("VICUNA_PI_MCP", f);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| RunError::Fatal(crate::work_engine::spawn_error("pi", spec.cli, Path::new(spec.workspace), &e)))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().ok_or(RunError::Fatal("pi: no stdin".into()))?;
    let stdout = child.stdout.take().ok_or(RunError::Fatal("pi: no stdout".into()))?;
    let stderr = child.stderr.take().ok_or(RunError::Fatal("pi: no stderr".into()))?;

    let (writer, mut write_rx) = mpsc::unbounded_channel::<String>();
    let write_task = tokio::spawn(async move {
        while let Some(l) = write_rx.recv().await {
            if stdin.write_all(l.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let err_tail = Arc::new(Mutex::new(String::new()));
    let tail = err_tail.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Ok(mut s) = tail.lock() {
                s.push_str(&l);
                s.push('\n');
                if s.len() > 4000 {
                    let cut = s.len() - 4000;
                    let cut = (cut..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                    s.drain(..cut);
                }
            }
        }
    });

    let _ = writer.send(line(&prompt_record("vicuna-1", first_text, &last_user.images)));
    let _ = writer.send(line(&json!({ "id": "vicuna-state", "type": "get_state" })));
    emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "fresh".into() });

    // split only on LF (JSON strings may hold U+2028)
    let mut lines = BufReader::new(stdout).split(b'\n');
    let mut tr = Translator::default();
    let mut started = Instant::now();
    let mut got_output = false;
    let mut aborted_at: Option<Instant> = None;
    let mut writer = Some(writer);
    let mut prompt_seq = 1u32;
    let outcome: Result<(), RunError> = loop {
        tokio::select! {
            next = lines.next_segment() => {
                let Ok(Some(raw)) = next else {
                    if stop.load(Ordering::Relaxed) || writer.is_none() { break Ok(()); }
                    let tail = err_tail.lock().map(|s| s.trim().to_string()).unwrap_or_default();
                    let tail = crate::exe_resolve::strip_ansi(&tail);
                    if resume.is_some() && !got_output {
                        break Err(RunError::ResumeMissing(format!("pi could not resume the session: {tail}")));
                    }
                    break Err(RunError::Fatal(acp::friendly_prompt_error(Engine::Pi,
                        if tail.is_empty() { "pi exited unexpectedly".to_string() } else { format!("pi exited unexpectedly:\n{tail}") })));
                };
                let raw = String::from_utf8_lossy(&raw);
                let Ok(msg) = serde_json::from_str::<Value>(raw.trim_end_matches('\r')) else { continue };
                match msg.get("type").and_then(|t| t.as_str()) {
                    Some("response") => {
                        let ok = msg.get("success").and_then(|s| s.as_bool()).unwrap_or(true);
                        if msg.get("id").and_then(|i| i.as_str()) == Some("vicuna-state") {
                            if let (Some(k), Some(sid)) = (key, msg.pointer("/data/sessionId").and_then(|s| s.as_str())) {
                                state.set(k.to_string(), sid.to_string());
                            }
                            if spec.model.is_none() {
                                if let (Some(p), Some(id)) = (msg.pointer("/data/model/provider").and_then(|s| s.as_str()), msg.pointer("/data/model/id").and_then(|s| s.as_str())) {
                                    emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Pi.reply_label(&format!("{p}/{id}")) });
                                }
                            }
                        } else if !ok && msg.get("command").and_then(|c| c.as_str()) == Some("prompt") {
                            let e = msg.get("error").and_then(|e| e.as_str()).unwrap_or("pi rejected the prompt").to_string();
                            break Err(RunError::Fatal(acp::friendly_prompt_error(Engine::Pi, e)));
                        } else if msg.get("command").and_then(|c| c.as_str()) == Some("prompt")
                            && msg.pointer("/data/disposition").and_then(|d| d.as_str()) == Some("handled") {
                            writer = None; // no run started: nothing to wait for
                        }
                        got_output = true;
                    }
                    Some("extension_ui_request") => {
                        if let Some(w) = &writer {
                            handle_ui_request(&msg, turn, on_event, w, spec.chat_key);
                        }
                    }
                    Some("agent_settled") => {
                        got_output = true;
                        let gen_ms = started.elapsed().as_millis() as u64;
                        let secs = (gen_ms as f64 / 1000.0).max(0.001);
                        let gen_tps = (tr.output_tokens > 0.0).then(|| tr.output_tokens / secs)
                            .or_else(|| (tr.output_chars > 0).then(|| (tr.output_chars as f64 / 4.0) / secs));
                        let gen_cost = (tr.cost > 0.0).then_some(tr.cost);
                        emit_ev(on_event, stop, WorkAgentEvent::RoundEnd { gen_ms, gen_tps, gen_cost });
                        let aborted = msg.get("aborted").and_then(|a| a.as_bool()).unwrap_or(false);
                        if let Some(e) = tr.error.take() {
                            if !stop.load(Ordering::Relaxed) && !aborted {
                                break Err(RunError::Fatal(acp::friendly_prompt_error(Engine::Pi, e)));
                            }
                        }
                        if stop.load(Ordering::Relaxed) {
                            writer = None; // close stdin: pi exits
                            continue;
                        }
                        let queued = acp::take_queue(turn);
                        if queued.is_empty() {
                            writer = None;
                            continue;
                        }
                        let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
                        let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
                        emit_ev(on_event, stop, WorkAgentEvent::QueueDrained { ids, messages: drained.clone() });
                        let (t, imgs) = acp::messages_text(&drained);
                        prompt_seq += 1;
                        if let Some(w) = &writer {
                            let _ = w.send(line(&prompt_record(&format!("vicuna-{prompt_seq}"), &t, &imgs)));
                        }
                        tr.output_chars = 0;
                        tr.output_tokens = 0.0;
                        tr.cost = 0.0;
                        started = Instant::now();
                        emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "continue".into() });
                    }
                    _ => {
                        for ev in tr.translate(&msg) {
                            got_output = true;
                            emit_ev(on_event, stop, ev);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                if stop.load(Ordering::Relaxed) && aborted_at.is_none() {
                    aborted_at = Some(Instant::now());
                    // open ask cards answer "cancelled" (Deny) as their senders drop
                    if let Ok(mut m) = turn.ask_pending.lock() { m.clear(); }
                    if let Some(w) = &writer {
                        let _ = w.send(line(&json!({ "id": "vicuna-abort", "type": "abort" })));
                    }
                }
                if aborted_at.is_some_and(|t| t.elapsed() > CANCEL_GRACE) {
                    break Ok(());
                }
            }
        }
    };
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    // pi and anything it started (a long bash command, MCP servers)
    kill_group(pid);
    let _ = child.start_kill();
    let _ = child.wait().await;
    write_task.abort();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("vicuna-pi-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_cli_in_pi_agent_bin() {
        let home = tmp("home");
        let bin = home.join(".pi").join("agent").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join("pi");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_pi_in(None, Some(OsString::new()), Some(&home)), Some(p));
    }

    #[test]
    fn list_models_table_parses() {
        // `pi --list-models` output of pi 1.5.0
        let out = "provider  model                      context  max-out  thinking  images\n\
                   ollama    deepseek-v4.1-flash:cloud  1.0M     16.4K    yes       yes   \n\
                   openai    gpt-6-luna                 400K     128K     yes       yes\n\
                   ollama    deepseek-v4.1-flash:cloud  1.0M     16.4K    yes       yes\n\n";
        let m: Vec<_> = parse_models_output(out).into_iter().map(|m| (m.id, m.provider)).collect();
        assert_eq!(
            m,
            vec![("ollama/deepseek-v4.1-flash:cloud".into(), "ollama".into()), ("openai/gpt-6-luna".into(), "openai".into())]
        );
        assert!(parse_models_output("").is_empty());
    }

    #[test]
    fn plugins_are_scripts_and_folders_sorted() {
        let d = tmp("plugins");
        std::fs::write(d.join("b.ts"), "").unwrap();
        std::fs::write(d.join("a.js"), "").unwrap();
        std::fs::write(d.join("notes.md"), "").unwrap();
        std::fs::write(d.join(".hidden.ts"), "").unwrap();
        std::fs::create_dir_all(d.join("c-pkg")).unwrap();
        let names: Vec<_> = plugin_paths(&d).iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["a.js", "b.ts", "c-pkg"]);
        assert!(plugin_paths(&d.join("missing")).is_empty());
        // the bridge is (re)written and its extensions folder created
        let b = write_bridge(&d.join("pi")).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), BRIDGE_SOURCE);
        assert!(d.join("pi").join("extensions").is_dir());
    }

    #[test]
    fn rpc_args_load_bridge_then_plugins() {
        let a = rpc_args(Some("ollama/x:cloud"), Some("sid1"), Path::new("/v/bridge.ts"), &[PathBuf::from("/v/ext/a.ts")]);
        assert_eq!(
            a,
            ["--mode", "rpc", "--no-approve", "--model", "ollama/x:cloud", "--session", "sid1", "-e", "/v/bridge.ts", "-e", "/v/ext/a.ts"]
        );
        assert_eq!(rpc_args(None, None, Path::new("/b.ts"), &[]), ["--mode", "rpc", "--no-approve", "-e", "/b.ts"]);
    }

    #[test]
    fn mcp_entries_become_pi_servers() {
        let s = mcp_servers(&[
            json!({ "name": "probe", "command": "/usr/bin/python3", "args": ["s.py", "3"], "env": [{ "name": "K", "value": "v" }] }),
            json!({ "type": "http", "name": "vicuna-mcp", "url": "http://127.0.0.1:9/mcp/t", "headers": [] }),
            json!({ "name": "odd name.x", "url": "https://m/mcp", "headers": [{ "name": "Authorization", "value": "Bearer t" }] }),
        ])
        .unwrap();
        assert_eq!(s["probe"], json!({ "command": "/usr/bin/python3", "args": ["s.py", "3"], "env": { "K": "v" } }));
        assert_eq!(s["vicuna-mcp"], json!({ "url": "http://127.0.0.1:9/mcp/t", "headers": {} }));
        assert_eq!(s["odd_name_x"]["headers"]["Authorization"], "Bearer t");
        assert_eq!(mcp_servers(&[]), None);
    }

    /// Records captured from a real `pi --mode rpc` turn (pi 1.5.0, ollama
    /// deepseek-v4.1-flash:cloud): deltas stream, the tool card gets the
    /// complete args, the result closes it; a denied call ends as an error.
    #[test]
    fn rpc_records_map_to_ui_events() {
        let mut tr = Translator::default();
        let delta = |t: &str, d: &str| json!({ "type": "message_update", "assistantMessageEvent": { "type": t, "delta": d } });
        assert!(matches!(&tr.translate(&delta("thinking_delta", "hmm"))[..], [WorkAgentEvent::Think { text }] if text == "hmm"));
        assert!(tr.translate(&delta("thinking_delta", "")).is_empty());
        assert!(matches!(&tr.translate(&delta("text_delta", "DONE"))[..], [WorkAgentEvent::Token { text }] if text == "DONE"));
        assert_eq!(tr.output_chars, 4);
        let start = json!({ "type": "tool_execution_start", "toolCallId": "call_1", "toolName": "bash", "args": { "command": "cat note.txt" } });
        match &tr.translate(&start)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "cat note.txt");
            }
            other => panic!("{other:?}"),
        }
        assert!(tr.translate(&start).is_empty(), "a repeated start adds no second card");
        let end = json!({ "type": "tool_execution_end", "toolCallId": "call_1", "toolName": "bash",
            "result": { "content": [{ "type": "text", "text": "secret-word: tangerine-42\n" }] }, "isError": false, "durationMs": 25 });
        assert!(matches!(&tr.translate(&end)[..], [WorkAgentEvent::ToolEnd { name, output }] if name == "exec" && output.contains("tangerine-42")));
        let mcp = json!({ "type": "tool_execution_start", "toolCallId": "call_2", "toolName": "mcp__vicuna_probe__noop_001", "args": { "note": "hi" } });
        assert!(matches!(&tr.translate(&mcp)[..], [WorkAgentEvent::ToolStart { name, args }] if name == "mcp__vicuna_probe__noop_001" && args["note"] == "hi"));
        let denied = json!({ "type": "tool_execution_end", "toolCallId": "call_2", "toolName": "mcp__vicuna_probe__noop_001",
            "result": { "content": [{ "type": "text", "text": "The user denied this tool call in Vicuna." }], "details": {} }, "isError": true });
        assert!(matches!(&tr.translate(&denied)[..], [WorkAgentEvent::ToolEnd { output, .. }] if output == "Error: The user denied this tool call in Vicuna."));
        let fin = json!({ "type": "message_end", "message": { "role": "assistant", "provider": "ollama", "model": "deepseek-v4.1-flash:cloud",
            "usage": { "output": 40, "cost": { "total": 0.0 } }, "stopReason": "stop" } });
        assert!(tr.translate(&fin).is_empty());
        assert_eq!(tr.output_tokens, 40.0);
        assert_eq!(tr.model.as_deref(), Some("ollama/deepseek-v4.1-flash:cloud"));
        assert_eq!(tr.error, None);
        let err = json!({ "type": "message_end", "message": { "role": "assistant", "stopReason": "error", "errorMessage": "401 Unauthorized" } });
        tr.translate(&err);
        assert_eq!(tr.error.as_deref(), Some("401 Unauthorized"));
    }

    #[test]
    fn approval_requests_parse_and_read_well() {
        let title = r#"vicuna-approve:{"tool":"bash","id":"call_1","input":{"command":"echo two"}}"#;
        let (tool, input) = parse_approval(title).unwrap();
        assert_eq!(tool, "bash");
        assert_eq!(approval_title(&tool, &input), "bash: echo two");
        assert_eq!(approval_title("write", &json!({ "path": "a.txt", "content": "x" })), "write: a.txt");
        assert_eq!(approval_title("mcp__p__noop_001", &json!({ "note": "hi" })), r#"mcp__p__noop_001 {"note":"hi"}"#);
        assert_eq!(approval_title("vicuna_stamp", &json!({})), "vicuna_stamp");
        assert!(parse_approval("Allow dangerous command?").is_none());
        let r: Value = serde_json::from_str(ui_response("u1", json!({ "value": "Allow" })).trim()).unwrap();
        assert_eq!(r, json!({ "type": "extension_ui_response", "id": "u1", "value": "Allow" }));
        assert!(!always_has("k|bash"));
        always_add("k|bash".into());
        assert!(always_has("k|bash") && !always_has("k|write"));
    }

    #[test]
    fn prompt_record_carries_images() {
        let r = prompt_record("p1", "hi", &Some(vec!["data:image/png;base64,AAAA".into(), "bogus".into()]));
        assert_eq!(r["type"], "prompt");
        assert_eq!(r["message"], "hi");
        assert_eq!(r["images"], json!([{ "type": "image", "data": "AAAA", "mimeType": "image/png" }]));
        assert!(prompt_record("p1", "hi", &None).get("images").is_none());
    }
}
