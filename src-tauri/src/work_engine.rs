// Work engines: which external agent CLI a Work turn runs on.
//
// opencode, Antigravity (agy) and fx speak ACP (acp.rs), Claude Code stream-json (claude_cli.rs),
// Codex its app-server JSON-RPC (codex_cli.rs) and pi its RPC mode (pi.rs); this module owns the choice
// between them (persisted in ~/vicuna/config/work-engine.json), the combined
// detection the UI renders (engine picker when more than one is installed),
// and per-engine model lists + saved model picks.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
#[cfg(feature = "desktop")]
use tauri::State;

use crate::cli_accounts::Harness;
use crate::opencode::{home_dir, OpencodeModels, WorkEngineInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Opencode,
    Claude,
    Codex,
    Agy,
    Fx,
    Pi,
    /// A harness a plugin adds (`plugin:<plugin-id>/<harness-id>`); holds the
    /// interned `<plugin-id>/<harness-id>` key. Driven over ACP.
    Plugin(&'static str),
    /// An extra Antigravity account (`agy@<account-id>`, agy_accounts.rs):
    /// agy with its own HOME, so its own login, models and quota.
    AgyAccount(&'static str),
    CliAccount(Harness, &'static str),
}

impl Engine {
    pub const ALL: [Engine; 6] = [Engine::Opencode, Engine::Claude, Engine::Codex, Engine::Agy, Engine::Fx, Engine::Pi];

    /// Built-in engines plus every installed plugin harness.
    pub fn all() -> Vec<Engine> {
        let mut v = Vec::new();
        for base in Engine::ALL {
            v.push(base);
            if base == Engine::Agy {
                v.extend(crate::agy_accounts::list().into_iter()
                    .map(|a| Engine::AgyAccount(crate::plugin_harness::intern(&a.id))));
            } else if let Some(harness) = Harness::parse(base.id()) {
                v.extend(harness.list().into_iter()
                    .map(|a| Engine::CliAccount(harness, crate::plugin_harness::intern(&a.id))));
            }
        }
        v.extend(crate::plugin_harness::all_keys().into_iter().map(Engine::Plugin));
        v
    }

    /// The CLI an engine runs: every Antigravity account is `agy`.
    pub fn base(self) -> Engine {
        match self {
            Engine::AgyAccount(_) => Engine::Agy,
            Engine::CliAccount(h, _) => h.base(),
            e => e,
        }
    }

    /// The extra Antigravity account this engine runs on, if any.
    pub fn agy_account(self) -> Option<&'static str> {
        match self {
            Engine::AgyAccount(a) => Some(a),
            _ => None,
        }
    }

    pub fn changes_cli_account(self, previous_model: Option<&str>) -> bool {
        previous_model.is_some_and(|model| {
            let previous = model.split('/').next().unwrap_or_default();
            previous != self.id() && (self.cli_account().is_some()
                || previous.split_once('@').is_some_and(|(base, _)| Harness::parse(base).is_some()))
        })
    }

    pub fn cli_account(self) -> Option<(Harness, &'static str)> {
        match self {
            Engine::CliAccount(h, id) => Some((h, id)),
            _ => None,
        }
    }

    pub fn is_plugin(self) -> bool {
        matches!(self, Engine::Plugin(_))
    }

    pub fn id(self) -> &'static str {
        match self {
            Engine::Plugin(k) => crate::plugin_harness::intern(&format!("{}{k}", crate::plugin_harness::ENGINE_PREFIX)),
            Engine::AgyAccount(a) => crate::plugin_harness::intern(&format!("{}{a}", crate::agy_accounts::ENGINE_PREFIX)),
            Engine::CliAccount(h, id) => crate::plugin_harness::intern(&format!("{}@{id}", h.base().id())),
            Engine::Opencode => "opencode",
            Engine::Claude => "claude",
            Engine::Codex => "codex",
            Engine::Agy => "agy",
            Engine::Fx => "fx",
            Engine::Pi => "pi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::Opencode => "OpenCode",
            Engine::Claude => "Claude Code",
            Engine::Codex => "Codex",
            Engine::Agy => "Antigravity",
            Engine::Fx => "fx",
            Engine::Pi => "pi",
            Engine::Plugin(k) => crate::plugin_harness::label(k),
            Engine::CliAccount(h, id) => {
                let name = h.find(id).map(|a| a.label).unwrap_or_else(|| id.to_owned());
                crate::plugin_harness::intern(&format!("{} · {name}", h.base().label()))
            }
            Engine::AgyAccount(a) => {
                let name = crate::agy_accounts::find(a).map(|x| x.label).unwrap_or_else(|| a.to_string());
                crate::plugin_harness::intern(&format!("Antigravity · {name}"))
            }
        }
    }

    pub fn parse(s: &str) -> Option<Engine> {
        match s.trim().to_ascii_lowercase().as_str() {
            "opencode" => Some(Engine::Opencode),
            "claude" | "claude-code" | "claude_code" => Some(Engine::Claude),
            "codex" | "codex-cli" => Some(Engine::Codex),
            "agy" | "antigravity" => Some(Engine::Agy),
            "fx" => Some(Engine::Fx),
            "pi" | "pi-coding-agent" => Some(Engine::Pi),
            low if low.starts_with(crate::agy_accounts::ENGINE_PREFIX) => {
                let id = &low[crate::agy_accounts::ENGINE_PREFIX.len()..];
                crate::agy_accounts::find(id).map(|a| Engine::AgyAccount(crate::plugin_harness::intern(&a.id)))
            }
            low if low.split_once('@').is_some_and(|(base, _)| Harness::parse(base).is_some()) => {
                let (base, id) = low.split_once('@')?;
                let harness = Harness::parse(base)?;
                harness.find(id).map(|a| Engine::CliAccount(harness, crate::plugin_harness::intern(&a.id)))
            }
            _ => crate::plugin_harness::parse_engine(s).map(Engine::Plugin),
        }
    }

    /// Cheap presence check (stats only).
    pub fn is_present(self) -> bool {
        match self {
            Engine::Opencode => crate::opencode::find_opencode().is_some(),
            Engine::Claude => crate::claude::find_claude().is_some(),
            Engine::Codex => crate::codex::find_codex().is_some(),
            Engine::Agy => crate::agy::find_agy().is_some(),
            Engine::Fx => crate::fx::find_fx().is_some(),
            Engine::Pi => crate::pi::find_pi().is_some(),
            Engine::Plugin(k) => crate::plugin_harness::unavailable(k).is_none(),
            Engine::AgyAccount(a) => crate::agy::find_agy().is_some() && crate::agy_accounts::find(a).is_some(),
            Engine::CliAccount(h, id) => h.base().is_present() && h.find(id).is_some(),
        }
    }

    pub fn missing_message(self) -> String {
        match self {
            Engine::Opencode => crate::acp::not_found_message(),
            Engine::Claude => crate::claude::missing_message(),
            Engine::Codex => crate::codex::missing_message(),
            Engine::Agy => crate::agy::missing_message(),
            Engine::Fx => crate::fx::missing_message(),
            Engine::Pi => crate::pi::missing_message(),
            Engine::Plugin(k) => crate::plugin_harness::unavailable(k).unwrap_or_default(),
            Engine::AgyAccount(a) if crate::agy_accounts::find(a).is_none() => {
                format!("The Antigravity account \"{a}\" was removed. Pick another harness for this session.")
            }
            Engine::AgyAccount(_) => crate::agy::missing_message(),
            Engine::CliAccount(h, id) if h.find(id).is_none() => {
                format!("The {} account \"{id}\" was removed. Pick another account for this session.", h.base().label())
            }
            Engine::CliAccount(h, _) => h.base().missing_message(),
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
            Engine::CliAccount(..) => format!("{}/{model}", self.id()),
            Engine::Agy | Engine::AgyAccount(_) => format!("agy/{model}"),
            Engine::Fx => format!("fx/{model}"),
            Engine::Pi => format!("pi/{model}"),
            Engine::Plugin(k) => format!("{}/{model}", crate::plugin_harness::split_key(k).1),
        }
    }

    pub fn saved_model(self) -> Option<String> {
        match self {
            Engine::Opencode => crate::opencode::saved_model(),
            Engine::Claude => crate::claude::saved_model(),
            Engine::Codex => crate::codex::saved_model(),
            Engine::Agy => crate::agy::saved_model(),
            Engine::Fx => crate::fx::saved_model(),
            Engine::Pi => crate::pi::saved_model(),
            Engine::Plugin(k) => crate::plugin_harness::saved_model(k),
            Engine::CliAccount(h, id) => read_model_from_opt(h.model_prefs(id)),
            Engine::AgyAccount(a) => read_model_from_opt(crate::agy_accounts::model_prefs_path(a)),
        }
    }

    pub fn save_model(self, model: Option<&str>) -> Result<(), String> {
        match self {
            Engine::Opencode => crate::opencode::save_model(model),
            Engine::Claude => crate::claude::save_model(model),
            Engine::Codex => crate::codex::save_model(model),
            Engine::Agy => crate::agy::save_model(model),
            Engine::Fx => crate::fx::save_model(model),
            Engine::Pi => crate::pi::save_model(model),
            Engine::Plugin(k) => crate::plugin_harness::save_model(k, model),
            Engine::CliAccount(h, id) => {
                let p = h.model_prefs(id).ok_or("cannot locate home folder")?;
                crate::opencode::write_model_to(&p, model)
            }
            Engine::AgyAccount(a) => {
                let p = crate::agy_accounts::model_prefs_path(a).ok_or("cannot locate home folder")?;
                crate::opencode::write_model_to(&p, model)
            }
        }
    }

    async fn info(self) -> WorkEngineInfo {
        match self {
            Engine::Opencode => crate::opencode::engine_info().await,
            Engine::Claude => crate::claude::engine_info().await,
            Engine::Codex => crate::codex::engine_info().await,
            Engine::Agy => crate::agy::engine_info().await,
            Engine::Fx => crate::fx::engine_info().await,
            Engine::Pi => crate::pi::engine_info().await,
            Engine::Plugin(k) => crate::plugin_harness::engine_info(k),
            Engine::AgyAccount(a) => crate::agy::account_engine_info(a, self).await,
            Engine::CliAccount(..) => unreachable!("account info uses its base engine info"),
        }
    }
}

fn read_model_from_opt(p: Option<PathBuf>) -> Option<String> {
    crate::opencode::read_model_from(&p?)
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

fn saved_engine_raw() -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(prefs_path()?).ok()?).ok()?;
    Some(v.get("engine")?.as_str()?.to_string())
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

/// A turn's engine when the caller names one: it must exist (no fallback).
pub fn requested_engine(requested: Option<&str>) -> Result<Engine, String> {
    match requested.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(current_engine()),
        Some(n) => Engine::parse(n).ok_or_else(|| format!(
            "This session runs on the harness \"{n}\", which is not installed any more. Pick another harness for this session in the model picker."
        )),
    }
}

/// The harness + model a session's turn runs on. A session that stored its
/// own harness (`chats.extra.engine`) always runs on exactly that one: an
/// unknown or uninstalled harness is an error, never a silent fallback to the
/// global pick. Sessions without one use the global pick (`fallback`).
pub fn resolve_session_engine(
    stored: Option<&str>,
    stored_model: Option<&str>,
    parse: impl Fn(&str) -> Option<Engine>,
    fallback: impl FnOnce() -> (Engine, Option<String>),
) -> Result<(Engine, Option<String>), String> {
    match stored.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(fallback()),
        Some(name) => match parse(name) {
            Some(e) => Ok((e, stored_model.map(str::to_string).filter(|m| !m.is_empty()))),
            None => Err(format!(
                "This session runs on the harness \"{name}\", which is not installed any more. Pick another harness for this session in the model picker."
            )),
        },
    }
}

/// [`resolve_session_engine`] with the real parser and global pick.
pub fn session_engine(stored: Option<&str>, stored_model: Option<&str>) -> Result<(Engine, Option<String>), String> {
    resolve_session_engine(stored, stored_model, Engine::parse, || {
        let e = current_engine();
        (e, e.saved_model())
    })
}

#[cfg(test)]
mod session_engine_tests {
    use super::*;
    fn parse(s: &str) -> Option<Engine> {
        match s { "claude" => Some(Engine::Claude), "agy" => Some(Engine::Agy), _ => None }
    }
    #[test]
    fn stored_harness_wins_and_missing_one_errors() {
        let global = || (Engine::Agy, Some("gemini".to_string()));
        assert_eq!(resolve_session_engine(Some("claude"), Some("opus"), parse, global).unwrap(), (Engine::Claude, Some("opus".into())));
        assert_eq!(resolve_session_engine(Some("claude"), Some(""), parse, global).unwrap(), (Engine::Claude, None));
        assert_eq!(resolve_session_engine(None, Some("opus"), parse, global).unwrap(), (Engine::Agy, Some("gemini".into())));
        assert_eq!(resolve_session_engine(Some(" "), None, parse, global).unwrap().0, Engine::Agy);
        let err = resolve_session_engine(Some("plugin:gone"), None, parse, global).unwrap_err();
        assert!(err.contains("plugin:gone") && err.contains("not installed"), "{err}");
    }
}

/// Detection for the UI: the current engine's info plus a summary of all.
pub async fn engines_info() -> WorkEngineInfo {
    engines_info_for(None).await
}

async fn engines_info_for(requested: Option<Engine>) -> WorkEngineInfo {
    let engines = Engine::all();
    let mut bases = Vec::new();
    for e in engines.iter().copied().filter(|e| e.cli_account().is_none()) {
        bases.push((e, e.info().await));
    }
    let infos = futures_util::future::join_all(engines.into_iter().map(|e| {
        let bases = &bases;
        async move {
            if let Some((h, id)) = e.cli_account() {
                let base = &bases.iter().find(|(engine, _)| *engine == h.base()).expect("registered harness").1;
                let (info, account) = h.engine_info(id, base).await;
                (e, info, account)
            } else {
                let info = bases.iter().find(|(engine, _)| *engine == e).expect("installed harness info").1.clone();
                (e, info, None)
            }
        }
    })).await;
    let saved = requested.or_else(saved_engine);
    let chosen = choose(saved, |e| infos.iter().any(|(x, i, _)| *x == e && i.installed));
    let summaries = infos.iter().map(|(_, info, account)| {
        let mut summary = crate::claude::summary(info);
        if account.is_some() {
            summary.account = account.clone();
        }
        summary
    }).collect();
    let mut info = infos
        .into_iter()
        .find(|(e, _, _)| *e == chosen)
        .map(|(_, i, _)| i)
        .expect("every engine has info");
    info.engines = summaries;
    info.ask_permission = approval_ask();
    // Explain a removed saved account or an unavailable saved plugin.
    if saved.is_none() {
        if let Some(raw) = saved_engine_raw().filter(|r| r.starts_with(crate::agy_accounts::ENGINE_PREFIX)) {
            info.warning = Some(format!("The Antigravity account \"{}\" was removed. Work is using {} instead.", &raw[crate::agy_accounts::ENGINE_PREFIX.len()..], chosen.label()));
        } else if let Some((h, id)) = saved_engine_raw().as_deref().and_then(|s| s.split_once('@')).and_then(|(base, id)| Some((Harness::parse(base)?, id.to_owned()))) {
            info.warning = Some(format!("The {} account \"{id}\" was removed. Work is using {} instead.", h.base().label(), chosen.label()));
        }
    }
    if let Some(Engine::Plugin(k)) = saved.filter(|s| *s != chosen) {
        if let Some(why) = crate::plugin_harness::unavailable(k) {
            let why = why.trim_end_matches(" Pick another harness for this session.");
            info.warning = Some(format!("{why} Work is using {} instead.", chosen.label()));
        }
    }
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
        Engine::Claude | Engine::CliAccount(Harness::Claude, _) => Ok(OpencodeModels {
            models: crate::claude_cli::list_models(engine).await?,
            current: engine.saved_model(),
        }),
        Engine::Codex | Engine::CliAccount(Harness::Codex, _) => Ok(OpencodeModels {
            models: crate::codex_cli::list_models(engine).await?,
            current: engine.saved_model(),
        }),
        Engine::Agy => Ok(OpencodeModels {
            models: crate::agy::list_models().await?,
            current: crate::agy::saved_model(),
        }),
        Engine::Fx => Ok(OpencodeModels {
            models: acp.fx_models().await?,
            current: crate::fx::saved_model(),
        }),
        Engine::Pi => Ok(OpencodeModels {
            models: crate::pi::list_models().await?,
            current: crate::pi::saved_model(),
        }),
        Engine::AgyAccount(a) => Ok(OpencodeModels {
            models: crate::agy::list_models_for(Some(a)).await?,
            current: engine.saved_model(),
        }),
        Engine::Plugin(k) => {
            let mut models = crate::plugin_harness::manifest_models(k);
            if models.is_empty() && engine.is_present() {
                // none declared: whatever the agent reports over ACP
                models = acp.plugin_models(engine).await.unwrap_or_default();
            }
            Ok(OpencodeModels { models, current: crate::plugin_harness::saved_model(k) })
        }
    }
}

// ---------- shared logic (Tauri commands + headless bridge) ----------

pub async fn select_engine(engine: &str) -> Result<WorkEngineInfo, String> {
    let e = Engine::parse(engine).ok_or_else(|| format!("unknown Work engine: {engine}"))?;
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_engine_to(&path, e)?;
    Ok(engines_info_for(Some(e)).await)
}

pub async fn set_approval(ask: bool) -> Result<WorkEngineInfo, String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_ask_to(&path, ask)?;
    Ok(engines_info().await)
}

pub fn set_model(engine: Option<&str>, model: Option<&str>) -> Result<(), String> {
    requested_engine(engine)?.save_model(model)
}

// ---------- Tauri commands ----------

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_engine_select(engine: String) -> Result<WorkEngineInfo, String> {
    select_engine(&engine).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_models(engine: Option<String>, acp: State<'_, crate::acp::AcpState>) -> Result<OpencodeModels, String> {
    list_models(requested_engine(engine.as_deref())?, &acp).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_set_approval(ask: bool) -> Result<WorkEngineInfo, String> {
    set_approval(ask).await
}

#[cfg(feature = "desktop")]
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
        assert_eq!(Engine::parse("Antigravity"), Some(Engine::Agy));
        assert_eq!(Engine::Agy.reply_label("gemini-3.8-flash-medium"), "agy/gemini-3.8-flash-medium");
        assert_eq!(Engine::Agy.label(), "Antigravity");
        assert_eq!(Engine::parse("FX"), Some(Engine::Fx));
        assert_eq!(Engine::Fx.reply_label("codex/gpt-6-luna"), "fx/codex/gpt-6-luna");
        assert_eq!(Engine::Fx.label(), "fx");
        assert_eq!(Engine::parse("PI"), Some(Engine::Pi));
        assert_eq!(Engine::Pi.reply_label("ollama/deepseek-v4.1-flash:cloud"), "pi/ollama/deepseek-v4.1-flash:cloud");
        assert_eq!(Engine::Pi.label(), "pi");
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
