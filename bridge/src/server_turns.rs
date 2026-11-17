// Server-owned Work turns (`vicuna serve`).
//
// A server must keep working with every window closed (laptop lid shut):
//   * each Work turn's transcript is recorded into the server db as it runs
//     (persist::Recorder; writes throttled to ~2 s, plus one at the end);
//   * what used to happen in the window when a turn ended happens here:
//     leftover queued prompts start the next turn, a worker's reply reaches
//     its lead, an MCP server added mid-turn wakes the session;
//   * scheduled sends (the composer's and the agent's `schedule_wakeup`) are
//     stored in the db and delivered by a server timer.
// Windows still drive everything they start; they follow server-started
// turns through `work_agent_live` / `work_agent_attach` like any other.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use vicuna_lib::work_agent::{agent_answer, agent_queue_msg, TurnState, WorkAgentEvent};
use vicuna_lib::work_engine::Engine;
use vicuna_lib::ChatMessage;

use crate::persist::{self, Recorder};
use crate::BridgeState;

/// How often a live turn's transcript is written while it streams.
const WRITE_EVERY: Duration = Duration::from_secs(2);

/// One recorded turn (per event channel).
pub struct RecState {
    pub rec: Recorder,
    pub turn_id: String,
    /// the terminal event arrived: the final write + follow-ups are running,
    /// later events wait in `deferred` so they reach windows in order
    pub finishing: bool,
    pub deferred: Vec<String>,
    /// lead session to report to when this turn ends
    pub report_to: Option<String>,
    /// the session was deleted mid-turn: stop writing
    pub gone: bool,
}

pub type Recorders = Arc<Mutex<HashMap<String, Arc<Mutex<RecState>>>>>;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("os randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Sessions whose transcript the server is writing right now: client saves
/// for them are refused (the server is their only writer until the turn ends).
pub fn live_sessions(st: &BridgeState) -> HashSet<String> {
    let recs: Vec<Arc<Mutex<RecState>>> = st.recorders.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
    recs.iter().filter_map(|r| r.lock().ok().map(|r| r.rec.chat_id.clone())).collect()
}

fn chat_message(v: &Value) -> Option<ChatMessage> {
    let mut o = json!({
        "role": v.get("role").and_then(Value::as_str).unwrap_or("user"),
        "content": v.get("content").and_then(Value::as_str).unwrap_or(""),
    });
    if let Some(imgs) = v.get("images").filter(|i| i.as_array().is_some_and(|a| !a.is_empty())) {
        o["images"] = imgs.clone();
    }
    serde_json::from_value(o).ok()
}

pub struct LaunchParams {
    pub uuid: String,
    pub engine: Engine,
    pub model: Option<String>,
    pub workspace: String,
    pub messages: Vec<ChatMessage>,
    pub session_id: Option<String>,
    pub goal_text: Option<String>,
    pub fresh_session: bool,
    pub lead: bool,
    pub rec: Option<Recorder>,
    pub report_to: Option<String>,
}

/// The live turn of a session, if one is still running.
pub fn live_turn(st: &BridgeState, session_id: &str) -> Option<(String, Arc<TurnState>)> {
    let (tid, _, _) = st.session_turns.lock().ok()?.get(session_id).cloned()?;
    let turn = st.agent.get_turn(&tid)?;
    (!turn.done.load(Ordering::Relaxed)).then_some((tid, turn))
}

/// Start a Work turn (window-started or server-started) and return its id.
pub async fn launch(st: &Arc<BridgeState>, p: LaunchParams) -> Result<String, String> {
    if !p.engine.is_present() {
        return Err(p.engine.missing_message());
    }
    if let Some(sid) = &p.session_id {
        if let Some((tid, _)) = live_turn(st, sid) {
            return Err(format!(
                "this session already has a Work turn running on the server ({tid}); reopen the session to follow it"
            ));
        }
    }
    let turn = Arc::new(TurnState::new());
    turn.lead.store(p.lead, Ordering::Relaxed);
    let turn_id = st.agent.next_turn(turn.clone());
    let mcp_snapshot = vicuna_lib::acp::McpSnapshot::capture(&st.mcp, crate::test_resource_dir().as_deref()).await;
    if let (Some(sid), Ok(mut m)) = (&p.session_id, st.session_turns.lock()) {
        m.insert(sid.clone(), (turn_id.clone(), p.uuid.clone(), now_ms() as u64));
    }
    let rs = match (p.rec, &st.db) {
        (Some(rec), Some(_)) => {
            let rs = Arc::new(Mutex::new(RecState {
                rec,
                turn_id: turn_id.clone(),
                finishing: false,
                deferred: Vec::new(),
                report_to: p.report_to,
                gone: false,
            }));
            if let Ok(mut m) = st.recorders.lock() {
                m.insert(p.uuid.clone(), rs.clone());
            }
            spawn_writer(st.clone(), rs.clone());
            Some(rs)
        }
        _ => None,
    };
    let on_event = work_channel(st.clone(), p.uuid.clone(), rs, turn.clone(), p.session_id.clone());
    let acp = st.acp.clone();
    let session_turns = st.session_turns.clone();
    let live_key = p.session_id.clone().map(|sid| (sid, turn_id.clone()));
    let (model, workspace, messages, session_id, goal_text, fresh, engine) =
        (p.model, p.workspace, p.messages, p.session_id, p.goal_text, p.fresh_session, p.engine);
    tokio::spawn(async move {
        vicuna_lib::acp::run_turn(
            turn, model, workspace, messages, session_id, goal_text, fresh, on_event, &acp, mcp_snapshot, engine,
        )
        .await;
        if let (Some((sid, tid)), Ok(mut m)) = (live_key, session_turns.lock()) {
            if m.get(&sid).is_some_and(|(t, _, _)| *t == tid) {
                m.remove(&sid);
            }
        }
    });
    Ok(turn_id)
}

/// The turn's event channel: every event lands in the poll queue for
/// windows, and (server mode) in the recorder; the agent's wake-ups and its
/// session-control requests are handled here when no window can.
fn work_channel(
    st: Arc<BridgeState>,
    uuid: String,
    rs: Option<Arc<Mutex<RecState>>>,
    turn: Arc<TurnState>,
    session_id: Option<String>,
) -> Channel<WorkAgentEvent> {
    Channel::<WorkAgentEvent>::new(move |body| {
        let InvokeResponseBody::Json(raw) = body else { return Ok(()) };
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        // the server keeps its own Scheduled list: the agent's wake-up goes
        // there (delivered with every window closed), never to a window
        if kind == "schedulewakeup" && st.db.is_some() {
            if let Some(sid) = session_id.clone() {
                let (st, turn) = (st.clone(), turn.clone());
                tokio::spawn(async move { handle_wakeup(&st, &turn, &sid, &v).await });
                return Ok(());
            }
        }
        // Server-owned session management must keep worker chains moving
        // even with every window asleep. Deletion retains the user card.
        if kind == "sessioncontrol" && st.db.is_some() && v.get("action").and_then(Value::as_str) != Some("delete") {
            if let Some(sid) = session_id.clone() {
                let (st, turn) = (st.clone(), turn.clone());
                tokio::spawn(async move {
                    let req = v.get("req_id").and_then(Value::as_str).unwrap_or("");
                    let action = v.get("action").and_then(Value::as_str).unwrap_or("");
                    let result = session_control(&st, &sid, action, &v["args"]).await;
                    let reply = match result {
                        Ok(text) => json!({"ok": true, "text": text}),
                        Err(error) => json!({"ok": false, "error": error}),
                    };
                    let _ = agent_answer(&turn, req, reply.to_string());
                });
                return Ok(());
            }
        }
        let Some(rs) = &rs else {
            crate::push_event(&st.events, &uuid, &raw);
            return Ok(());
        };
        let Ok(mut g) = rs.lock() else { return Ok(()) };
        if g.finishing {
            g.deferred.push(raw);
            return Ok(());
        }
        g.rec.apply(&v, &raw);
        if kind == "done" || kind == "error" {
            g.finishing = true;
            g.deferred.push(raw);
            let stopped = kind == "done" && v.get("stopped").and_then(Value::as_bool) == Some(true);
            let (st, rs, uuid, turn) = (st.clone(), rs.clone(), uuid.clone(), turn.clone());
            tokio::spawn(async move { finalize(st, rs, uuid, turn, stopped).await });
            return Ok(());
        }
        // pushed under the recorder lock: a reattaching window's snapshot
        // (which takes the same lock) never sees an event twice or misses one
        crate::push_event(&st.events, &uuid, &raw);
        Ok(())
    })
}

async fn write_rec(st: &BridgeState, rs: &Arc<Mutex<RecState>>, force: bool) {
    let Some(db) = &st.db else { return };
    let _write = st.chat_lock.lock().await;
    let (id, msgs, touched) = {
        let Ok(mut g) = rs.lock() else { return };
        if g.gone || (!force && (g.finishing || (!g.rec.dirty && !g.rec.touched))) {
            return;
        }
        let out = (g.rec.chat_id.clone(), g.rec.messages.clone(), g.rec.touched);
        g.rec.dirty = false;
        g.rec.touched = false;
        out
    };
    if let Err(e) = vicuna_lib::server_store::write_messages(db, &id, &msgs, touched.then(now_ms)).await {
        eprintln!("vicuna: {e}");
        if e.contains("no longer exists") {
            if let Ok(mut g) = rs.lock() {
                g.gone = true;
            }
        } else if let Ok(mut g) = rs.lock() {
            g.rec.dirty = true; // try again next tick
        }
    }
}

fn spawn_writer(st: Arc<BridgeState>, rs: Arc<Mutex<RecState>>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WRITE_EVERY).await;
            if rs.lock().map(|g| g.finishing || g.gone).unwrap_or(true) {
                break;
            }
            write_rec(&st, &rs, false).await;
        }
    });
}

/// The turn ended: final write, then everything a window used to do at the
/// end of a turn, then release the terminal event to windows.
async fn finalize(st: Arc<BridgeState>, rs: Arc<Mutex<RecState>>, uuid: String, turn: Arc<TurnState>, stopped: bool) {
    write_rec(&st, &rs, true).await;
    // the backend loop is about to return; follow-ups must start a new turn,
    // not be queued into this finished one
    let until = Instant::now() + Duration::from_secs(30);
    while !turn.done.load(Ordering::Relaxed) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (chat_id, reply, report_to, gone) = rs
        .lock()
        .map(|g| (g.rec.chat_id.clone(), g.rec.last_reply(), g.report_to.clone(), g.gone))
        .unwrap_or_default();
    if !gone && st.db.is_some() {
        // prompts queued after the engine's last round: run them next (an
        // aborted turn leaves them to the window that queued them, which puts
        // them back into its composer)
        let leftovers: Vec<Value> = if stopped {
            Vec::new()
        } else {
            turn.queue.lock().map(|mut q| q.drain(..).collect::<Vec<_>>()).unwrap_or_default()
                .into_iter()
                .filter_map(|(_, m)| serde_json::to_value(m).ok())
                .collect()
        };
        if !leftovers.is_empty() {
            if let Err(e) = start_server_turn(&st, &chat_id, leftovers, None).await {
                eprintln!("vicuna: queued prompts for {chat_id} not sent: {e}");
            }
        }
        if let Some(lead) = report_to {
            report_to_lead(&st, &chat_id, &lead, &reply, stopped).await;
        }
        let wakes = st.mcp_wakes.lock().ok().and_then(|mut m| m.remove(&chat_id)).unwrap_or_default();
        if !wakes.is_empty() && !stopped {
            let msg = json!({"role": "user", "content": persist::mcp_wake_message(&wakes)});
            if let Err(e) = deliver(&st, &chat_id, msg).await {
                eprintln!("vicuna: MCP wake for {chat_id} not sent: {e}");
            }
        }
    }
    if let Ok(mut g) = rs.lock() {
        for raw in std::mem::take(&mut g.deferred) {
            crate::push_event(&st.events, &uuid, &raw);
        }
    }
    if let Ok(mut m) = st.recorders.lock() {
        m.remove(&uuid);
    }
}

async fn report_to_lead(st: &Arc<BridgeState>, worker: &str, lead: &str, reply: &str, stopped: bool) {
    let Some(db) = &st.db else { return };
    let Ok(Some(w)) = vicuna_lib::server_store::load_chat(db, worker).await else { return };
    let extra = parse_extra(&w);
    let engine = vicuna_lib::work_engine::effective_engine(extra.engine.as_deref());
    let model = if extra.engine.is_some() { extra.work_model.clone() } else { engine.saved_model() };
    let title = w.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let msg = json!({
        "role": "user",
        "content": persist::worker_done_message(worker, &title, engine.id(), model.as_deref(), reply, stopped),
    });
    // the lead may be mid-compaction in a window or briefly unavailable: retry
    for _ in 0..30 {
        match deliver(st, lead, msg.clone()).await {
            Ok(()) => return,
            Err(e) if e.contains("no longer exists") || e.contains("not a Work session") => {
                eprintln!("vicuna: worker report for {lead} dropped: {e}");
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

#[derive(Default)]
struct Extra {
    engine: Option<String>,
    work_model: Option<String>,
    lead: bool,
}

fn parse_extra(chat: &Value) -> Extra {
    let o: Value = chat.get("extra").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
    let s = |k: &str| o.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
    Extra { engine: s("engine"), work_model: s("workModel"), lead: o.get("role").and_then(Value::as_str) == Some("lead") }
}

/// Send a message into a session the way a window would: queued into its
/// running turn, else as a new server-started turn.
pub async fn deliver(st: &Arc<BridgeState>, chat_id: &str, msg: Value) -> Result<(), String> {
    if let Some((_, turn)) = live_turn(st, chat_id) {
        let m = chat_message(&msg).ok_or("bad message")?;
        return agent_queue_msg(&turn, uuid(), m);
    }
    start_server_turn(st, chat_id, vec![msg], None).await.map(|_| ())
}

/// Start a turn the server decided on (no window involved): append the user
/// message(s) and a placeholder to the stored session, then run it with the
/// session's own harness/model (or the server's saved pick).
pub async fn start_server_turn(st: &Arc<BridgeState>, chat_id: &str, user_msgs: Vec<Value>, report_to: Option<String>) -> Result<String, String> {
    let _write = st.chat_lock.lock().await;
    if live_turn(st, chat_id).is_some() {
        return Err(format!("session {chat_id} already has a Work turn running"));
    }
    let db = st.db.as_ref().ok_or("no server db")?;
    let chat = vicuna_lib::server_store::load_chat(db, chat_id)
        .await?
        .ok_or_else(|| format!("session {chat_id} no longer exists"))?;
    if chat.get("mode").and_then(Value::as_str) != Some("work") {
        return Err(format!("session {chat_id} is not a Work session"));
    }
    let workspace = chat.get("workspace").and_then(Value::as_str).unwrap_or("").to_string();
    if workspace.is_empty() {
        return Err(format!("session {chat_id} has no workspace"));
    }
    let extra = parse_extra(&chat);
    let engine = vicuna_lib::work_engine::effective_engine(extra.engine.as_deref());
    if !engine.is_present() {
        return Err(engine.missing_message());
    }
    let pick = if extra.engine.is_some() { extra.work_model.clone() } else { engine.saved_model() };
    let mut messages: Vec<Value> = chat.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    messages.extend(user_msgs);
    messages.push(persist::assistant_placeholder(&persist::work_reply_model(None, pick.as_deref(), engine.id())));
    vicuna_lib::server_store::write_messages(db, chat_id, &messages, Some(now_ms())).await?;
    let native: Vec<ChatMessage> =
        persist::compacted_history(&messages[..messages.len() - 1]).iter().filter_map(chat_message).collect();
    let goal_text = chat
        .get("goal")
        .and_then(Value::as_str)
        .and_then(|g| serde_json::from_str::<Value>(g).ok())
        .filter(|g| g.get("paused").and_then(Value::as_bool) != Some(true))
        .and_then(|g| g.get("text").and_then(Value::as_str).map(str::to_string));
    let idx = messages.len() - 1;
    let rec = Recorder::new(chat_id, messages, idx, pick.clone(), engine.id());
    launch(
        st,
        LaunchParams {
            uuid: uuid(),
            engine,
            model: Some(pick.unwrap_or_default()),
            workspace,
            messages: native,
            session_id: Some(chat_id.to_string()),
            goal_text,
            fresh_session: false,
            lead: extra.lead,
            rec: Some(rec),
            report_to,
        },
    )
    .await
}

/// Non-destructive lead actions run against the server database. User
/// confirmation for delete remains with the connected window.
async fn session_control(st: &Arc<BridgeState>, lead_id: &str, action: &str, args: &Value) -> Result<String, String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let report = args.get("report").and_then(Value::as_bool) != Some(false);
    if action == "create" {
        let lead = vicuna_lib::server_store::load_chat(db, lead_id).await?.ok_or("lead session no longer exists")?;
        let requested = arg("engine");
        let engine = if requested.is_empty() { vicuna_lib::work_engine::current_engine() }
            else { Engine::parse(&requested).ok_or("unknown engine")? };
        if !engine.is_present() { return Err(engine.missing_message()); }
        let ws = if arg("workspace").is_empty() { lead["workspace"].as_str().unwrap_or("").to_string() } else { arg("workspace") };
        let ws = vicuna_lib::work_validate_root_logic(ws).await?;
        let msg = arg("message");
        let title: String = if !arg("title").is_empty() { arg("title") } else if !msg.is_empty() { msg.clone() } else { "Worker".into() }.chars().take(40).collect();
        let id = uuid();
        let model = arg("model");
        let extra = json!({"engine":engine.id(), "workModel": if model.is_empty() {Value::Null} else {json!(model)}, "leadId":lead_id});
        vicuna_lib::server_store::create_chat(db, &json!({"id":id,"title":title,"createdAt":now_ms(),"updatedAt":now_ms(),"mode":"work","workspace":ws,"extra":extra.to_string(),"messages":[]})).await?;
        if !msg.is_empty() {
            start_server_turn(st, &id, vec![json!({"role":"user","content":msg})], report.then(|| lead_id.to_string())).await?;
        }
        return Ok(format!("Created \"{title}\" (session_id={id}, engine={}, workspace={ws}).{}", engine.id(), if !msg.is_empty() {" Its turn has started; its reply will reach the lead when it ends if reporting is enabled."} else {""}));
    }
    if action == "list" {
        let chats = vicuna_lib::server_store::load_chats(db).await?;
        let rows: Vec<String> = chats.as_array().into_iter().flatten().filter(|c| c["mode"] == "work").take(40).map(|c| {
            let id = c["id"].as_str().unwrap_or("");
            format!("- {id} · \"{}\" · {} · {}", c["title"].as_str().unwrap_or(""), if live_turn(st,id).is_some() {"running"} else {"idle"}, c["workspace"].as_str().unwrap_or(""))
        }).collect();
        return Ok(format!("Work sessions:\n{}", rows.join("\n")));
    }
    let id = if action == "set_workspace" && arg("session_id").is_empty() { lead_id.to_string() } else { arg("session_id") };
    if id.is_empty() { return Err("`session_id` is required (see list).".into()); }
    if id == lead_id && action != "set_workspace" { return Err("That is your own session.".into()); }
    let mut chat = vicuna_lib::server_store::load_chat(db, &id).await?.ok_or_else(|| format!("No session {id}."))?;
    match action {
        "status" => {
            let reply = chat["messages"].as_array().and_then(|m|m.last()).and_then(|m|m["content"].as_str()).unwrap_or("");
            Ok(format!("{id}: {}\nLast reply:\n{}", if live_turn(st,&id).is_some() {"running"} else {"idle"}, persist::tail_text(reply,3000)))
        }
        "send" => {
            let msg = arg("message");
            if msg.is_empty() { return Err("`message` is required.".into()); }
            if let Some((tid, turn)) = live_turn(st, &id) {
                if report {
                    let recs: Vec<_> = st.recorders.lock().map(|m|m.values().cloned().collect()).unwrap_or_default();
                    for rs in recs { if let Ok(mut r) = rs.lock() { if r.turn_id == tid {r.report_to = Some(lead_id.to_string());} } }
                }
                agent_queue_msg(&turn, uuid(), chat_message(&json!({"role":"user","content":msg})).ok_or("bad message")?)?;
                Ok(format!("Queued into session {id}."))
            } else {
                start_server_turn(st,&id,vec![json!({"role":"user","content":msg})],report.then(||lead_id.to_string())).await?;
                Ok(format!("Started a turn in {id}."))
            }
        }
        "stop" => {
            if let Some((_,turn)) = live_turn(st,&id) { vicuna_lib::work_agent::agent_stop(&turn); }
            Ok(format!("Stopped {id}."))
        }
        "archive" | "unarchive" | "set_workspace" => {
            let _write = st.chat_lock.lock().await;
            chat = vicuna_lib::server_store::load_chat(db, &id).await?.ok_or("session no longer exists")?;
            if action == "set_workspace" {
                if id != lead_id && live_turn(st,&id).is_some() { return Err("Session is running; stop it or wait first.".into()); }
                chat["workspace"] = json!(vicuna_lib::work_validate_root_logic(arg("workspace")).await?);
                chat["mode"] = json!("work");
            } else {
                let mut extra: Value = chat["extra"].as_str().and_then(|s|serde_json::from_str(s).ok()).unwrap_or(json!({}));
                if action == "archive" && extra["role"] == "lead" { return Err("Refusing to archive a lead session.".into()); }
                extra["archived"] = json!(action == "archive");
                chat["extra"] = json!(extra.to_string());
            }
            chat["baseRev"] = chat["rev"].clone();
            vicuna_lib::server_store::merge_json(db,&json!([chat]),vec![],&Default::default()).await?;
            Ok(format!("Session {id}: {action} applied."))
        }
        _ => Err(format!("Unknown action {action}.")),
    }
}

// ---------- window reattach ----------

/// A window (re)opening a session that has a live turn: the recorder's
/// transcript plus the live ids. Content events still queued are dropped
/// from the channel (the snapshot already holds them, replaying would double
/// the text); asks, session control and the terminal event stay queued.
pub fn attach(st: &BridgeState, session_id: &str) -> Value {
    let Some((tid, ch, started)) = st.session_turns.lock().ok().and_then(|m| m.get(session_id).cloned()) else {
        return Value::Null;
    };
    if st.agent.get_turn(&tid).is_none() {
        return Value::Null;
    }
    let rs = st.recorders.lock().ok().and_then(|m| m.get(&ch).cloned());
    let mut out = json!({ "turnId": tid, "channel": ch, "startedMs": started });
    if let Some(rs) = rs {
        let Ok(g) = rs.lock() else { return out };
        let snap = g.rec.snapshot();
        let has_ask = !snap["pendingAsk"].is_null();
        if let Ok(mut map) = st.events.lock() {
            if let Some(q) = map.get_mut(&ch) {
                q.retain(|e| {
                    let k = e.get("type").and_then(Value::as_str).unwrap_or("");
                    !persist::is_content_event(k) && !(has_ask && k == "askrequest")
                });
            }
        }
        out["snapshot"] = snap;
    }
    out
}

/// A window answered an ask card: mirror its Q/A record into the recorder.
pub fn record_answer(st: &BridgeState, turn: &Arc<TurnState>, turn_id: &str, ask_id: &str, answer: String, display: Option<&str>, dismissed: bool) -> Result<(), String> {
    let recs: Vec<Arc<Mutex<RecState>>> = st.recorders.lock().map(|m| m.values().cloned().collect()).unwrap_or_default();
    for rs in recs {
        if let Ok(mut g) = rs.lock() {
            if g.turn_id == turn_id {
                agent_answer(turn, ask_id, answer)?;
                g.rec.record_answer(ask_id, display, dismissed);
                return Ok(());
            }
        }
    }
    agent_answer(turn, ask_id, answer)
}

// ---------- scheduled sends ----------

const SCHED_KEY: &str = "scheduled";
const SCHED_DONE_KEY: &str = "scheduled_done";

async fn sched_load(db: &vicuna_lib::server_store::Pool) -> (Vec<Value>, Vec<String>) {
    let items = vicuna_lib::server_store::meta_get(db, SCHED_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default();
    let done = vicuna_lib::server_store::meta_get(db, SCHED_DONE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    (items, done)
}

async fn sched_save(db: &vicuna_lib::server_store::Pool, items: &[Value], done: &[String]) -> Result<(), String> {
    vicuna_lib::server_store::meta_put(db, SCHED_KEY, &Value::Array(items.to_vec()).to_string()).await?;
    let keep = &done[done.len().saturating_sub(500)..];
    vicuna_lib::server_store::meta_put(db, SCHED_DONE_KEY, &json!(keep).to_string()).await
}

fn item_id(v: &Value) -> &str {
    v.get("id").and_then(Value::as_str).unwrap_or("")
}

pub async fn schedule_list(st: &BridgeState) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    Ok(Value::Array(sched_load(db).await.0))
}

/// Upsert items by id. Ids already delivered or removed are ignored, so a
/// window with an outdated list can never re-add (and re-send) one.
pub async fn schedule_put(st: &BridgeState, items: Vec<Value>) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    let _g = st.sched_lock.lock().await;
    let (mut list, done) = sched_load(db).await;
    for it in items {
        let id = item_id(&it).to_string();
        if id.is_empty() || done.contains(&id) {
            continue;
        }
        match list.iter_mut().find(|x| item_id(x) == id) {
            Some(x) => *x = it,
            None => list.push(it),
        }
    }
    sched_save(db, &list, &done).await?;
    Ok(Value::Array(list))
}

pub async fn schedule_remove(st: &BridgeState, ids: Vec<String>) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    let _g = st.sched_lock.lock().await;
    let (mut list, mut done) = sched_load(db).await;
    list.retain(|x| !ids.iter().any(|i| i == item_id(x)));
    done.extend(ids);
    sched_save(db, &list, &done).await?;
    Ok(Value::Array(list))
}

/// At server start: anything already due was missed while the server was
/// down; it waits for the user instead of firing late (same as the app).
pub async fn schedule_mark_missed(st: &BridgeState) {
    let Some(db) = &st.db else { return };
    let _g = st.sched_lock.lock().await;
    let (mut list, done) = sched_load(db).await;
    let now = now_ms();
    let mut changed = false;
    for it in &mut list {
        if it.get("at").and_then(Value::as_i64).unwrap_or(i64::MAX) <= now && it.get("missed").and_then(Value::as_bool) != Some(true) {
            it["missed"] = json!(true);
            changed = true;
        }
    }
    if changed {
        let _ = sched_save(db, &list, &done).await;
    }
}

/// Send every due (not missed) scheduled message. Runs every few seconds.
pub async fn schedule_tick(st: &Arc<BridgeState>) {
    let Some(db) = &st.db else { return };
    let due: Vec<Value> = {
        let (list, _) = sched_load(db).await;
        let now = now_ms();
        list.into_iter()
            .filter(|it| it.get("missed").and_then(Value::as_bool) != Some(true))
            .filter(|it| it.get("at").and_then(Value::as_i64).is_some_and(|a| a <= now))
            .collect()
    };
    for it in due {
        let id = item_id(&it).to_string();
        match deliver_scheduled(st, &it).await {
            Ok(()) => {
                let _ = schedule_remove(st, vec![id]).await;
            }
            Err(e) => {
                let _g = st.sched_lock.lock().await;
                let (mut list, done) = sched_load(db).await;
                if let Some(x) = list.iter_mut().find(|x| item_id(x) == id) {
                    x["missed"] = json!(true);
                    x["error"] = json!(e);
                }
                let _ = sched_save(db, &list, &done).await;
            }
        }
    }
}

async fn deliver_scheduled(st: &Arc<BridgeState>, it: &Value) -> Result<(), String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    let text = it.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let mut msg = json!({ "role": "user", "content": text });
    if let Some(imgs) = it.get("images").filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        msg["images"] = imgs.clone();
    }
    if let Some(cid) = it.get("chatId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        if let Some(c) = vicuna_lib::server_store::load_chat(db, cid).await? {
            if c.get("mode").and_then(Value::as_str) == Some("work") {
                return deliver(st, cid, msg).await;
            }
        }
    }
    // the target is gone: a new session in the stored workspace
    let ws = it.get("workspace").and_then(Value::as_str).unwrap_or("").to_string();
    let ws = vicuna_lib::work_validate_root_logic(ws).await?;
    let src = if text.is_empty() { "Image".to_string() } else { text.clone() };
    let title = if src.chars().count() > 32 { format!("{}..", src.chars().take(32).collect::<String>()) } else { src };
    let id = uuid();
    let now = now_ms();
    vicuna_lib::server_store::create_chat(
        db,
        &json!({ "id": id, "title": title, "createdAt": now, "updatedAt": now, "mode": "work", "workspace": ws, "messages": [] }),
    )
    .await?;
    start_server_turn(st, &id, vec![msg], None).await.map(|_| ())
}

/// The agent's `schedule_wakeup`, resolved and stored on the server.
async fn handle_wakeup(st: &Arc<BridgeState>, turn: &Arc<TurnState>, chat_id: &str, ev: &Value) {
    let Some(req_id) = ev.get("req_id").and_then(Value::as_str) else { return };
    let now = now_ms();
    let offset = st.tz_offset_min.load(Ordering::Relaxed);
    let reply = match persist::resolve_wake_time(
        ev.get("at").and_then(Value::as_str),
        ev.get("in_minutes").and_then(Value::as_f64),
        now,
        offset,
    ) {
        Err(e) => json!({ "ok": false, "error": e }),
        Ok(at) => {
            let chat = match &st.db {
                Some(db) => vicuna_lib::server_store::load_chat(db, chat_id).await.ok().flatten(),
                None => None,
            };
            let s = |k: &str| chat.as_ref().and_then(|c| c.get(k)).and_then(Value::as_str).unwrap_or("").to_string();
            let item = json!({
                "id": uuid(),
                "chatId": chat_id,
                "title": s("title"),
                "workspace": s("workspace"),
                "text": persist::wake_message(ev.get("message").and_then(Value::as_str).unwrap_or("")),
                "at": at,
                "createdAt": now,
            });
            match schedule_put(st, vec![item]).await {
                Ok(_) => json!({ "ok": true, "when": persist::describe_when(at, now, offset), "server": true }),
                Err(e) => json!({ "ok": false, "error": e }),
            }
        }
    };
    let _ = agent_answer(turn, req_id, reply.to_string());
}

/// Background timer: scheduled sends, every 5 s.
pub fn spawn_scheduler(st: Arc<BridgeState>) {
    tokio::spawn(async move {
        schedule_mark_missed(&st).await;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            schedule_tick(&st).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn recording() -> (Arc<BridgeState>, Arc<Mutex<RecState>>, Arc<TurnState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("vicuna-recording-{}", uuid()));
        let db = vicuna_lib::server_store::open(&dir.join("test.db")).await.unwrap();
        let messages = vec![json!({"role":"user", "content":"go"}), persist::assistant_placeholder("opencode")];
        vicuna_lib::server_store::create_chat(&db, &json!({
            "id":"chat", "title":"Test", "createdAt":1, "mode":"work", "messages":messages,
        })).await.unwrap();
        let st = Arc::new(BridgeState { db: Some(db), ..Default::default() });
        let turn = Arc::new(TurnState::new());
        let tid = st.agent.next_turn(turn.clone());
        let rs = Arc::new(Mutex::new(RecState {
            rec: Recorder::new("chat", messages, 1, None, "opencode"),
            turn_id: tid.clone(), finishing: false, deferred: vec![], report_to: None, gone: false,
        }));
        st.recorders.lock().unwrap().insert("channel".into(), rs.clone());
        st.session_turns.lock().unwrap().insert("chat".into(), (tid, "channel".into(), 1));
        (st, rs, turn, dir)
    }

    #[tokio::test]
    async fn event_channel_records_without_polling_and_attach_does_not_replay_content() {
        let (st, rs, turn, dir) = recording().await;
        let ch = work_channel(st.clone(), "channel".into(), Some(rs.clone()), turn.clone(), Some("chat".into()));
        ch.send(WorkAgentEvent::Token { text: "Before tool.".into() }).unwrap();
        ch.send(WorkAgentEvent::Think { text: "Reasoning".into() }).unwrap();
        ch.send(WorkAgentEvent::ToolStart { name:"read".into(), args:json!({"path":"a.txt"}) }).unwrap();
        write_rec(&st, &rs, false).await;
        let snap = attach(&st, "chat");
        assert_eq!(snap["snapshot"]["pendingHead"]["name"], "read");
        assert!(crate::drain_events(&st.events, "channel").is_empty(), "snapshot covers queued content");
        ch.send(WorkAgentEvent::ToolEnd { name:"read".into(), output:"<file> & content".into() }).unwrap();
        ch.send(WorkAgentEvent::Token { text:"All done.".into() }).unwrap();
        ch.send(WorkAgentEvent::RoundEnd { gen_ms:50, gen_tps:Some(20.0), gen_cost:None }).unwrap();
        let expected = rs.lock().unwrap().rec.messages[1]["content"].clone();
        turn.done.store(true, Ordering::Relaxed);
        ch.send(WorkAgentEvent::Done { stopped:false }).unwrap();
        // The terminal event is released only after its transcript is committed.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let events = crate::drain_events(&st.events, "channel");
                if events.iter().any(|e| e["type"] == "done") { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let db = st.db.as_ref().unwrap();
        let c = vicuna_lib::server_store::load_chat(db, "chat").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], expected);
        assert_eq!(c["messages"][1]["thinking"], "Reasoning");
        assert_eq!(c["messages"][1]["genMs"], 50.0);
        db.close().await;
        let reopened = vicuna_lib::server_store::open(&dir.join("test.db")).await.unwrap();
        let c = vicuna_lib::server_store::load_chat(&reopened, "chat").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], expected);
        reopened.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn delayed_periodic_write_cannot_overwrite_final_transcript() {
        let (st, rs, _, dir) = recording().await;
        let guard = st.chat_lock.lock().await;
        rs.lock().unwrap().rec.apply(&json!({"type":"token", "text":"partial"}), "");
        let (s, r) = (st.clone(), rs.clone());
        let periodic = tokio::spawn(async move { write_rec(&s, &r, false).await });
        tokio::task::yield_now().await;
        {
            let mut g = rs.lock().unwrap();
            g.rec.apply(&json!({"type":"token", "text":" complete"}), "");
            g.finishing = true;
        }
        drop(guard);
        write_rec(&st, &rs, true).await;
        periodic.await.unwrap();
        let c = vicuna_lib::server_store::load_chat(st.db.as_ref().unwrap(), "chat").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], "partial complete");
        st.db.as_ref().unwrap().close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn session_management_works_without_a_window() {
        let (st, _, turn, dir) = recording().await;
        let listed = session_control(&st, "lead", "list", &json!({})).await.unwrap();
        assert!(listed.contains("chat"));
        let status = session_control(&st,"lead","status",&json!({"session_id":"chat"})).await.unwrap();
        assert!(status.contains("running"));
        session_control(&st,"lead","archive",&json!({"session_id":"chat"})).await.unwrap();
        let c = vicuna_lib::server_store::load_chat(st.db.as_ref().unwrap(),"chat").await.unwrap().unwrap();
        assert!(c["extra"].as_str().unwrap().contains("true"));
        session_control(&st,"lead","send",&json!({"session_id":"chat","message":"follow up"})).await.unwrap();
        assert_eq!(turn.queue.lock().unwrap().len(),1);
        assert_eq!(st.recorders.lock().unwrap()["channel"].lock().unwrap().report_to.as_deref(),Some("lead"));
        session_control(&st,"lead","stop",&json!({"session_id":"chat"})).await.unwrap();
        assert!(turn.stop.load(Ordering::Relaxed));
        assert!(session_control(&st,"chat","send",&json!({"session_id":"chat","message":"no"})).await.is_err());
        st.db.as_ref().unwrap().close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn server_schedule_survives_restart_and_removal_is_not_resurrected() {
        let (st, _, _, dir) = recording().await;
        let item = json!({"id":"wake", "chatId":"chat", "text":"check back", "at":now_ms()+60_000});
        schedule_put(&st, vec![item.clone()]).await.unwrap();
        assert_eq!(schedule_list(&st).await.unwrap()[0], item);
        st.db.as_ref().unwrap().close().await;
        let db = vicuna_lib::server_store::open(&dir.join("test.db")).await.unwrap();
        let restarted = BridgeState { db:Some(db), ..Default::default() };
        assert_eq!(schedule_list(&restarted).await.unwrap()[0], item);
        schedule_remove(&restarted, vec!["wake".into()]).await.unwrap();
        schedule_put(&restarted, vec![item]).await.unwrap();
        assert_eq!(schedule_list(&restarted).await.unwrap(), json!([]));
        restarted.db.as_ref().unwrap().close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
