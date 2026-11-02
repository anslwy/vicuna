// Vicuna headless bridge: loopback HTTP (+ poll-based events) exposing the
// backend commands so a plain browser (live-browser/Chrome) can drive the
// REAL backend without the Tauri shell. Dev/test tool only: binds
// 127.0.0.1, no auth — never expose beyond loopback.
//
//   cargo run --bin bridge [--port 1421]      (src-tauri/)
//   POST /invoke {"cmd": "...", "args": {...}} -> {"ok": true, "data": ...}
//   GET  /poll/<channel-id>                    -> {"events": [ ... ]}
//   GET  /health                                -> {"ok": true}
//
// The dispatch surface itself lives in the shared `vicuna_bridge` lib (also
// used by `bin/worker.rs`, the remote worker). This binary stays the
// zero-config local dev server: no auth, no persistence, in-memory queues.

use std::sync::Arc;

use vicuna_bridge::{drain_events, json_resp, BridgeState, BODY_CAP};

fn handle(rt: &tokio::runtime::Handle, st: &Arc<BridgeState>, mut req: tiny_http::Request) {
    use tiny_http::Method::{Get, Options, Post};
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if *req.method() == Options {
        let _ = req.respond(json_resp(String::new()));
        return;
    }
    if *req.method() == Get && path == "/health" {
        let _ = req.respond(json_resp(r#"{"ok":true,"bridge":1}"#.to_string()));
        return;
    }
    if *req.method() == Get && path.starts_with("/poll/") {
        let id = path.trim_start_matches("/poll/").to_string();
        let events = drain_events(&st.events, &id);
        let body = serde_json::json!({ "events": events }).to_string();
        let _ = req.respond(json_resp(body));
        return;
    }
    if *req.method() == Post && path == "/invoke" {
        let mut body: Vec<u8> = Vec::new();
        let mut ok = true;
        {
            let reader = req.as_reader();
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if body.len() + n > BODY_CAP as usize {
                            ok = false;
                            break;
                        }
                        body.extend_from_slice(&buf[..n]);
                    }
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        let out = if !ok {
            serde_json::json!({"ok": false, "error": "unreadable or oversized body"}).to_string()
        } else {
            match serde_json::from_slice::<serde_json::Value>(&body) {
                Err(e) => serde_json::json!({"ok": false, "error": format!("bad json: {e}")}).to_string(),
                Ok(v) => {
                    let cmd = v.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
                    match rt.block_on(vicuna_bridge::dispatch(st, cmd, &args)) {
                        Ok(data) => serde_json::json!({"ok": true, "data": data}).to_string(),
                        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
                    }
                }
            }
        };
        let _ = req.respond(json_resp(out));
        return;
    }
    let _ = req.respond(json_resp(r#"{"ok":false,"error":"not found"}"#.to_string()));
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1421);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("bridge runtime");
    let state = Arc::new(BridgeState::default());
    let server =
        tiny_http::Server::http(format!("127.0.0.1:{port}")).expect("bridge bind 127.0.0.1");
    println!("vicuna bridge on http://127.0.0.1:{port} (dev only: no auth, loopback)");
    for req in server.incoming_requests() {
        let st = state.clone();
        let h = rt.handle().clone();
        std::thread::spawn(move || handle(&h, &st, req));
    }
}
