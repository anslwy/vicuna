// Live MCP verification: real `lb mcp` stdio server + real Ollama model.
// Run explicitly — it needs the live-browser daemon, `lb` on PATH, and a
// working Ollama (cloud) model:
//
//   VICUNA_LIVE_MCP=1 cargo test --manifest-path src-tauri/Cargo.toml \
//     --test mcp_live -- --nocapture
//
// Without the env var the test passes trivially so plain `cargo test`
// never hangs in CI.

use serde_json::{json, Value};
use vicuna_lib::mcp::{McpManager, McpServerConfig};

fn live_enabled() -> bool {
    std::env::var("VICUNA_LIVE_MCP").as_deref() == Ok("1")
}

fn test_config() -> McpServerConfig {
    McpServerConfig {
        id: "live-browser".to_string(),
        name: "live-browser".to_string(),
        transport: "stdio".to_string(),
        command: Some(vec!["lb".to_string(), "mcp".to_string()]),
        env: None,
        url: None,
        headers: None,
    }
}

async fn ollama_chat(model: &str, messages: Value, tools: Option<Value>) -> Value {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .unwrap();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if let Some(t) = tools {
        body["tools"] = t;
    }
    let res = client
        .post("http://localhost:11434/api/chat")
        .json(&body)
        .send()
        .await
        .expect("ollama chat request failed");
    assert!(
        res.status().is_success(),
        "ollama chat status: {}",
        res.status()
    );
    res.json::<Value>().await.expect("ollama chat bad JSON")
}

#[tokio::test]
async fn live_browser_mcp_tool_use() {
    if !live_enabled() {
        eprintln!("skipped (set VICUNA_LIVE_MCP=1 to run against live services)");
        return;
    }
    let model = std::env::var("VICUNA_LIVE_MODEL").unwrap_or("gemma4:31b-cloud".to_string());

    // 1. connect + list tools over real stdio
    let mgr = McpManager::default();
    let info = mgr
        .connect(&test_config())
        .await
        .expect("mcp connect failed");
    assert!(info.connected);
    let names: Vec<&str> = info.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"status"), "no status tool: {names:?}");
    assert!(names.contains(&"eval"), "no eval tool: {names:?}");
    eprintln!("connected: {} tools", names.len());

    // 2. call one tool directly
    let status = mgr
        .call_tool("live-browser", "status", json!({}))
        .await
        .expect("status call failed");
    assert!(!status.is_error, "status error: {}", status.content);
    assert!(!status.content.trim().is_empty(), "status empty");
    eprintln!("status => {}", status.content.chars().take(120).collect::<String>());

    // 3. plain chat still works with tools attached (no forced call)
    let eval_schema = info
        .tools
        .iter()
        .find(|t| t.name == "eval")
        .map(|t| t.input_schema.clone())
        .unwrap_or(json!({"type": "object"}));
    let tools = json!([{ "type": "function", "function": {
        "name": "eval",
        "description": "Evaluate JavaScript in the page",
        "parameters": eval_schema,
    }}]);
    let r1 = ollama_chat(
        &model,
        json!([{ "role": "user", "content": "Reply with exactly: standing by" }]),
        Some(tools.clone()),
    )
    .await;
    let text1 = r1["message"]["content"].as_str().unwrap_or("");
    assert!(
        text1.to_lowercase().contains("standing by"),
        "plain reply broken: {text1:?} full: {r1}"
    );

    // 4. model calls the tool, we execute it via MCP, model answers with it
    let r2 = ollama_chat(
        &model,
        json!([{ "role": "user", "content": "Call the eval tool with js set to the string \"40 + 2\" and nothing else matters." }]),
        Some(tools.clone()),
    )
    .await;
    let calls = r2["message"]["tool_calls"].as_array().cloned().unwrap_or_default();
    assert!(!calls.is_empty(), "model made no tool call: {r2}");
    assert_eq!(calls[0]["function"]["name"].as_str(), Some("eval"));
    let args = calls[0]["function"]["arguments"].clone();
    eprintln!("model called eval with {args}");
    let exec = mgr
        .call_tool("live-browser", "eval", args)
        .await
        .expect("eval call failed");
    assert!(!exec.is_error, "eval error: {}", exec.content);
    eprintln!("eval => {}", exec.content.chars().take(120).collect::<String>());

    let r3 = ollama_chat(
        &model,
        json!([
            { "role": "user", "content": "Call the eval tool with js set to the string \"40 + 2\" and nothing else matters." },
            { "role": "assistant", "content": r2["message"]["content"].as_str().unwrap_or(""),
              "tool_calls": calls },
            { "role": "tool", "content": exec.content },
        ]),
        Some(tools),
    )
    .await;
    let text3 = r3["message"]["content"].as_str().unwrap_or("");
    assert!(!text3.trim().is_empty(), "final answer empty: {r3}");
    eprintln!("final => {}", text3.chars().take(200).collect::<String>());

    mgr.disconnect("live-browser").await.ok();
}
