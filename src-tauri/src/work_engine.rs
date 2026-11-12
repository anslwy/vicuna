// Work engines: which external agent CLI a Work turn runs on.
//
// opencode and fx speak ACP (acp.rs), Claude Code stream-json (claude_cli.rs)
// and Codex its app-server JSON-RPC (codex_cli.rs); this module owns the choice
// between them (persisted in ~/vicuna/config/work-engine.json), the combined
// detection the UI renders (engine picker when more than one is installed),
// and per-engine model lists + saved model picks.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::State;

use crate::opencode::{home_dir, OpencodeModels, WorkEngineInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Opencode,
    Claude,
    Codex,
    Fx,
}

impl Engine {
    pub const ALL: [Engine; 4] = [Engine::Opencode, Engine::Claude, Engine::Codex, Engine::Fx];

    pub fn id(self) -> &'static str {
        match self {
            Engine::Opencode => "opencode",
            Engine::Claude => "claude",
            Engine::Codex => "codex",
            Engine::Fx => "fx",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::Opencode => "OpenCode",
            Engine::Claude => "Claude Code",
            Engine::Codex => "Codex",
            Engine::Fx => "fx",
        }
    }

    pub fn parse(s: &str) -> Option<Engine> {
        match s.trim().to_ascii_lowercase().as_str() {
            "opencode" => Some(Engine::Opencode),
            "claude" | "claude-code" | "claude_code" => Some(Engine::Claude),
            "codex" | "codex-cli" => Some(Engine::Codex),
            "fx" => Some(Engine::Fx),
            _ => None,
        }
    }

    /// Cheap presence check (stats only).
    pub fn is_present(self) -> bool {
        match self {
            Engine::Opencode => crate::opencode::find_opencode().is_some(),
            Engine::Claude => crate::claude::find_claude().is_some(),
            Engine::Codex => crate::codex::find_codex().is_some(),
            Engine::Fx => crate::fx::find_fx().is_some(),
        }
    }

    pub fn missing_message(self) -> String {
        match self {
            Engine::Opencode => crate::acp::not_found_message(),
            Engine::Claude => crate::claude::missing_message(),
            Engine::Codex => crate::codex::missing_message(),
            Engine::Fx => crate::fx::missing_message(),
        }
    }

    /// The label prefix for a model id in reply footers: opencode ids are
    /// already `provider/model`; Claude's are bare ("haiku" -> "claude/haiku");
    /// fx's are `<fx provider>/<model>` ("fx/codex/gpt-6-luna").
    pub fn reply_label(self, model: &str) -> String {
        match self {
            Engine::Opencode => model.to_string(),
            Engine::Claude => format!("claude/{model}"),
            Engine::Codex => format!("codex/{model}"),
            Engine::Fx => format!("fx/{model}"),
        }
    }

    pub fn saved_model(self) -> Option<String> {
        match self {
            Engine::Opencode => crate::opencode::saved_model(),
            Engine::Claude => crate::claude::saved_model(),
            Engine::Codex => crate::codex::saved_model(),
            Engine::Fx => crate::fx::saved_model(),
        }
    }

    pub fn save_model(self, model: Option<&str>) -> Result<(), String> {
        match self {
            Engine::Opencode => crate::opencode::save_model(model),
            Engine::Claude => crate::claude::save_model(model),
            Engine::Codex => crate::codex::save_model(model),
            Engine::Fx => crate::fx::save_model(model),
        }
    }

    async fn info(self) -> WorkEngineInfo {
        match self {
            Engine::Opencode => crate::opencode::engine_info().await,
            Engine::Claude => crate::claude::engine_info().await,
            Engine::Codex => crate::codex::engine_info().await,
            Engine::Fx => crate::fx::engine_info().await,
        }
    }
}

// ---------- persisted engine choice ----------

fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("work-engine.json"))
}

pub fn read_engine_from(path: &Path) -> Option<Engine> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Engine::parse(v.get("engine")?.as_str()?)
}

fn update_prefs(path: &Path, key: &str, value: Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut doc = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    doc[key] = value;
    let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn write_engine_to(path: &Path, engine: Engine) -> Result<(), String> {
    update_prefs(path, "engine", json!(engine.id()))
}

/// Tool approval: "auto" (default; every permission request is approved and
/// noted in the transcript) or "ask" (each request becomes an ask card in the
/// chat; the user picks Allow / Reject).
pub fn read_ask_from(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("approval").and_then(|a| a.as_str()).map(|a| a == "ask"))
        .unwrap_or(false)
}

pub fn write_ask_to(path: &Path, ask: bool) -> Result<(), String> {
    update_prefs(path, "approval", json!(if ask { "ask" } else { "auto" }))
}

pub fn approval_ask() -> bool {
    prefs_path().is_some_and(|p| read_ask_from(&p))
}

pub fn saved_engine() -> Option<Engine> {
    read_engine_from(&prefs_path()?)
}

/// The engine a turn or the UI uses: the saved pick while it is installed;
/// otherwise the first installed engine (opencode first); otherwise the saved
/// pick (or opencode) so the UI shows that engine's install hint.
pub fn choose(saved: Option<Engine>, present: impl Fn(Engine) -> bool) -> Engine {
    if let Some(e) = saved.filter(|e| present(*e)) {
        return e;
    }
    Engine::ALL
        .into_iter()
        .find(|e| present(*e))
        .unwrap_or(saved.unwrap_or(Engine::Opencode))
}

pub fn current_engine() -> Engine {
    choose(saved_engine(), Engine::is_present)
}

/// A turn's engine: the one the frontend sent (it shows that engine's
/// picker), else the current choice.
pub fn effective_engine(requested: Option<&str>) -> Engine {
    requested.and_then(Engine::parse).unwrap_or_else(current_engine)
}

/// Detection for the UI: the current engine's info plus a summary of all.
pub async fn engines_info() -> WorkEngineInfo {
    let mut infos = Vec::new();
    for e in Engine::ALL {
        infos.push((e, e.info().await));
    }
    let saved = saved_engine();
    let chosen = choose(saved, |e| infos.iter().any(|(x, i)| *x == e && i.installed));
    let summaries = infos.iter().map(|(_, i)| crate::claude::summary(i)).collect();
    let mut info = infos
        .into_iter()
        .find(|(e, _)| *e == chosen)
        .map(|(_, i)| i)
        .expect("every engine has info");
    info.engines = summaries;
    info.ask_permission = approval_ask();
    info
}

/// A failed CLI spawn. With a missing working folder the OS reports ENOENT,
/// which reads like the binary is missing, so name the folder instead.
pub fn spawn_error(name: &str, bin: &Path, cwd: &Path, e: &std::io::Error) -> String {
    if !cwd.is_dir() {
        return format!("workspace folder not found: {} (pick an existing folder for this Work chat)", cwd.display());
    }
    format!("cannot start {name} ({}): {e}", bin.display())
}

// ---------- models ----------

pub async fn list_models(engine: Engine, acp: &crate::acp::AcpState) -> Result<OpencodeModels, String> {
    match engine {
        Engine::Opencode => crate::opencode::list_models().await,
        Engine::Claude => Ok(OpencodeModels {
            models: crate::claude_cli::list_models().await?,
            current: crate::claude::saved_model(),
        }),
        Engine::Codex => Ok(OpencodeModels {
            models: crate::codex_cli::list_models().await?,
            current: crate::codex::saved_model(),
        }),
        Engine::Fx => Ok(OpencodeModels {
            models: acp.fx_models().await?,
            current: crate::fx::saved_model(),
        }),
    }
}

// ---------- shared logic (Tauri commands + headless bridge) ----------

pub async fn select_engine(engine: &str) -> Result<WorkEngineInfo, String> {
    let e = Engine::parse(engine).ok_or_else(|| format!("unknown Work engine: {engine}"))?;
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_engine_to(&path, e)?;
    Ok(engines_info().await)
}

pub async fn set_approval(ask: bool) -> Result<WorkEngineInfo, String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_ask_to(&path, ask)?;
    Ok(engines_info().await)
}

pub fn set_model(engine: Option<&str>, model: Option<&str>) -> Result<(), String> {
    effective_engine(engine).save_model(model)
}

// ---------- Tauri commands ----------

#[tauri::command]
pub async fn work_engine_select(engine: String) -> Result<WorkEngineInfo, String> {
    select_engine(&engine).await
}

#[tauri::command]
pub async fn work_models(engine: Option<String>, acp: State<'_, crate::acp::AcpState>) -> Result<OpencodeModels, String> {
    list_models(effective_engine(engine.as_deref()), &acp).await
}

#[tauri::command]
pub async fn work_set_approval(ask: bool) -> Result<WorkEngineInfo, String> {
    set_approval(ask).await
}

#[tauri::command]
pub async fn work_set_model(engine: Option<String>, model: Option<String>) -> Result<(), String> {
    set_model(engine.as_deref(), model.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ids_roundtrip_and_labels() {
        for e in Engine::ALL {
            assert_eq!(Engine::parse(e.id()), Some(e));
        }
        assert_eq!(Engine::parse("Claude-Code"), Some(Engine::Claude));
        assert_eq!(Engine::parse("gpt"), None);
        assert_eq!(Engine::Claude.reply_label("haiku"), "claude/haiku");
        assert_eq!(Engine::parse("Codex"), Some(Engine::Codex));
        assert_eq!(Engine::Codex.reply_label("gpt-6-luna"), "codex/gpt-6-luna");
        assert_eq!(Engine::Opencode.reply_label("opencode/beam-free"), "opencode/beam-free");
        assert_eq!(Engine::parse("FX"), Some(Engine::Fx));
        assert_eq!(Engine::Fx.reply_label("codex/gpt-6-luna"), "fx/codex/gpt-6-luna");
        assert_eq!(Engine::Fx.label(), "fx");
    }

    #[test]
    fn choose_prefers_saved_installed_engine_then_any_installed() {
        let both = |_: Engine| true;
        let only_claude = |e: Engine| e == Engine::Claude;
        let none = |_: Engine| false;
        assert_eq!(choose(None, both), Engine::Opencode);
        assert_eq!(choose(Some(Engine::Claude), both), Engine::Claude);
        // saved opencode but it was uninstalled: fall over to claude
        assert_eq!(choose(Some(Engine::Opencode), only_claude), Engine::Claude);
        assert_eq!(choose(None, only_claude), Engine::Claude);
        // nothing installed: keep the saved pick so its install hint shows
        assert_eq!(choose(Some(Engine::Claude), none), Engine::Claude);
        assert_eq!(choose(None, none), Engine::Opencode);
        let only_codex = |e: Engine| e == Engine::Codex;
        assert_eq!(choose(Some(Engine::Claude), only_codex), Engine::Codex);
        assert_eq!(choose(Some(Engine::Codex), both), Engine::Codex);
        let only_fx = |e: Engine| e == Engine::Fx;
        assert_eq!(choose(None, only_fx), Engine::Fx);
        assert_eq!(choose(Some(Engine::Fx), both), Engine::Fx);
    }

    #[test]
    fn spawn_error_blames_a_missing_workspace() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let gone = Path::new("/nonexistent/vicuna-ws");
        let msg = spawn_error("codex", Path::new("/usr/bin/codex"), gone, &e);
        assert!(msg.starts_with("workspace folder not found: /nonexistent/vicuna-ws"), "{msg}");
        let here = std::env::temp_dir();
        assert!(spawn_error("codex", Path::new("/x/codex"), &here, &e).starts_with("cannot start codex (/x/codex):"));
    }

    #[test]
    fn engine_choice_persists() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let p = base.join(format!("vicuna-engine-{n}")).join("work-engine.json");
        assert_eq!(read_engine_from(&p), None);
        write_engine_to(&p, Engine::Claude).unwrap();
        assert_eq!(read_engine_from(&p), Some(Engine::Claude));
        write_engine_to(&p, Engine::Opencode).unwrap();
        assert_eq!(read_engine_from(&p), Some(Engine::Opencode));
        // approval mode lives in the same file and survives engine switches
        assert!(!read_ask_from(&p));
        write_ask_to(&p, true).unwrap();
        write_engine_to(&p, Engine::Claude).unwrap();
        assert!(read_ask_from(&p));
        assert_eq!(read_engine_from(&p), Some(Engine::Claude));
        write_ask_to(&p, false).unwrap();
        assert!(!read_ask_from(&p));
    }
}
