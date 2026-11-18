use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use tauri::ipc::{Channel, InvokeResponseBody};

use super::{BridgeState, Options, Turn, images_stripped, token};
use crate::{ChatMessage, ollama_chat_stream, ollama_generate_stream, ollama_model_show};

#[derive(Default)]
pub(super) struct Stats {
    pub tokens: u64,
    pub seconds: Option<f64>,
    pub cost: Option<f64>,
}

pub(super) async fn generate(
    st: &Arc<BridgeState>,
    turn: &Arc<Turn>,
    mut options: Options,
    history: Vec<Value>,
) -> Result<Stats, String> {
    if let Some(model) = options.model.strip_prefix("openrouter:") {
        return openrouter(st, turn, model, &options, history).await;
    }
    if turn.extending {
        options.think = Some(json!(false));
        if options.continue_mode.as_deref() != Some("native")
            && history.iter().all(|m| m["images"].is_null())
        {
            let family = ollama_model_show(options.model.clone())
                .await
                .ok()
                .and_then(|info| info.family);
            if let Some((prompt, markers)) = raw_prompt(
                &history,
                family.as_deref(),
                options.continue_mode.as_deref(),
            ) {
                for marker in markers {
                    if !options.stop.iter().any(|stop| stop == marker) {
                        options.stop.push((*marker).to_owned());
                    }
                }
                return ollama(st, turn, options, history, Some(prompt)).await;
            }
        }
    }
    ollama(st, turn, options, history, None).await
}

/// Both transports reuse the Ollama request and NDJSON streaming code used by
/// the desktop commands. The server owns retry and cancellation policy.
async fn ollama(
    st: &Arc<BridgeState>,
    turn: &Arc<Turn>,
    mut options: Options,
    mut history: Vec<Value>,
    prompt: Option<String>,
) -> Result<Stats, String> {
    let temperature = turn.extending.then_some(0.2).or(options.temperature);
    let stop = turn.extending.then_some(options.stop);
    let mut retries = 0;
    loop {
        let raw = prompt.is_some();
        let done = Arc::new(Mutex::new(Value::Null));
        let capture = done.clone();
        let event_st = st.clone();
        let event_turn = turn.clone();
        // Channel callbacks implement Fn. The shared state also lets us
        // ignore trailing chunks from the packet that triggered a raw cut.
        let feed = move |body| {
            let InvokeResponseBody::Json(body) = body else {
                return Ok(());
            };
            let event: Value = serde_json::from_str(&body).unwrap_or_default();
            let mut done = capture.lock().unwrap();
            if done["cut"] == true {
                return Ok(());
            }
            match event["type"].as_str() {
                Some("done") => *done = event,
                Some("token" | "think") => {
                    if !token(
                        &event_st,
                        &event_turn,
                        event["text"].as_str().unwrap_or(""),
                        event["type"] == "think",
                        raw,
                    ) {
                        *done = json!({"cut": true});
                        event_turn.stop.notify_one();
                    }
                }
                _ => {}
            }
            Ok(())
        };
        let result = if let Some(prompt) = &prompt {
            ollama_generate_stream(
                options.model.clone(),
                prompt.clone(),
                temperature,
                stop.clone(),
                Channel::new(feed),
            )
            .await
        } else {
            let messages = history.iter().map(ollama_message).collect::<Vec<_>>();
            ollama_chat_stream(
                options.model.clone(),
                messages,
                options.think.clone(),
                temperature,
                stop.clone(),
                None,
                Channel::new(feed),
            )
            .await
        };
        let info = done.lock().unwrap().clone();
        if info["cut"] == true {
            return Ok(Stats::default());
        }
        match result {
            Ok(text) if text.is_empty() && info["done_reason"] == "load" && retries < 3 => {
                retries += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(_) => {
                let seconds = info["eval_duration"]
                    .as_u64()
                    .filter(|ns| *ns > 0)
                    .map(|ns| ns as f64 / 1e9);
                return Ok(Stats {
                    tokens: info["eval_count"].as_u64().unwrap_or(0),
                    seconds,
                    cost: None,
                });
            }
            Err(err)
                if err.contains("does not support multimodal") && strip_images(&mut history) =>
            {
                images_stripped(st, turn)
            }
            Err(err)
                if retries == 0
                    && err.contains("does not support thinking")
                    && options.think != Some(json!(false)) =>
            {
                options.think = Some(json!(false));
                retries += 1;
            }
            Err(err)
                if retries == 0
                    && err.contains("think")
                    && options.think.as_ref().is_some_and(Value::is_string) =>
            {
                options.think = Some(json!(true));
            }
            Err(err) => return Err(err),
        }
    }
}

fn ollama_message(msg: &Value) -> ChatMessage {
    ChatMessage {
        role: msg["role"].as_str().unwrap_or("user").to_owned(),
        content: msg["content"].as_str().unwrap_or("").to_owned(),
        images: msg["images"].as_array().map(|images| {
            images
                .iter()
                .filter_map(Value::as_str)
                .map(|image| {
                    image
                        .split_once(',')
                        .map_or(image, |(_, data)| data)
                        .to_owned()
                })
                .collect()
        }),
        tool_calls: None,
        tool_name: None,
    }
}

fn strip_images(history: &mut [Value]) -> bool {
    let mut stripped = false;
    for message in history {
        if let Some(obj) = message.as_object_mut() {
            stripped |= obj.remove("images").is_some();
        }
    }
    stripped
}

fn raw_prompt(
    history: &[Value],
    family: Option<&str>,
    mode: Option<&str>,
) -> Option<(String, &'static [&'static str])> {
    let family = family?.trim().to_ascii_lowercase();
    let family = family.split('.').next()?;
    let gemma4 = family == "gemma4";
    if mode == Some("native")
        || (!gemma4 && (mode == Some("auto") || !matches!(family, "gemma2" | "gemma3")))
    {
        return None;
    }
    let (last, head) = history.split_last()?;
    if last["role"] != "assistant" {
        return None;
    }
    let mut prompt = String::from("<bos>");
    let mut system = String::new();
    for message in head {
        let role = match message["role"].as_str() {
            Some("assistant") => "model",
            Some("system") => "system",
            _ => "user",
        };
        let content = message["content"].as_str().unwrap_or("");
        if gemma4 {
            prompt.push_str(&format!("<|turn>{role}\n{content}<turn|>\n"));
        } else if role == "system" {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(content);
        } else {
            prompt.push_str(&format!("<start_of_turn>{role}\n"));
            if role == "user" && !system.is_empty() {
                prompt.push_str(&system);
                prompt.push_str("\n\n");
                system.clear();
            }
            prompt.push_str(content);
            prompt.push_str("<end_of_turn>\n");
        }
    }
    let (prefill, markers): (&str, &[_]) = if gemma4 {
        (
            "<|turn>model\n",
            &["<turn|>", "<|turn>", "<|channel>", "<channel|>"],
        )
    } else {
        (
            "<start_of_turn>model\n",
            &["<end_of_turn>", "<start_of_turn>"],
        )
    };
    prompt.push_str(prefill);
    prompt.push_str(last["content"].as_str().unwrap_or(""));
    Some((prompt, markers))
}

pub(super) fn trim_repetition(text: &mut String) -> bool {
    if text.len() < 24 {
        return false;
    }
    let mut cut = false;
    for _ in 0..64 {
        let tail: Vec<_> = text.chars().rev().take(4000).collect();
        let mut repeated = None;
        for unit in 2..=32 {
            if tail.len() < unit * 6 {
                break;
            }
            let pattern = &tail[..unit];
            if !pattern
                .iter()
                .any(|c| c.is_ascii_alphanumeric() || (!c.is_ascii() && !c.is_whitespace()))
            {
                continue;
            }
            let count = tail
                .chunks_exact(unit)
                .take_while(|chunk| *chunk == pattern)
                .count()
                * unit;
            if count >= 24 && count >= 6 * unit {
                repeated = Some(tail[..count].iter().map(|c| c.len_utf8()).sum::<usize>());
                break;
            }
        }
        let Some(bytes) = repeated else { break };
        text.truncate(text.len() - bytes);
        cut = true;
    }
    cut
}

async fn openrouter(
    st: &BridgeState,
    turn: &Turn,
    model: &str,
    options: &Options,
    mut history: Vec<Value>,
) -> Result<Stats, String> {
    let key = options
        .open_router_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or("No OpenRouter API key — add one in Settings → OpenRouter.")?;
    let client = reqwest::Client::new();
    let mut retried = false;
    let response = loop {
        let messages: Vec<_> = history.iter().map(openrouter_message).collect();
        let mut body = json!({"model": model, "messages": messages, "stream": true, "usage": {"include": true}});
        match options.think.as_ref() {
            Some(t) if t == false => body["reasoning"] = json!({"enabled": false}),
            Some(t)
                if matches!(
                    t.as_str(),
                    Some("minimal" | "low" | "medium" | "high" | "xhigh")
                ) =>
            {
                body["reasoning"] = json!({"effort": t});
            }
            _ => {}
        }
        let response = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("X-Title", "Vicuna")
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|_| "Couldn't reach OpenRouter — check your internet connection.")?;
        let status = response.status();
        if status.is_success() {
            break response;
        }
        if !retried && matches!(status.as_u16(), 500 | 502 | 503 | 529) {
            retried = true;
            continue;
        }
        let detail = response.json::<Value>().await.unwrap_or_default();
        let detail = detail["error"]["message"].as_str().unwrap_or("");
        let lower = detail.to_ascii_lowercase();
        if lower.contains("image")
            && (lower.contains("support")
                || lower.contains("modality")
                || lower.contains("multimodal"))
            && strip_images(&mut history)
        {
            images_stripped(st, turn);
            retried = false;
            continue;
        }
        let message = match status.as_u16() {
            401 if detail.is_empty() => {
                "OpenRouter rejected the API key (401). Check it and try again.".into()
            }
            401 => format!("OpenRouter rejected the API key (401): {detail}"),
            402 => "OpenRouter says this key is out of credit.".into(),
            403 if detail.is_empty() => "OpenRouter refused this key (403).".into(),
            403 => format!("OpenRouter refused this key (403): {detail}"),
            429 => "OpenRouter is rate-limiting this key — wait a moment and retry.".into(),
            _ if detail.is_empty() => format!("OpenRouter request failed ({status})"),
            _ => format!("OpenRouter request failed ({status}): {detail}"),
        };
        return Err(message);
    };

    let mut completion = Completion::default();
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| s.starts_with("application/json"))
    {
        let event = response
            .json::<Value>()
            .await
            .map_err(|_| "OpenRouter returned an unreadable response.")?;
        completion.feed(st, turn, &event)?;
    } else {
        let mut stream = response.bytes_stream();
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.map_err(|e| format!("OpenRouter stream failed: {e}"))?);
            let mut end = 0;
            for line in buf.split_inclusive(|b| *b == b'\n') {
                if !line.ends_with(b"\n") {
                    break;
                }
                completion.line(st, turn, line)?;
                end += line.len();
            }
            buf.drain(..end);
        }
        completion.line(st, turn, &buf)?;
    }
    Ok(completion.stats())
}

fn openrouter_message(message: &Value) -> Value {
    if message["role"] == "user" {
        if let Some(images) = message["images"].as_array() {
            let mut parts = Vec::new();
            if message["content"].as_str().is_some_and(|s| !s.is_empty()) {
                parts.push(json!({"type": "text", "text": message["content"]}));
            }
            parts.extend(
                images
                    .iter()
                    .map(|url| json!({"type": "image_url", "image_url": {"url": url}})),
            );
            return json!({"role": "user", "content": parts});
        }
    }
    json!({"role": message["role"], "content": message["content"]})
}

#[derive(Default)]
struct Completion {
    tokens: Option<u64>,
    cost: Option<f64>,
    text_len: usize,
    start: Option<Instant>,
    end: Option<Instant>,
}

impl Completion {
    fn line(&mut self, st: &BridgeState, turn: &Turn, line: &[u8]) -> Result<(), String> {
        if let Some(data) = line.strip_prefix(b"data:") {
            if let Ok(event) = serde_json::from_slice::<Value>(data) {
                self.feed(st, turn, &event)?;
            }
        }
        Ok(())
    }

    fn feed(&mut self, st: &BridgeState, turn: &Turn, event: &Value) -> Result<(), String> {
        if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
            let detail = error
                .as_str()
                .or_else(|| error["message"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            return Err(format!("OpenRouter error: {detail}"));
        }
        self.tokens = event["usage"]["completion_tokens"].as_u64().or(self.tokens);
        self.cost = event["usage"]["cost"].as_f64().or(self.cost);
        let choice = &event["choices"][0];
        let delta = choice.get("delta").unwrap_or(&choice["message"]);
        let mut think = content_text(&delta["reasoning"]);
        if think.is_empty() {
            think = content_text(&delta["reasoning_content"]);
        }
        if !think.is_empty() {
            self.start.get_or_insert_with(Instant::now);
            token(st, turn, &think, true, false);
        }
        let text = content_text(&delta["content"]);
        if !text.is_empty() {
            let now = Instant::now();
            self.start.get_or_insert(now);
            self.end = Some(now);
            self.text_len += text.encode_utf16().count();
            token(st, turn, &text, false, false);
        }
        Ok(())
    }

    fn stats(self) -> Stats {
        Stats {
            tokens: self.tokens.unwrap_or(((self.text_len + 2) / 4) as u64),
            seconds: self
                .start
                .zip(self.end)
                .map(|(start, end)| end.duration_since(start).as_secs_f64())
                .filter(|s| *s > 0.0),
            cost: self.cost,
        }
    }
}

fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["text"].as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_prefills_the_matching_gemma_template() {
        let history = [
            json!({"role": "system", "content": "system"}),
            json!({"role": "user", "content": "user"}),
            json!({"role": "assistant", "content": "partial "}),
        ];
        assert_eq!(
            raw_prompt(&history, Some("gemma4"), Some("auto")).unwrap(),
            ("<bos><|turn>system\nsystem<turn|>\n<|turn>user\nuser<turn|>\n<|turn>model\npartial ".into(),
             &["<turn|>", "<|turn>", "<|channel>", "<channel|>"][..]),
        );
        assert_eq!(
            raw_prompt(&history, Some("gemma3"), Some("raw")).unwrap().0,
            "<bos><start_of_turn>user\nsystem\n\nuser<end_of_turn>\n<start_of_turn>model\npartial ",
        );
        assert!(raw_prompt(&history, Some("gemma3"), Some("auto")).is_none());
        assert!(raw_prompt(&history, Some("gemma4"), Some("native")).is_none());
        assert!(raw_prompt(&history, Some("qwen3"), Some("raw")).is_none());
    }

    #[test]
    fn trims_the_repeated_raw_tail() {
        let mut text = format!("A clean reply.{}", " owns".repeat(8));
        assert!(trim_repetition(&mut text));
        assert_eq!(text, "A clean reply.");
        assert!(!trim_repetition(
            &mut "------------------------------".into()
        ));
    }
}
