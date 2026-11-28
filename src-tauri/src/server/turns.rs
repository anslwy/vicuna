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
use crate::ipc::{Channel, InvokeResponseBody};
use crate::work_agent::{agent_answer, agent_queue_msg, TurnState, WorkAgentEvent};
use crate::work_engine::Engine;
use crate::ChatMessage;

use crate::server::persist::{self, Recorder};
use crate::server::BridgeState;

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
    recs.iter()
        .filter_map(|r| {
            let g = r.lock().ok()?;
            // a recorder whose turn ended without a terminal event (finalize
            // never started) would otherwise refuse every save of that session
            // forever with SESSION_CHANGED; once finalize runs it is live
            // until it removes itself
            let ended = st.agent.get_turn(&g.turn_id).map_or(true, |t| t.done.load(Ordering::Relaxed));
            (g.finishing || !ended).then(|| g.rec.chat_id.clone())
        })
        .chain(super::chat::running_ids(st))
        .collect()
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
    pub fresh_session: bool,
    pub lead: bool,
    pub rec: Option<Recorder>,
    pub report_to: Option<String>,
    pub(crate) mcp_selection: crate::mcp::SessionSelection,
    pub(crate) skills_enabled: Option<crate::skills::EnabledMap>,
}

/// The live turn of a session, if one is still running.
pub fn live_turn(st: &BridgeState, session_id: &str) -> Option<(String, Arc<TurnState>)> {
    let (tid, _, _) = st.session_turns.lock().ok()?.get(session_id).cloned()?;
    let turn = st.agent.get_turn(&tid)?;
    (!turn.done.load(Ordering::Relaxed)).then_some((tid, turn))
}

/// The final reply text of a turn that just ended, for bridges mirroring it.
///
/// `turn.done` flips when the engine loop returns, which is independent of the
/// recorder applying the last channel events and of `finalize`'s forced write
/// (queued behind `chat_lock`). Reading the DB after a fixed sleep therefore
/// returned the last periodic write and dropped the closing text. Instead,
/// wait until the turn's recorder is gone (finalize removes it after its final
/// write) and use its in-memory transcript; the DB is only a fallback.
pub async fn final_reply(st: &BridgeState, session: &str, turn_id: &str) -> String {
    final_reply_and_images(st, session, turn_id).await.0
}

/// One outbound picture (vicuna-mcp show_image) for bridges.
#[derive(Debug, Clone, PartialEq)]
pub struct OutImage {
    pub data_url: String,
    pub caption: Option<String>,
    pub filename: Option<String>,
}

impl OutImage {
    /// `{dataUrl, caption, filename}` (Recorder::shown_images) or a bare
    /// data URL string (a stored message's `images`).
    pub fn from_value(v: &Value) -> Option<Self> {
        let (url, caption, filename) = match v {
            Value::String(s) => (s.clone(), None, None),
            _ => (
                v.get("dataUrl")?.as_str()?.to_string(),
                v.get("caption").and_then(Value::as_str).map(str::to_string),
                v.get("filename").and_then(Value::as_str).map(str::to_string),
            ),
        };
        url.starts_with("data:image/").then_some(Self { data_url: url, caption, filename })
    }

    /// (mime, decoded bytes)
    pub fn decode(&self) -> Option<(String, Vec<u8>)> {
        use base64::Engine as _;
        let (mime, b64) = crate::attachments::parse_data_url(&self.data_url)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).ok()?;
        Some((mime, bytes))
    }

    /// Filename with an extension matching the mime.
    pub fn file_name(&self, mime: &str, n: usize) -> String {
        if let Some(f) = self.filename.as_deref().map(crate::attachments::sanitize_name).filter(|f| !f.is_empty()) {
            return f;
        }
        let ext = mime.strip_prefix("image/").map(|e| if e == "jpeg" { "jpg" } else { e }).unwrap_or("png");
        format!("image-{n}.{ext}")
    }
}

/// Images not yet in `seen` (data URLs), marking them seen: each picture of
/// a turn is forwarded once, whether it was seen live or only at the end.
pub fn unseen_images(all: &[Value], seen: &mut std::collections::HashSet<String>) -> Vec<OutImage> {
    all.iter()
        .filter_map(OutImage::from_value)
        .filter(|i| seen.insert(i.data_url.clone()))
        .collect()
}

/// The live recorder's show_image list for `session` (empty when none).
pub fn live_images(st: &BridgeState, session: &str) -> Vec<Value> {
    let Ok(recs) = st.recorders.lock() else { return Vec::new() };
    let recs: Vec<_> = recs.values().cloned().collect();
    recs.into_iter()
        .find_map(|r| r.lock().ok().filter(|g| g.rec.chat_id == session).map(|g| g.rec.shown_images.clone()))
        .unwrap_or_default()
}

/// `final_reply` plus the turn's show_image pictures (from the recorder, or
/// the stored last reply's `images` when no recorder was held).
pub async fn final_reply_and_images(st: &BridgeState, session: &str, turn_id: &str) -> (String, Vec<Value>) {
    let find = || -> Option<Arc<Mutex<RecState>>> {
        let recs: Vec<_> = st.recorders.lock().ok()?.values().cloned().collect();
        recs.into_iter().find(|r| r.lock().map(|g| g.turn_id == turn_id).unwrap_or(false))
    };
    let mut held = find();
    let until = Instant::now() + Duration::from_secs(60);
    // a turn with no recorder at all (no db) has nothing to wait for
    let mut grace = Instant::now() + Duration::from_secs(3);
    loop {
        match find() {
            Some(r) => {
                held = Some(r);
                grace = until;
            }
            None if held.is_some() || Instant::now() >= grace => break,
            None => {}
        }
        if Instant::now() >= until {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut images = Vec::new();
    let had_rec = held.is_some();
    if let Some((reply, imgs)) = held.and_then(|r| r.lock().ok().map(|g| (g.rec.last_reply(), g.rec.shown_images.clone()))) {
        images = imgs;
        if !reply.is_empty() {
            return (reply, images);
        }
    }
    let reply = match &st.db {
        Some(db) => {
            let chat = crate::server_store::load_chat(db, session).await.ok().flatten();
            let msgs = chat.as_ref().and_then(|c| c["messages"].as_array().cloned()).unwrap_or_default();
            // no recorder: the stored last reply's pictures (the recorder's
            // own list is authoritative when there was one)
            if !had_rec {
                if let Some(m) = msgs.iter().rfind(|m| m["role"] == "assistant") {
                    images = m["images"].as_array().cloned().unwrap_or_default();
                }
            }
            persist::last_reply(&msgs).to_string()
        }
        None => String::new(),
    };
    (reply, images)
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
    let skills_enabled = match (&st.db, &p.session_id) {
        (Some(db), Some(id)) => crate::server_store::session_skills(db, id).await?,
        _ => None,
    }.or(p.skills_enabled).unwrap_or_else(crate::skills::defaults);
    let turn = Arc::new(TurnState::new().with_skills(skills_enabled));
    turn.lead.store(p.lead, Ordering::Relaxed);
    let turn_id = st.agent.next_turn(turn.clone());
    let resources = st.resource_dir.clone().or_else(crate::server::test_resource_dir);
    let mut mcp_snapshot = crate::acp::McpSnapshot::capture(&st.mcp, resources.as_deref(), &p.mcp_selection).await;
    let _ = turn.mcp.set(crate::vicuna_mcp::TurnMcp::new(
        st.mcp.clone(), resources.clone(), p.mcp_selection, Some(Arc::downgrade(st)),
    ));
    if let (Some(sid), Ok(mut m)) = (&p.session_id, st.session_turns.lock()) {
        m.insert(sid.clone(), (turn_id.clone(), p.uuid.clone(), now_ms() as u64));
        st.redo_history.clear(sid);
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
    let browser = st.browser.clone();
    let (model, workspace, messages, session_id, fresh, engine) =
        (p.model, p.workspace, p.messages, p.session_id, p.fresh_session, p.engine);
    tokio::spawn(async move {
        if mcp_snapshot.uses_builtin_browser() {
            let _ = crate::browser::browser_start_logic(resources, &browser).await;
            mcp_snapshot.update_browser_url();
        }
        crate::acp::run_turn(
            turn, model, workspace, messages, session_id, None, fresh, on_event, &acp, mcp_snapshot, engine,
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
        if kind == "schedulecontrol" && st.db.is_some() {
            if let Some(sid) = session_id.clone() {
                let (st, turn) = (st.clone(), turn.clone());
                tokio::spawn(async move {
                    let req = v.get("req_id").and_then(Value::as_str).unwrap_or("");
                    let action = v.get("action").and_then(Value::as_str).unwrap_or("");
                    let lead = v.get("lead").and_then(Value::as_bool).unwrap_or(false);
                    let reply = match schedule_control(&st, &sid, lead, action, &v["args"]).await {
                        Ok(text) => json!({"ok": true, "text": text}),
                        Err(error) => json!({"ok": false, "error": error}),
                    };
                    let _ = agent_answer(&turn, req, reply.to_string());
                });
                return Ok(());
            }
        }
        // Server-owned session management must keep worker chains moving
        // even with every window asleep. Deletion retains the user card.
        // (a delete aimed at another server is refused there, so it stays here too)
        let remote_ctl = v["args"].get("server").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
        if kind == "sessioncontrol" && st.db.is_some() && (remote_ctl || v.get("action").and_then(Value::as_str) != Some("delete")) {
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
            crate::server::push_event(&st.events, &uuid, &raw);
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
        crate::server::push_event(&st.events, &uuid, &raw);
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
    if let Err(e) = crate::server_store::write_messages(db, &id, &msgs, touched.then(now_ms)).await {
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
            let woken = turn.report_woken.load(Ordering::Relaxed)
                || leftovers.iter().any(|m| m["content"].as_str().is_some_and(persist::is_worker_done_message));
            match start_server_turn(&st, &chat_id, leftovers, None).await {
                Ok(_) if woken => mark_report_woken(&st, &chat_id),
                Ok(_) => {}
                Err(e) => eprintln!("vicuna: queued prompts for {chat_id} not sent: {e}"),
            }
        }
        if let Some(lead) = report_to {
            report_to_lead(&st, &chat_id, &lead, &reply, stopped).await;
        }
        let wakes = turn.mcp_wakes.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default();
        if !wakes.is_empty() && !stopped {
            let msg = json!({"role": "user", "content": persist::mcp_wake_message(&wakes)});
            if let Err(e) = deliver(&st, &chat_id, msg).await {
                eprintln!("vicuna: MCP wake for {chat_id} not sent: {e}");
            }
        }
    }
    if let Ok(mut g) = rs.lock() {
        for raw in std::mem::take(&mut g.deferred) {
            crate::server::push_event(&st.events, &uuid, &raw);
        }
    }
    if let Ok(mut m) = st.recorders.lock() {
        m.remove(&uuid);
    }
}

async fn report_to_lead(st: &Arc<BridgeState>, worker: &str, lead: &str, reply: &str, stopped: bool) {
    let Some(db) = &st.db else { return };
    let Ok(Some(w)) = crate::server_store::load_chat(db, worker).await else { return };
    let extra = parse_extra(&w);
    let engine = crate::work_engine::effective_engine(extra.engine.as_deref());
    let model = if extra.engine.is_some() { extra.work_model.clone() } else { engine.saved_model() };
    let title = w.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let msg = json!({
        "role": "user",
        "content": persist::worker_done_message(worker, &title, engine.id(), model.as_deref(), reply, stopped),
    });
    deliver_report(st, lead, msg).await;
}

/// Flag the live turn of `chat_id` as woken by a report callback (see
/// `TurnState::report_woken`).
pub fn mark_report_woken(st: &BridgeState, chat_id: &str) {
    if let Some((_, turn)) = live_turn(st, chat_id) {
        turn.report_woken.store(true, Ordering::Relaxed);
    }
}

/// Deliver a report callback (local `report_to` or a remote watch) into the
/// receiving session and mark the turn that takes it as report-woken.
pub async fn deliver_report(st: &Arc<BridgeState>, lead: &str, msg: Value) {
    // the lead may be mid-compaction in a window or briefly unavailable: retry
    for _ in 0..30 {
        match deliver(st, lead, msg.clone()).await {
            Ok(()) => {
                mark_report_woken(st, lead);
                return;
            }
            Err(e) if e.contains("no longer exists") || e.contains("not a Work session") => {
                eprintln!("vicuna: worker report for {lead} dropped: {e}");
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

#[derive(Default)]
pub struct Extra {
    pub engine: Option<String>,
    pub work_model: Option<String>,
    pub lead: bool,
    pub(crate) mcp: crate::mcp::SessionSelection,
}

pub fn parse_extra(chat: &Value) -> Extra {
    let o = crate::mcp::extra_object(chat.get("extra").and_then(Value::as_str));
    let s = |k: &str| o.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
    Extra {
        engine: s("engine"),
        work_model: s("workModel"),
        lead: o.get("role").and_then(Value::as_str) == Some("lead"),
        mcp: crate::mcp::SessionSelection::from_extra(&o),
    }
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

/// Deliver a message from a messaging bridge (Telegram, bridge plugins):
/// with the "send follow-ups now" setting on (default) a running turn is
/// interrupted so it goes right away; off, it just queues. Returns
/// `queued` = it is waiting for the running turn to finish.
pub async fn deliver_follow_up(st: &Arc<BridgeState>, chat_id: &str, msg: Value) -> Result<bool, String> {
    let running = live_turn(st, chat_id).is_some();
    deliver(st, chat_id, msg).await?;
    if !running || !crate::work_engine::send_now_default() {
        return Ok(running);
    }
    if let Some((_, turn)) = live_turn(st, chat_id) {
        if crate::work_agent::agent_send_now(&turn, None) {
            return Ok(false);
        }
    } else {
        return Ok(false); // the turn ended meanwhile and took the message
    }
    // engine without a round interrupt: stop + restart can take a while, so
    // don't hold up the caller (a bridge's update loop / RPC)
    let (st, id) = (st.clone(), chat_id.to_string());
    tokio::spawn(async move {
        if let Err(e) = send_now(&st, &id).await {
            eprintln!("vicuna: send-now for {id}: {e}");
        }
    });
    Ok(false)
}

/// Interrupt a session's running turn so its queued messages go now: the
/// queue is taken off the turn (a stopped turn would leave it to a window),
/// the turn is stopped, and once it has ended the messages start a new turn.
/// Returns how many messages were sent (0 = nothing queued / idle).
pub async fn send_now(st: &Arc<BridgeState>, chat_id: &str) -> Result<usize, String> {
    let Some((_, turn)) = live_turn(st, chat_id) else { return Ok(0) };
    // fast path: the engine ends just the current round and runs the queue
    // as its next prompt, in the same turn (no cancel grace, no respawn)
    let queued = turn.queue.lock().map(|q| q.len()).unwrap_or(0);
    if queued > 0 && crate::work_agent::agent_send_now(&turn, None) {
        return Ok(queued);
    }
    let msgs: Vec<Value> = turn.queue.lock().map(|mut q| q.drain(..).collect::<Vec<_>>()).unwrap_or_default()
        .into_iter()
        .filter_map(|(_, m)| serde_json::to_value(m).ok())
        .collect();
    if msgs.is_empty() {
        return Ok(0);
    }
    crate::work_agent::agent_stop(&turn);
    // engines can take ~20 s to honor a cancel
    let until = Instant::now() + Duration::from_secs(45);
    while live_turn(st, chat_id).is_some() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let n = msgs.len();
    match live_turn(st, chat_id) {
        // still winding down: put them back so they aren't lost
        Some((_, t)) => {
            for m in msgs {
                if let Some(cm) = chat_message(&m) {
                    agent_queue_msg(&t, uuid(), cm)?;
                }
            }
            Err("the running turn didn't stop in time; your message stays queued".into())
        }
        None => start_server_turn(st, chat_id, msgs, None).await.map(|_| n),
    }
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
    let chat = crate::server_store::load_chat(db, chat_id)
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
    let (engine, pick) = crate::work_engine::session_engine(extra.engine.as_deref(), extra.work_model.as_deref())?;
    if !engine.is_present() {
        return Err(engine.missing_message());
    }
    let mut messages: Vec<Value> = chat.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let reply_at = messages.iter().rposition(persist::is_assistant_reply);
    let previous_model = reply_at.and_then(|i| messages[i]["model"].as_str());
    let clear_at = messages.iter().rposition(|m| m["contextCleared"] == true);
    let fresh_session = clear_at.is_some_and(|i| reply_at.is_none_or(|r| i > r))
        || engine.changes_cli_account(previous_model);
    messages.extend(user_msgs);
    let mut ph = persist::assistant_placeholder(&persist::work_reply_model(None, pick.as_deref(), engine.id()));
    if !workspace.is_empty() {
        ph["workspace"] = json!(&workspace);
    }
    messages.push(ph);
    crate::server_store::write_messages(db, chat_id, &messages, Some(now_ms())).await?;
    let native: Vec<ChatMessage> =
        persist::compacted_history(&messages[..messages.len() - 1]).iter().filter_map(chat_message).collect();
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
            fresh_session,
            lead: extra.lead,
            rec: Some(rec),
            report_to,
            mcp_selection: extra.mcp,
            skills_enabled: None,
        },
    )
    .await
}

/// Non-destructive lead actions run against the server database. User
/// confirmation for delete remains with the connected window.
async fn session_control(st: &Arc<BridgeState>, lead_id: &str, action: &str, args: &Value) -> Result<String, String> {
    if action == "servers" || args.get("server").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()) {
        return super::remote_lead::control(st, lead_id, action, args).await;
    }
    let db = st.db.as_ref().ok_or("no server db")?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let report = args.get("report").and_then(Value::as_bool) != Some(false);
    if action == "create" {
        let lead = crate::server_store::load_chat(db, lead_id).await?.ok_or("lead session no longer exists")?;
        let requested = arg("engine");
        let engine = if requested.is_empty() { crate::work_engine::current_engine() }
            else { Engine::parse(&requested).ok_or("unknown engine")? };
        if !engine.is_present() { return Err(engine.missing_message()); }
        let ws = if arg("workspace").is_empty() { lead["workspace"].as_str().unwrap_or("").to_string() } else { arg("workspace") };
        let ws = crate::work_validate_root_logic(ws).await?;
        let msg = arg("message");
        let title: String = if !arg("title").is_empty() { arg("title") } else if !msg.is_empty() { msg.clone() } else { "Worker".into() }.chars().take(40).collect();
        let id = uuid();
        let model = arg("model");
        let extra = json!({"engine":engine.id(), "workModel": if model.is_empty() {Value::Null} else {json!(model)}, "leadId":lead_id});
        crate::server_store::create_chat(db, &json!({"id":id,"title":title,"createdAt":now_ms(),"updatedAt":now_ms(),"mode":"work","workspace":ws,"extra":extra.to_string(),"messages":[]})).await?;
        if !msg.is_empty() {
            start_server_turn(st, &id, vec![json!({"role":"user","content":msg})], report.then(|| lead_id.to_string())).await?;
        }
        return Ok(format!("Created \"{title}\" (session_id={id}, engine={}, workspace={ws}).{}", engine.id(), if !msg.is_empty() {" Its turn has started; its reply will reach the lead when it ends if reporting is enabled."} else {""}));
    }
    if action == "list" {
        let chats = crate::server_store::load_chats(db).await?;
        let rows: Vec<String> = chats.as_array().into_iter().flatten().filter(|c| c["mode"] == "work" && extra_obj(c)["archived"] != true).take(40).map(|c| {
            let id = c["id"].as_str().unwrap_or("");
            format!("- {id} · \"{}\" · {} · {}", c["title"].as_str().unwrap_or(""), if live_turn(st,id).is_some() {"running"} else {"idle"}, c["workspace"].as_str().unwrap_or(""))
        }).collect();
        return Ok(format!("Work sessions:\n{}", rows.join("\n")));
    }
    let id = if action == "set_workspace" && arg("session_id").is_empty() { lead_id.to_string() } else { arg("session_id") };
    if id.is_empty() { return Err("`session_id` is required (see list).".into()); }
    if id == lead_id && action != "set_workspace" { return Err("That is your own session.".into()); }
    let mut chat = crate::server_store::load_chat(db, &id).await?.ok_or_else(|| format!("No session {id}."))?;
    match action {
        "status" => {
            let reply = chat["messages"].as_array().map(|messages| persist::last_reply(messages)).unwrap_or("");
            Ok(format!("{id}: {}\nLast reply:\n{}", if live_turn(st,&id).is_some() {"running"} else {"idle"}, persist::tail_text(reply,3000)))
        }
        "permissions" => {
            let _write = st.chat_lock.lock().await;
            let caller = crate::server_store::load_chat(db, lead_id).await?.ok_or("calling session no longer exists")?;
            if extra_obj(&caller)["role"].as_str() != Some("lead") {
                return Err("Only the lead session can view or change other sessions' permissions.".into());
            }
            let mut chat = crate::server_store::load_chat(db, &id).await?.ok_or("session no longer exists")?;
            let mut extra = extra_obj(&chat);
            let mut permissions = extra.get("permissions").filter(|v| v.is_object()).cloned().unwrap_or(json!({}));
            let mut changed = false;
            if let Some(value) = args.get("read").and_then(Value::as_bool) {
                permissions["read"] = json!(value);
                changed = true;
            }
            if let Some(value) = args.get("allow_message").and_then(Value::as_bool) {
                permissions["message"] = json!(value);
                changed = true;
            }
            if changed {
                extra["permissions"] = permissions.clone();
                chat["extra"] = json!(extra.to_string());
                crate::server_store::write_title_and_extra(db, &id, &chat).await?;
            }
            let read = permissions.get("read").and_then(Value::as_bool).unwrap_or(true);
            let allow_message = permissions.get("message").and_then(Value::as_bool)
                .unwrap_or(extra["role"].as_str() == Some("lead"));
            Ok(json!({"session_id":id,"read":read,"allow_message":allow_message}).to_string())
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
            if let Some((_,turn)) = live_turn(st,&id) { crate::work_agent::agent_stop(&turn); }
            Ok(format!("Stopped {id}."))
        }
        "archive" | "unarchive" | "set_workspace" => {
            let _write = st.chat_lock.lock().await;
            chat = crate::server_store::load_chat(db, &id).await?.ok_or("session no longer exists")?;
            if action == "set_workspace" {
                if id != lead_id && live_turn(st,&id).is_some() { return Err("Session is running; stop it or wait first.".into()); }
                chat["workspace"] = json!(crate::work_validate_root_logic(arg("workspace")).await?);
                chat["mode"] = json!("work");
            } else {
                let mut extra: Value = chat["extra"].as_str().and_then(|s|serde_json::from_str(s).ok()).unwrap_or(json!({}));
                if action == "archive" && extra["role"] == "lead" { return Err("Refusing to archive a lead session.".into()); }
                extra["archived"] = json!(action == "archive");
                chat["extra"] = json!(extra.to_string());
            }
            chat["baseRev"] = chat["rev"].clone();
            super::actions::as_save_row(&mut chat);
            crate::server_store::merge_json(db,&json!([chat]),vec![],&Default::default()).await?;
            Ok(format!("Session {id}: {action} applied."))
        }
        _ => Err(format!("Unknown action {action}.")),
    }
}

// ---------- `vicuna session …` (CLI) ----------

/// A session id or unique prefix of one.
async fn resolve_session(db: &crate::server_store::Pool, id: &str) -> Result<Value, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("session id required".into());
    }
    if let Some(c) = crate::server_store::load_chat(db, id).await? {
        return Ok(c);
    }
    let chats = crate::server_store::load_chats(db).await?;
    let hits: Vec<&str> = chats.as_array().into_iter().flatten().filter_map(|c| c["id"].as_str()).filter(|c| c.starts_with(id)).collect();
    match hits.as_slice() {
        [one] => crate::server_store::load_chat(db, one).await?.ok_or_else(|| format!("no session {id}")),
        [] => Err(format!("no session matches {id}")),
        many => Err(format!("{id} is ambiguous: {} sessions match ({}…)", many.len(), many.iter().take(3).map(|s| &s[..8.min(s.len())]).collect::<Vec<_>>().join(", "))),
    }
}

fn extra_obj(chat: &Value) -> Value {
    chat.get("extra").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).filter(Value::is_object).unwrap_or(json!({}))
}

fn session_row(st: &BridgeState, c: &Value) -> Value {
    let id = c["id"].as_str().unwrap_or("");
    let x = extra_obj(c);
    let live = st.session_turns.lock().ok().and_then(|m| m.get(id).cloned()).filter(|_| live_turn(st, id).is_some());
    json!({
        "id": id,
        "title": c["title"],
        "workspace": c["workspace"],
        "mode": c["mode"],
        "engine": x.get("engine").cloned().unwrap_or(Value::Null),
        "model": x.get("workModel").cloned().unwrap_or(Value::Null),
        "mcp": x.get("mcp").cloned().unwrap_or(Value::Null),
        "archived": x.get("archived").and_then(Value::as_bool).unwrap_or(false),
        "lead": x.get("role").and_then(Value::as_str) == Some("lead"),
        "updatedAt": c["updatedAt"],
        "createdAt": c["createdAt"],
        "running": live.is_some() || super::chat::is_running(st, id),
        "turnId": live.as_ref().map(|l| json!(l.0)).unwrap_or(Value::Null),
        "channel": live.as_ref().map(|l| json!(l.1)).unwrap_or(Value::Null),
        "startedMs": live.as_ref().map(|l| json!(l.2)).unwrap_or(Value::Null),
    })
}

fn live_ids(st: &BridgeState, id: &str) -> Value {
    match st.session_turns.lock().ok().and_then(|m| m.get(id).cloned()) {
        Some((tid, ch, _)) => json!({ "turnId": tid, "channel": ch }),
        None => Value::Null,
    }
}

fn mcp_list(args: &Value) -> Option<Value> {
    let a = args.get("mcp")?;
    if a.is_null() {
        return None;
    }
    let names: Vec<String> = a.as_array()?.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    Some(json!(names))
}

async fn check_engine_model(engine: &str) -> Result<Engine, String> {
    let e = Engine::parse(engine).ok_or_else(|| format!("unknown harness {engine} (claude, codex, opencode, pi, fx, agy)"))?;
    if !e.is_present() {
        return Err(e.missing_message());
    }
    Ok(e)
}

/// Session control for the `vicuna` CLI (no window, no lead): list, get,
/// create, send, stop, update, archive/unarchive, delete. Turns run through
/// the same server-owned path as scheduled sends and worker sessions.
/// `get` with `omit_images`: swap each message's base64 `images` for
/// `images_meta` and list the session's staged `uploads` (metadata only).
pub(crate) fn omit_images(row: &mut Value, id: &str) {
    if let Some(msgs) = row["messages"].as_array_mut() {
        for m in msgs {
            if let Some(imgs) = m.as_object_mut().and_then(|o| o.remove("images")) {
                let meta = crate::attachments::images_meta(&imgs);
                if !meta.is_empty() {
                    m["images_meta"] = json!(meta);
                }
            }
        }
    }
    let uploads = crate::attachments::uploads_meta(id);
    if !uploads.is_empty() {
        row["uploads"] = json!(uploads);
    }
}

pub async fn cli_session(st: &Arc<BridgeState>, action: &str, args: &Value) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("this bridge has no session database; run `vicuna serve`")?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    match action {
        "list" => {
            let chats = crate::server_store::load_chats(db).await?;
            let rows: Vec<Value> = chats.as_array().into_iter().flatten().map(|c| session_row(st, c)).collect();
            return Ok(Value::Array(rows));
        }
        "create" => {
            let engine = if arg("engine").is_empty() { crate::work_engine::current_engine() } else { check_engine_model(&arg("engine")).await? };
            if !engine.is_present() {
                return Err(engine.missing_message());
            }
            let ws = crate::work_validate_root_logic(arg("workspace")).await?;
            let prompt = arg("prompt");
            let src = if !arg("title").is_empty() { arg("title") } else if !prompt.is_empty() { prompt.clone() } else { "New session".into() };
            let title = if src.chars().count() > 40 { format!("{}..", src.chars().take(38).collect::<String>()) } else { src };
            let mut extra = json!({ "engine": engine.id() });
            if !arg("model").is_empty() {
                extra["workModel"] = json!(arg("model"));
            }
            if let Some(m) = mcp_list(args) {
                extra["mcp"] = m;
            }
            let id = uuid();
            let now = now_ms();
            crate::server_store::create_chat(
                db,
                &json!({ "id": id, "title": title, "createdAt": now, "updatedAt": now, "mode": "work", "workspace": ws, "extra": extra.to_string(), "messages": [] }),
            )
            .await?;
            if !prompt.is_empty() {
                start_server_turn(st, &id, vec![json!({ "role": "user", "content": prompt })], None).await?;
            }
            let c = crate::server_store::load_chat(db, &id).await?.ok_or("session vanished")?;
            return Ok(session_row(st, &c));
        }
        _ => {}
    }
    let chat = resolve_session(db, &arg("id")).await?;
    let id = chat["id"].as_str().unwrap_or("").to_string();
    match action {
        "get" => {
            let mut row = session_row(st, &chat);
            row["messages"] = chat["messages"].clone();
            if args.get("omit_images").and_then(Value::as_bool) == Some(true) {
                omit_images(&mut row, &id);
            }
            row["goal"] = chat.get("goal").cloned().unwrap_or(Value::Null);
            Ok(row)
        }
        "send" => {
            let text = arg("text");
            if text.is_empty() {
                return Err("message text required".into());
            }
            if chat["mode"].as_str() != Some("work") {
                return Err(format!("session {id} is a Chat-mode session; the CLI drives Work sessions only"));
            }
            let msg = json!({ "role": "user", "content": text });
            if let Some((_, turn)) = live_turn(st, &id) {
                agent_queue_msg(&turn, uuid(), chat_message(&msg).ok_or("bad message")?)?;
                let mut out = live_ids(st, &id);
                out["queued"] = json!(true);
                out["id"] = json!(id);
                return Ok(out);
            }
            start_server_turn(st, &id, vec![msg], None).await?;
            let mut out = live_ids(st, &id);
            out["queued"] = json!(false);
            out["id"] = json!(id);
            Ok(out)
        }
        "stop" => {
            let running = live_turn(st, &id);
            if let Some((_, turn)) = &running {
                crate::work_agent::agent_stop(turn);
            }
            let stopped = super::chat::stop(st, &id, None) || running.is_some();
            Ok(json!({ "id": id, "stopped": stopped }))
        }
        "delete" => {
            if let Some((_, turn)) = live_turn(st, &id) {
                crate::work_agent::agent_stop(&turn);
                // let the turn unwind so its recorder stops writing first
                for _ in 0..100 {
                    if live_turn(st, &id).is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            let _write = st.chat_lock.lock().await;
            super::chat::stop(st, &id, None);
            crate::server_store::merge_json(db, &json!([]), vec![id.clone()], &Default::default()).await?;
            st.redo_history.clear(&id);
            schedule_drop_for(st, &id).await;
            super::telegram::drop_for(&id);
            super::bridge_plugins::drop_for(&id);
            Ok(json!({ "id": id, "deleted": true }))
        }
        "update" | "archive" | "unarchive" => {
            let _write = st.chat_lock.lock().await;
            let mut chat = crate::server_store::load_chat(db, &id).await?.ok_or("session no longer exists")?;
            let mut extra = extra_obj(&chat);
            if action != "update" {
                if action == "archive" && extra["role"] == "lead" {
                    return Err("refusing to archive a lead session".into());
                }
                extra["archived"] = json!(action == "archive");
            } else {
                if !arg("engine").is_empty() {
                    extra["engine"] = json!(check_engine_model(&arg("engine")).await?.id());
                    if arg("model").is_empty() {
                        // a model id belongs to its harness
                        extra["workModel"] = Value::Null;
                    }
                }
                if !arg("model").is_empty() {
                    extra["workModel"] = json!(if arg("model") == "default" { "".to_string() } else { arg("model") });
                }
                if let Some(m) = args.get("mcp") {
                    extra["mcp"] = if m.is_null() { Value::Null } else { mcp_list(args).unwrap_or(json!([])) };
                }
                if !arg("title").is_empty() {
                    chat["title"] = json!(arg("title"));
                }
                if !arg("workspace").is_empty() {
                    if live_turn(st, &id).is_some() {
                        return Err("session is running; stop it or wait before moving it".into());
                    }
                    chat["workspace"] = json!(crate::work_validate_root_logic(arg("workspace")).await?);
                    chat["mode"] = json!("work");
                }
            }
            chat["extra"] = json!(extra.to_string());
            chat["baseRev"] = chat["rev"].clone();
            super::actions::as_save_row(&mut chat);
            let out = crate::server_store::merge_json(db, &json!([chat]), vec![], &live_sessions(st)).await?;
            if !out.conflicts.is_empty() {
                return Err("the session is being written by a running turn; try again when it settles".into());
            }
            let c = crate::server_store::load_chat(db, &id).await?.ok_or("session vanished")?;
            Ok(session_row(st, &c))
        }
        other => Err(format!("unknown session action {other}")),
    }
}

/// Scheduled sends aimed at a deleted session go with it.
async fn schedule_drop_for(st: &BridgeState, chat_id: &str) {
    let Ok(Value::Array(list)) = schedule_list(st).await else { return };
    let ids: Vec<String> = list.iter().filter(|it| it.get("chatId").and_then(Value::as_str) == Some(chat_id)).map(|it| item_id(it).to_string()).collect();
    if !ids.is_empty() {
        let _ = schedule_remove(st, ids).await;
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
        // A reader queue of its own: the channel's queue may still be
        // followed by the client that started the turn, and sharing it split
        // every event between the two (both saw a shredded reply). Seeded
        // with what is still queued minus content (the snapshot holds that;
        // replaying would double the text). Opened under the recorder lock,
        // so no event lands between the snapshot and the reader.
        let seed: Vec<Value> = st
            .events
            .lock()
            .ok()
            .and_then(|m| m.get(&ch).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                let k = e.get("type").and_then(Value::as_str).unwrap_or("");
                !persist::is_content_event(k) && !(has_ask && k == "askrequest")
            })
            .collect();
        let reader = crate::server::open_reader(&st.events, &ch, seed);
        if let Ok(mut lp) = st.last_poll.lock() {
            lp.insert(reader.clone(), std::time::Instant::now());
        }
        out["channel"] = json!(reader);
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

async fn sched_load(db: &crate::server_store::Pool) -> (Vec<Value>, Vec<String>) {
    let items = crate::server_store::meta_get(db, SCHED_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default();
    let done = crate::server_store::meta_get(db, SCHED_DONE_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    (items, done)
}

async fn sched_save(db: &crate::server_store::Pool, items: &[Value], done: &[String]) -> Result<(), String> {
    crate::server_store::meta_put(db, SCHED_KEY, &Value::Array(items.to_vec()).to_string()).await?;
    let keep = &done[done.len().saturating_sub(500)..];
    crate::server_store::meta_put(db, SCHED_DONE_KEY, &json!(keep).to_string()).await
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
        // claim it first: an item cancelled (or moved) since the scan above
        // must never fire, and a claimed id can't be re-added by a window
        let Some(it) = schedule_claim(st, &id).await else { continue };
        match deliver_scheduled(st, &it).await {
            Ok(()) => {}
            Err(e) => {
                // put it back, flagged, so the user sees why it didn't go
                let _g = st.sched_lock.lock().await;
                let (mut list, mut done) = sched_load(db).await;
                done.retain(|d| d != &id);
                let mut x = it.clone();
                x["missed"] = json!(true);
                x["error"] = json!(e);
                list.push(x);
                let _ = sched_save(db, &list, &done).await;
            }
        }
    }
}

/// Take a due item off the list (and mark it done) iff it is still there and
/// still due; None when it was cancelled, rescheduled or already taken.
async fn schedule_claim(st: &BridgeState, id: &str) -> Option<Value> {
    let db = st.db.as_ref()?;
    let _g = st.sched_lock.lock().await;
    let (mut list, mut done) = sched_load(db).await;
    let pos = list.iter().position(|x| item_id(x) == id)?;
    let it = &list[pos];
    if it.get("missed").and_then(Value::as_bool) == Some(true) || !it.get("at").and_then(Value::as_i64).is_some_and(|a| a <= now_ms()) {
        return None;
    }
    let it = list.remove(pos);
    done.push(id.to_string());
    sched_save(db, &list, &done).await.ok()?;
    Some(it)
}

const WAKE_PREFIX: &str = "Scheduled wake-up (you asked Vicuna to wake you now): ";

fn sched_kind(it: &Value) -> &'static str {
    match it.get("kind").and_then(Value::as_str) {
        Some("wakeup") => "wakeup",
        Some(_) => "send",
        None if it.get("text").and_then(Value::as_str).is_some_and(|t| t.starts_with(WAKE_PREFIX)) => "wakeup",
        None => "send",
    }
}

fn sched_preview(it: &Value) -> String {
    let t = it.get("text").and_then(Value::as_str).unwrap_or("");
    let t = t.strip_prefix(WAKE_PREFIX).unwrap_or(t).replace('\n', " ");
    if t.chars().count() > 120 { format!("{}…", t.chars().take(120).collect::<String>()) } else { t }
}

fn sched_owner(it: &Value) -> &str {
    it.get("chatId").and_then(Value::as_str).unwrap_or("")
}

/// vicuna-mcp `manage_schedule`. Any session manages items aimed at itself;
/// only a lead (`lead`, from the calling turn) touches other sessions' items
/// or new-session items (no `chatId`).
pub async fn schedule_control(st: &BridgeState, caller: &str, lead: bool, action: &str, args: &Value) -> Result<String, String> {
    let db = st.db.as_ref().ok_or("no server db")?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let lead_only = "only the user's lead session can manage other sessions' scheduled items";
    let may = |owner: &str| lead || owner == caller;
    let now = now_ms();
    let offset = st.tz_offset_min.load(Ordering::Relaxed);
    let resolve = || -> Result<Option<i64>, String> {
        let at = args.get("at").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
        let mins = args.get("in_minutes").and_then(Value::as_f64);
        if at.is_none() && mins.is_none() {
            return Ok(None);
        }
        if at.is_some() && mins.is_some() {
            return Err("give either `in_minutes` or `at`, not both".into());
        }
        if let Some(m) = mins {
            if !(m.is_finite() && (1.0..=43200.0).contains(&m)) {
                return Err("`in_minutes` must be between 1 and 43200 (30 days)".into());
            }
        }
        let t = persist::resolve_wake_time(at, mins, now, offset)?;
        if t <= now {
            return Err("that time is in the past".into());
        }
        Ok(Some(t))
    };
    let find = |list: &[Value], id: &str| -> Result<Value, String> {
        if id.is_empty() {
            return Err("give the item's `id` (see `list`)".into());
        }
        let hits: Vec<&Value> = list.iter().filter(|x| item_id(x).starts_with(id)).collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!("no scheduled item {id} (it may have been sent or cancelled already)")),
            _ => Err(format!("id {id} is ambiguous; give more characters")),
        }
    };
    match action {
        "list" => {
            let (list, _) = sched_load(db).await;
            let all = args.get("all").and_then(Value::as_bool) == Some(true);
            let target = arg("session_id");
            let target = if target.is_empty() { caller.to_string() } else { target };
            if (all || target != caller) && !lead {
                return Err(lead_only.into());
            }
            let include_missed = args.get("include_past").and_then(Value::as_bool) == Some(true);
            let mut rows: Vec<&Value> = list
                .iter()
                .filter(|it| all || sched_owner(it) == target)
                .filter(|it| include_missed || it.get("missed").and_then(Value::as_bool) != Some(true))
                .collect();
            rows.sort_by_key(|it| it.get("at").and_then(Value::as_i64).unwrap_or(0));
            if rows.is_empty() {
                return Ok(if all { "No scheduled items.".into() } else { format!("No scheduled items for session {target}.") });
            }
            let mut out = format!("{} scheduled item(s):", rows.len());
            for it in rows {
                let at = it.get("at").and_then(Value::as_i64).unwrap_or(0);
                let owner = sched_owner(it);
                let title = it.get("title").and_then(Value::as_str).unwrap_or("");
                let by = it.get("createdBy").and_then(Value::as_str).unwrap_or(if sched_kind(it) == "wakeup" { owner } else { "user" });
                let by = if by == caller { "you" } else { by };
                let tgt = if owner.is_empty() { "a new session".to_string() } else { format!("{owner} \"{title}\"") };
                let state = match (it.get("missed").and_then(Value::as_bool), it.get("error").and_then(Value::as_str)) {
                    (Some(true), Some(e)) => format!(" [not sent: {e}]"),
                    (Some(true), None) => " [missed]".into(),
                    _ => String::new(),
                };
                out.push_str(&format!(
                    "\n- id {} · {} · {} · to {} · created by {}{}\n  {}",
                    item_id(it), sched_kind(it), persist::describe_when(at, now, offset), tgt, by, state, sched_preview(it)
                ));
            }
            Ok(out)
        }
        "cancel" => {
            let (list, _) = sched_load(db).await;
            let it = find(&list, &arg("id"))?;
            if !may(sched_owner(&it)) {
                return Err(lead_only.into());
            }
            schedule_remove(st, vec![item_id(&it).to_string()]).await?;
            Ok(format!("Cancelled scheduled item {} ({}). It will not be sent.", item_id(&it), sched_preview(&it)))
        }
        "update" => {
            let (list, _) = sched_load(db).await;
            let mut it = find(&list, &arg("id"))?;
            if !may(sched_owner(&it)) {
                return Err(lead_only.into());
            }
            let at = resolve()?;
            let msg = arg("message");
            if at.is_none() && msg.is_empty() {
                return Err("give a new time (`in_minutes` or `at`) and/or a new `message`".into());
            }
            if let Some(t) = at {
                it["at"] = json!(t);
                if let Some(o) = it.as_object_mut() {
                    o.remove("missed");
                    o.remove("error");
                }
            }
            if !msg.is_empty() {
                it["text"] = json!(if sched_kind(&it) == "wakeup" { persist::wake_message(&msg) } else { msg });
            }
            let id = item_id(&it).to_string();
            let saved = schedule_put(st, vec![it.clone()]).await?;
            if !saved.as_array().is_some_and(|l| l.iter().any(|x| item_id(x) == id)) {
                return Err("it was sent or cancelled meanwhile".into());
            }
            let when = it.get("at").and_then(Value::as_i64).unwrap_or(0);
            Ok(format!("Updated scheduled item {id}: {} · {}", persist::describe_when(when, now, offset), sched_preview(&it)))
        }
        "create" => {
            let msg = arg("message");
            if msg.is_empty() {
                return Err("`message` is required".into());
            }
            let at = resolve()?.ok_or("give `in_minutes` (relative) or `at` (local time)")?;
            let target = arg("session_id");
            let target = if target.is_empty() { caller.to_string() } else { target };
            if !may(&target) {
                return Err(lead_only.into());
            }
            let chat = crate::server_store::load_chat(db, &target).await?.ok_or(format!("no session {target}"))?;
            if chat.get("mode").and_then(Value::as_str) != Some("work") {
                return Err("scheduled items can only target Work sessions".into());
            }
            let s = |k: &str| chat.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let id = uuid();
            let item = json!({
                "id": id, "chatId": target, "title": s("title"), "workspace": s("workspace"),
                "text": persist::wake_message(&msg), "at": at, "createdAt": now,
                "kind": "wakeup", "createdBy": caller,
            });
            schedule_put(st, vec![item]).await?;
            Ok(format!("Scheduled item {id} for session {target}: {}. The server delivers it even with no app window open.", persist::describe_when(at, now, offset)))
        }
        other => Err(format!("unknown action {other}; use list, create, update or cancel")),
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
        if let Some(c) = crate::server_store::load_chat(db, cid).await? {
            if c.get("mode").and_then(Value::as_str) == Some("work") {
                return deliver(st, cid, msg).await;
            }
        }
    }
    // the target is gone: a new session in the stored workspace
    let ws = it.get("workspace").and_then(Value::as_str).unwrap_or("").to_string();
    let ws = crate::work_validate_root_logic(ws).await?;
    let src = if text.is_empty() { "Image".to_string() } else { text.clone() };
    let title = if src.chars().count() > 32 { format!("{}..", src.chars().take(32).collect::<String>()) } else { src };
    let id = uuid();
    let now = now_ms();
    crate::server_store::create_chat(
        db,
        &json!({ "id": id, "title": title, "createdAt": now, "updatedAt": now, "mode": "work", "workspace": ws, "extra": sched_extra(it), "messages": [] }),
    )
    .await?;
    start_server_turn(st, &id, vec![msg], None).await.map(|_| ())
}

/// A scheduled new-session send may name its harness/model/MCP pick
/// (`vicuna schedule add --new -e … -m … --mcp …`); none = server defaults.
fn sched_extra(it: &Value) -> Value {
    let mut x = json!({});
    for (from, to) in [("engine", "engine"), ("model", "workModel")] {
        if let Some(v) = it.get(from).and_then(Value::as_str).filter(|v| !v.is_empty()) {
            x[to] = json!(v);
        }
    }
    if let Some(m) = it.get("mcp").filter(|m| m.is_array()) {
        x["mcp"] = m.clone();
    }
    if x.as_object().is_some_and(|o| o.is_empty()) { Value::Null } else { json!(x.to_string()) }
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
                Some(db) => crate::server_store::load_chat(db, chat_id).await.ok().flatten(),
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
                "kind": "wakeup",
                "createdBy": chat_id,
            });
            let id = item["id"].clone();
            match schedule_put(st, vec![item]).await {
                Ok(_) => json!({ "ok": true, "id": id, "when": persist::describe_when(at, now, offset), "server": true }),
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

    #[tokio::test]
    async fn assistant_images_record_persist_and_dedupe() {
        let st = sched_state().await;
        let db = st.db.clone().unwrap();
        let msgs = vec![json!({"role": "user", "content": "draw"}), json!({"role": "assistant", "content": "", "thinking": ""})];
        let mut rec = Recorder::new("me", msgs, 1, None, "claude");
        let png = "data:image/png;base64,iVBORw0KGgo=";
        let ev = json!({"type": "assistantimage", "data_url": png, "caption": "cat", "filename": "cat.png"});
        rec.apply(&ev, &ev.to_string());
        let bad = json!({"type": "assistantimage", "data_url": "javascript:x"});
        rec.apply(&bad, &bad.to_string());
        assert!(rec.dirty);
        assert_eq!(rec.messages[1]["images"], json!([png]));
        assert_eq!(rec.shown_images.len(), 1);
        let mut chat = crate::server_store::load_chat(&db, "me").await.unwrap().unwrap();
        chat["messages"] = json!(rec.messages);
        crate::server_store::merge_json(&db, &json!([chat]), vec![], &Default::default()).await.unwrap();
        let back = crate::server_store::load_chat(&db, "me").await.unwrap().unwrap();
        assert_eq!(back["messages"][1]["images"], json!([png]));
        // no recorder: the stored reply's pictures come back
        let (_, imgs) = final_reply_and_images(&st, "me", "turn-x").await;
        assert_eq!(imgs, vec![json!(png)]);
        // once per turn, live list or final list alike
        let mut seen = HashSet::new();
        let first = unseen_images(&rec.shown_images, &mut seen);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].caption.as_deref(), Some("cat"));
        assert!(unseen_images(&imgs, &mut seen).is_empty());
        let (mime, bytes) = first[0].decode().unwrap();
        assert_eq!((mime.as_str(), &bytes[..4]), ("image/png", &b"\x89PNG"[..]));
        assert_eq!(first[0].file_name(&mime, 1), "cat.png");
        assert_eq!(OutImage::from_value(&json!("data:image/jpeg;base64,/9j/")).unwrap().file_name("image/jpeg", 2), "image-2.jpg");
    }

    async fn sched_state() -> Arc<BridgeState> {
        let dir = std::env::temp_dir().join(format!("vicuna-sched-{}", uuid()));
        let db = crate::server_store::open(&dir.join("test.db")).await.unwrap();
        for id in ["me", "other"] {
            crate::server_store::create_chat(&db, &json!({"id": id, "title": id, "createdAt": 1, "mode": "work", "workspace": "/tmp", "messages": []})).await.unwrap();
        }
        Arc::new(BridgeState { db: Some(db), ..Default::default() })
    }

    fn new_id(text: &str) -> String {
        text.split("item ").nth(1).unwrap().split(' ').next().unwrap().to_string()
    }

    #[tokio::test]
    async fn manage_schedule_list_update_cancel_own_item_without_a_window() {
        let st = sched_state().await;
        let made = schedule_control(&st, "me", false, "create", &json!({"message": "upgrade", "in_minutes": 30})).await.unwrap();
        let id = new_id(&made);
        let list = schedule_control(&st, "me", false, "list", &json!({})).await.unwrap();
        assert!(list.contains(&id) && list.contains("wakeup") && list.contains("upgrade") && list.contains("created by you"), "{list}");
        let before = schedule_list(&st).await.unwrap()[0]["at"].as_i64().unwrap();
        schedule_control(&st, "me", false, "update", &json!({"id": &id[..8], "in_minutes": 90, "message": "later"})).await.unwrap();
        let it = schedule_list(&st).await.unwrap()[0].clone();
        assert!(it["at"].as_i64().unwrap() > before + 50 * 60_000);
        assert_eq!(it["text"], persist::wake_message("later"));
        schedule_control(&st, "me", false, "cancel", &json!({"id": id})).await.unwrap();
        assert_eq!(schedule_list(&st).await.unwrap(), json!([]));
        // a window with a stale list can't bring it back
        schedule_put(&st, vec![it]).await.unwrap();
        assert_eq!(schedule_list(&st).await.unwrap(), json!([]));
    }

    #[tokio::test]
    async fn manage_schedule_other_sessions_need_the_lead() {
        let st = sched_state().await;
        let made = schedule_control(&st, "other", false, "create", &json!({"message": "x", "in_minutes": 5})).await.unwrap();
        let id = new_id(&made);
        for (action, args) in [
            ("cancel", json!({"id": id})),
            ("update", json!({"id": id, "in_minutes": 9})),
            ("list", json!({"session_id": "other"})),
            ("list", json!({"all": true})),
            ("create", json!({"session_id": "other", "message": "y", "in_minutes": 5})),
        ] {
            let e = schedule_control(&st, "me", false, action, &args).await.unwrap_err();
            assert!(e.contains("lead"), "{action}: {e}");
        }
        assert!(schedule_control(&st, "me", false, "list", &json!({})).await.unwrap().contains("No scheduled"));
        assert!(schedule_control(&st, "me", true, "list", &json!({"all": true})).await.unwrap().contains(&id));
        schedule_control(&st, "me", true, "cancel", &json!({"id": id})).await.unwrap();
        assert_eq!(schedule_list(&st).await.unwrap(), json!([]));
    }

    #[tokio::test]
    async fn cancelled_or_moved_item_is_never_claimed() {
        let st = sched_state().await;
        let due = json!({"id": "a", "chatId": "me", "text": "t", "at": now_ms() - 1});
        schedule_put(&st, vec![due.clone()]).await.unwrap();
        schedule_control(&st, "me", false, "cancel", &json!({"id": "a"})).await.unwrap();
        assert!(schedule_claim(&st, "a").await.is_none());
        // rescheduled into the future after the tick's scan: not claimed either
        schedule_put(&st, vec![json!({"id": "b", "chatId": "me", "text": "t", "at": now_ms() + 600_000})]).await.unwrap();
        assert!(schedule_claim(&st, "b").await.is_none());
        schedule_put(&st, vec![json!({"id": "c", "chatId": "me", "text": "t", "at": now_ms() - 1})]).await.unwrap();
        assert!(schedule_claim(&st, "c").await.is_some());
        assert!(schedule_claim(&st, "c").await.is_none());
    }

    async fn recording() -> (Arc<BridgeState>, Arc<Mutex<RecState>>, Arc<TurnState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("vicuna-recording-{}", uuid()));
        let db = crate::server_store::open(&dir.join("test.db")).await.unwrap();
        let messages = vec![json!({"role":"user", "content":"go"}), persist::assistant_placeholder("opencode")];
        crate::server_store::create_chat(&db, &json!({
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
    async fn final_reply_waits_for_the_recorder_not_the_done_flag() {
        let (st, rs, turn, _dir) = recording().await;
        let tid = rs.lock().unwrap().turn_id.clone();
        // a periodic write saved only the opening line
        let ev = json!({"type":"token", "text":"Searching."});
        rs.lock().unwrap().rec.apply(&ev, &ev.to_string());
        write_rec(&st, &rs, false).await;
        // the engine loop returned before the recorder saw the closing text
        turn.done.store(true, Ordering::Relaxed);
        let (st2, rs2) = (st.clone(), rs.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(3000)).await;
            let ev = json!({"type":"token", "text":" Here are the headlines."});
            rs2.lock().unwrap().rec.apply(&ev, &ev.to_string());
            st2.recorders.lock().unwrap().remove("channel");
        });
        let reply = final_reply(&st, "chat", &tid).await;
        assert_eq!(reply, "Searching. Here are the headlines.");
    }

    #[tokio::test]
    async fn a_recorder_left_by_an_ended_turn_does_not_lock_the_session() {
        let (st, rs, turn, _dir) = recording().await;
        assert!(live_sessions(&st).contains("chat"), "running turn: the server is the writer");
        // the turn ended without a terminal event reaching the channel
        turn.done.store(true, Ordering::Relaxed);
        assert!(!live_sessions(&st).contains("chat"), "stale recorder must not refuse saves forever");
        // while finalize writes the last copy it is still the only writer
        rs.lock().unwrap().finishing = true;
        assert!(live_sessions(&st).contains("chat"));
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
        let reader = snap["channel"].as_str().unwrap().to_string();
        assert!(reader.starts_with("channel~"), "attach opens a reader of its own");
        assert!(crate::server::drain_events(&st.events, &reader).is_empty(), "snapshot covers queued content");
        // the client already following the channel keeps every event
        let owner = crate::server::drain_events(&st.events, "channel");
        let kinds: Vec<&str> = owner.iter().filter_map(|e| e["type"].as_str()).collect();
        assert_eq!(kinds, ["token", "think", "toolstart"], "attach must not strip the owner's queue");
        ch.send(WorkAgentEvent::ToolEnd { name:"read".into(), output:"<file> & content".into() }).unwrap();
        ch.send(WorkAgentEvent::Token { text:"All done.".into() }).unwrap();
        let fanned = crate::server::drain_events(&st.events, &reader);
        assert_eq!(fanned, crate::server::drain_events(&st.events, "channel"), "both clients see each new event");
        ch.send(WorkAgentEvent::RoundEnd { gen_ms:50, gen_tps:Some(20.0), gen_cost:None }).unwrap();
        let expected = rs.lock().unwrap().rec.messages[1]["content"].clone();
        turn.done.store(true, Ordering::Relaxed);
        ch.send(WorkAgentEvent::Done { stopped:false }).unwrap();
        // The terminal event is released only after its transcript is committed.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let events = crate::server::drain_events(&st.events, "channel");
                if events.iter().any(|e| e["type"] == "done") { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        let tail = crate::server::drain_events(&st.events, &reader);
        assert!(tail.iter().any(|e| e["type"] == "done"), "the reader gets the end of the turn too");
        assert!(!st.events.lock().unwrap().contains_key(&reader), "an ended reader is dropped");
        let db = st.db.as_ref().unwrap();
        let c = crate::server_store::load_chat(db, "chat").await.unwrap().unwrap();
        assert_eq!(c["messages"][1]["content"], expected);
        assert_eq!(c["messages"][1]["thinking"], "Reasoning");
        assert_eq!(c["messages"][1]["genMs"], 50.0);
        db.close().await;
        let reopened = crate::server_store::open(&dir.join("test.db")).await.unwrap();
        let c = crate::server_store::load_chat(&reopened, "chat").await.unwrap().unwrap();
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
        let c = crate::server_store::load_chat(st.db.as_ref().unwrap(), "chat").await.unwrap().unwrap();
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
        let c = crate::server_store::load_chat(st.db.as_ref().unwrap(),"chat").await.unwrap().unwrap();
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
    async fn manage_session_permissions_are_lead_only_and_persisted() {
        let (st, _, _, dir) = recording().await;
        let db = st.db.as_ref().unwrap();
        crate::server_store::create_chat(db, &json!({
            "id":"lead", "title":"Lead", "createdAt":1, "mode":"work",
            "extra":r#"{"role":"lead"}"#, "messages":[]
        })).await.unwrap();
        crate::server_store::create_chat(db, &json!({
            "id":"sender", "title":"Sender", "createdAt":1, "mode":"work",
            "extra":r#"{}"#, "messages":[]
        })).await.unwrap();

        let denied = session_control(&st, "sender", "permissions", &json!({"session_id":"chat", "read":false})).await.unwrap_err();
        assert!(denied.contains("Only the lead"));
        let defaults: Value = serde_json::from_str(&session_control(&st, "lead", "permissions", &json!({"session_id":"chat"})).await.unwrap()).unwrap();
        assert_eq!(defaults["read"], true);
        assert_eq!(defaults["allow_message"], false);
        session_control(&st, "lead", "permissions", &json!({"session_id":"chat", "read":false, "allow_message":true})).await.unwrap();
        let saved: Value = serde_json::from_str(&session_control(&st, "lead", "permissions", &json!({"session_id":"chat"})).await.unwrap()).unwrap();
        assert_eq!(saved["read"], false);
        assert_eq!(saved["allow_message"], true);

        db.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn permissioned_nonlead_send_runs_through_server_control_without_report_back() {
        let (st, rs, target_turn, dir) = recording().await;
        let db = st.db.as_ref().unwrap();
        crate::server_store::create_chat(db, &json!({
            "id":"sender", "title":"Sender", "createdAt":1, "mode":"work", "workspace":"/srv/p",
            "extra":r#"{"permissions":{"message":true}}"#, "messages":[]
        })).await.unwrap();

        let caller_turn = Arc::new(TurnState::new());
        st.agent.next_turn(caller_turn.clone());
        let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
        caller_turn.ask_pending.lock().unwrap().insert("send-1".into(), answer_tx);
        let ch = work_channel(st.clone(), "caller".into(), None, caller_turn.clone(), Some("sender".into()));
        ch.send(WorkAgentEvent::SessionControl {
            req_id: "send-1".into(),
            action: "send".into(),
            args: json!({"session_id":"chat", "message":"run this", "report":false}),
        }).unwrap();

        let answer = tokio::time::timeout(Duration::from_secs(2), answer_rx).await.unwrap().unwrap();
        let reply: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(reply["ok"], true);
        assert!(reply["text"].as_str().unwrap().contains("Queued into session chat"));
        assert_eq!(target_turn.queue.lock().unwrap().len(), 1);
        assert_eq!(rs.lock().unwrap().report_to, None, "the permissioned sender gets an acknowledgement only");

        db.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn permissioned_nonlead_send_with_report_true_attaches_report_callback() {
        let (st, rs, target_turn, dir) = recording().await;
        let db = st.db.as_ref().unwrap();
        crate::server_store::create_chat(db, &json!({
            "id":"sender", "title":"Sender", "createdAt":1, "mode":"work", "workspace":"/srv/p",
            "extra":r#"{"permissions":{"message":true}}"#, "messages":[]
        })).await.unwrap();
        let text = session_control(&st, "sender", "send", &json!({"session_id":"chat", "message":"run this", "report":true})).await.unwrap();
        assert!(text.contains("Queued into session chat"));
        assert_eq!(target_turn.queue.lock().unwrap().len(), 1);
        assert_eq!(rs.lock().unwrap().report_to.as_deref(), Some("sender"), "the reply is reported back to the non-lead caller");
        db.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn report_delivery_marks_the_receiving_turn_report_woken() {
        let (st, _, target_turn, dir) = recording().await;
        assert!(!target_turn.report_woken.load(Ordering::Relaxed));
        let msg = json!({"role":"user","content":persist::worker_done_message("w","W","claude",None,"done",false)});
        assert!(persist::is_worker_done_message(msg["content"].as_str().unwrap()));
        deliver_report(&st, "chat", msg).await;
        assert!(target_turn.report_woken.load(Ordering::Relaxed));
        assert_eq!(target_turn.queue.lock().unwrap().len(), 1);
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
        let db = crate::server_store::open(&dir.join("test.db")).await.unwrap();
        let restarted = BridgeState { db:Some(db), ..Default::default() };
        assert_eq!(schedule_list(&restarted).await.unwrap()[0], item);
        schedule_remove(&restarted, vec!["wake".into()]).await.unwrap();
        schedule_put(&restarted, vec![item]).await.unwrap();
        assert_eq!(schedule_list(&restarted).await.unwrap(), json!([]));
        restarted.db.as_ref().unwrap().close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
