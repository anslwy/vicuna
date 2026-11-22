//! Vicuna-managed Agent Skills (`~/vicuna/skills/<slug>/SKILL.md`).
//!
//! Each skill lives in its own folder under `~/vicuna/skills/<slug>/` with a
//! `SKILL.md` file (YAML frontmatter `name:` + `description:` followed by
//! Markdown instructions). Enabled/disabled state is stored in
//! Work session metadata. New sessions snapshot the defaults in
//! `~/vicuna/config/skills.json`; unlisted skills are enabled.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// Max `SKILL.md` size accepted by `save_skill` (matches `work_write_file_logic`).
pub const MAX_SKILL_BYTES: usize = 524_288;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSummary {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDetail {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub path: String,
    pub content: String,
}

pub type EnabledMap = BTreeMap<String, bool>;

pub(crate) fn selection(extra: &serde_json::Value) -> Option<EnabledMap> {
    serde_json::from_value(extra["skillsEnabled"].clone()).ok()
}

fn is_enabled(selection: &EnabledMap, slug: &str) -> bool {
    selection.get(slug).copied().unwrap_or(true)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsConfig {
    #[serde(default)]
    enabled: EnabledMap,
}

/// Root folder for Vicuna-managed skills: `~/vicuna/skills`.
pub fn skills_dir() -> Result<PathBuf, String> {
    let home = crate::opencode::home_dir().ok_or_else(|| "cannot locate home directory".to_string())?;
    Ok(home.join("vicuna").join("skills"))
}

/// Config file storing defaults for new Work sessions: `~/vicuna/config/skills.json`.
pub fn skills_config_path() -> Result<PathBuf, String> {
    let home = crate::opencode::home_dir().ok_or_else(|| "cannot locate home directory".to_string())?;
    Ok(home.join("vicuna").join("config").join("skills.json"))
}

/// Strict slug validation: lowercase `[a-z0-9][a-z0-9_-]{0,63}`, no `..`, no `/`.
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() {
        return Err("skill slug cannot be empty".to_string());
    }
    if slug.len() > 64 {
        return Err("skill slug must be at most 64 characters".to_string());
    }
    let bytes = slug.as_bytes();
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return Err("skill slug must start with a lowercase letter or digit (a-z, 0-9)".to_string());
    }
    for &b in &bytes[1..] {
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            return Err(
                "skill slug may only contain lowercase letters, digits, hyphens, and underscores"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn canonicalize_lenient(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(mut base) => {
                for part in tail.iter().rev() {
                    base.push(part);
                }
                return Some(base);
            }
            Err(_) => {
                let name = current.file_name()?.to_os_string();
                tail.push(name);
                if !current.pop() {
                    return None;
                }
            }
        }
    }
}

/// Resolve an absolute path if and only if it lies strictly under `~/vicuna/skills/`.
/// Used by the file editor (`work_read_file_logic` / `work_write_file_logic`) so
/// users can edit `SKILL.md` in CodeMirror without loosening workspace sandboxing
/// for any other path.
pub fn resolve_skills_absolute(abs: &Path) -> Option<PathBuf> {
    let root = skills_dir().ok()?;
    resolve_skills_absolute_in(&root, abs)
}

pub(crate) fn resolve_skills_absolute_in(skills_root: &Path, abs: &Path) -> Option<PathBuf> {
    if !abs.is_absolute() {
        return None;
    }
    if abs.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let canon_root = canonicalize_lenient(skills_root)?;
    let resolved = canonicalize_lenient(abs)?;
    if resolved.starts_with(&canon_root) && resolved != canon_root {
        Some(resolved)
    } else {
        None
    }
}

/// True when `path` is `~/vicuna/skills` or inside it.
pub fn path_is_under_skills(path: &Path) -> bool {
    let Ok(root) = skills_dir() else {
        return false;
    };
    let Some(canon_root) = canonicalize_lenient(&root) else {
        return false;
    };
    let resolved = canonicalize_lenient(path).unwrap_or_else(|| path.to_path_buf());
    resolved.starts_with(&canon_root) || path.starts_with(&root)
}

fn read_config(path: &Path) -> SkillsConfig {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return SkillsConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_config(path: &Path, cfg: &SkillsConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create config dir: {e}"))?;
    }
    let body = serde_json::to_string_pretty(cfg)
        .map_err(|e| format!("cannot serialize skills config: {e}"))?;
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).map_err(|e| format!("cannot prepare skills config: {e}"))?;
    let temp = path.with_extension(format!("{:016x}.tmp", u64::from_ne_bytes(nonce)));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        writeln!(file, "{body}")?;
        drop(file);
        std::fs::rename(&temp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot write skills config: {e}")
    })
}

pub(crate) fn defaults_in(path: &Path) -> EnabledMap {
    read_config(path).enabled
}

pub(crate) fn defaults() -> EnabledMap {
    skills_config_path().map(|path| defaults_in(&path)).unwrap_or_default()
}

pub(crate) fn set_defaults_in(root: &Path, path: &Path, enabled: &EnabledMap) -> Result<(), String> {
    let enabled = list_skills_in(root, enabled)?
        .into_iter()
        .map(|skill| (skill.slug, skill.enabled))
        .collect();
    write_config(path, &SkillsConfig { enabled })
}

pub(crate) fn set_defaults(enabled: &EnabledMap) -> Result<(), String> {
    set_defaults_in(&skills_dir()?, &skills_config_path()?, enabled)
}

fn unquote_yaml_scalar(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        let inner = &s[1..s.len() - 1];
        if s.starts_with('"') {
            inner
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
                .replace("\\n", " ")
        } else {
            inner.replace("''", "'")
        }
    } else {
        s.to_string()
    }
}

/// Extract `name` and `description` from leading YAML frontmatter (`--- ... ---`).
pub fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let stripped = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = stripped.lines();
    let Some(first) = lines.next() else {
        return (None, None);
    };
    if first.trim() != "---" {
        return (None, None);
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut current_key: Option<&str> = None;
    let mut folded_buf: Vec<String> = Vec::new();

    let flush_folded = |key: &mut Option<&str>, buf: &mut Vec<String>, name: &mut Option<String>, desc: &mut Option<String>| {
        if let Some(k) = key.take() {
            let joined = buf.join(" ").trim().to_string();
            buf.clear();
            if !joined.is_empty() {
                match k {
                    "name" => *name = Some(joined),
                    "description" => *desc = Some(joined),
                    _ => {}
                }
            }
        }
    };

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            flush_folded(&mut current_key, &mut folded_buf, &mut name, &mut description);
            return (name, description);
        }
        if current_key.is_some() && (line.starts_with(' ') || line.starts_with('\t')) {
            if !trimmed.is_empty() {
                folded_buf.push(trimmed.to_string());
            }
            continue;
        }
        flush_folded(&mut current_key, &mut folded_buf, &mut name, &mut description);

        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            let val = v.trim();
            if key == "name" || key == "description" {
                if val.is_empty() || val == ">" || val == "|" || val == ">-" || val == "|-" {
                    current_key = Some(if key == "name" { "name" } else { "description" });
                } else {
                    let parsed = unquote_yaml_scalar(val);
                    if !parsed.is_empty() {
                        if key == "name" {
                            name = Some(parsed);
                        } else {
                            description = Some(parsed);
                        }
                    }
                }
            }
        }
    }
    // Unclosed frontmatter — ignore rather than guessing.
    (None, None)
}

fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

fn skill_file_path(skills_root: &Path, slug: &str) -> Result<PathBuf, String> {
    validate_slug(slug)?;
    Ok(skills_root.join(slug).join("SKILL.md"))
}

fn verify_inside_root(skills_root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let canon_root = std::fs::canonicalize(skills_root)
        .map_err(|e| format!("cannot access skills directory: {e}"))?;
    let canon_target = std::fs::canonicalize(candidate)
        .map_err(|e| format!("skill not found: {e}"))?;
    if !canon_target.starts_with(&canon_root) || canon_target == canon_root {
        return Err("skill path escapes skills directory".to_string());
    }
    Ok(canon_target)
}

fn summary_from_content(slug: &str, path: &Path, content: &str, enabled: bool) -> SkillSummary {
    let (fm_name, fm_desc) = parse_frontmatter(content);
    SkillSummary {
        slug: slug.to_string(),
        name: fm_name.unwrap_or_else(|| slug.to_string()),
        description: fm_desc.unwrap_or_default(),
        enabled,
        path: display_path(path),
    }
}

/// List all valid skills in `~/vicuna/skills/`.
pub fn list_skills() -> Result<Vec<SkillSummary>, String> {
    list_skills_in(&skills_dir()?, &defaults())
}

pub fn list_skills_in(root: &Path, selection: &EnabledMap) -> Result<Vec<SkillSummary>, String> {
    find_skills(root, selection, None)
}

pub(crate) fn search_skills_in(
    root: &Path,
    selection: &EnabledMap,
    query: &str,
) -> Result<Vec<SkillSummary>, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Err("manage_skills action 'search' requires a non-empty query".into());
    }
    find_skills(root, selection, Some(&query))
}

fn find_skills(
    skills_root: &Path,
    selection: &EnabledMap,
    query: Option<&str>,
) -> Result<Vec<SkillSummary>, String> {
    if !skills_root.exists() {
        return Ok(Vec::new());
    }
    let canon_root = std::fs::canonicalize(skills_root)
        .map_err(|e| format!("cannot read skills directory: {e}"))?;
    let entries = std::fs::read_dir(&canon_root)
        .map_err(|e| format!("cannot list skills directory: {e}"))?;

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(fname) = entry.file_name().into_string() else {
            continue;
        };
        if validate_slug(&fname).is_err() {
            continue;
        }
        let skill_md = entry.path().join("SKILL.md");
        let Ok(canon_file) = std::fs::canonicalize(&skill_md) else {
            continue;
        };
        if !canon_file.starts_with(&canon_root) || !canon_file.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&canon_file) else {
            continue;
        };
        let summary = summary_from_content(&fname, &canon_file, &content, is_enabled(selection, &fname));
        if query.is_none_or(|query| {
            [&summary.slug, &summary.name, &summary.description, &content]
                .iter()
                .any(|field| field.to_lowercase().contains(query))
        }) {
            out.push(summary);
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Read a single skill's `SKILL.md` content and metadata.
pub fn read_skill(slug: &str) -> Result<SkillDetail, String> {
    read_skill_in(&skills_dir()?, &defaults(), slug)
}

pub fn read_skill_in(
    skills_root: &Path,
    selection: &EnabledMap,
    slug: &str,
) -> Result<SkillDetail, String> {
    let file = skill_file_path(skills_root, slug)?;
    let canon_file = verify_inside_root(skills_root, &file)?;
    if !canon_file.is_file() {
        return Err(format!("skill '{slug}' has no SKILL.md"));
    }
    let content = std::fs::read_to_string(&canon_file)
        .map_err(|e| format!("cannot read skill '{slug}': {e}"))?;
    let summary = summary_from_content(slug, &canon_file, &content, is_enabled(selection, slug));
    Ok(SkillDetail {
        slug: summary.slug,
        name: summary.name,
        description: summary.description,
        enabled: summary.enabled,
        path: summary.path,
        content,
    })
}

/// Create or overwrite a shared skill definition.
pub fn save_skill(slug: &str, content: &str) -> Result<SkillSummary, String> {
    save_skill_in(&skills_dir()?, &defaults(), slug, content)
}

pub fn save_skill_in(
    skills_root: &Path,
    selection: &EnabledMap,
    slug: &str,
    content: &str,
) -> Result<SkillSummary, String> {
    validate_slug(slug)?;
    if content.len() > MAX_SKILL_BYTES {
        return Err("skill content too large (max ~512KB)".to_string());
    }
    std::fs::create_dir_all(skills_root)
        .map_err(|e| format!("cannot create skills directory: {e}"))?;
    let dir = skills_root.join(slug);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create skill folder '{slug}': {e}"))?;
    // Ensure folder itself isn't a symlink escaping skills_root.
    let canon_dir = verify_inside_root(skills_root, &dir)?;
    let file = canon_dir.join("SKILL.md");
    if file.symlink_metadata().is_ok() {
        verify_inside_root(skills_root, &file)?;
    }
    std::fs::write(&file, content.as_bytes())
        .map_err(|e| format!("cannot write skill '{slug}': {e}"))?;
    let canon_file = verify_inside_root(skills_root, &file)?;

    Ok(summary_from_content(slug, &canon_file, content, is_enabled(selection, slug)))
}

/// Remove a skill directory and its config entry cleanly.
pub fn remove_skill(slug: &str) -> Result<(), String> {
    remove_skill_in(&skills_dir()?, &skills_config_path()?, slug)
}

pub fn remove_skill_in(skills_root: &Path, config_path: &Path, slug: &str) -> Result<(), String> {
    validate_slug(slug)?;
    let dir = skills_root.join(slug);
    if !dir.exists() {
        return Err(format!("skill '{slug}' not found"));
    }
    verify_inside_root(skills_root, &dir)?;
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("cannot remove skill '{slug}': {e}"))?;
    let mut cfg = read_config(config_path);
    if cfg.enabled.remove(slug).is_some() {
        write_config(config_path, &cfg)?;
    }
    Ok(())
}

/// Build the compact `<vicuna_skills>` prompt block injected into every Work
/// harness's `system_prompt`. Returns an empty string when no skills are enabled.
pub fn format_skills_prompt(selection: &EnabledMap) -> String {
    let Ok(root) = skills_dir() else {
        return String::new();
    };
    format_skills_prompt_in(&root, selection)
}

pub fn format_skills_prompt_in(skills_root: &Path, selection: &EnabledMap) -> String {
    let Ok(all) = list_skills_in(skills_root, selection) else {
        return String::new();
    };
    let enabled: Vec<SkillSummary> = all.into_iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n<vicuna_skills>\n\
         Available Vicuna skills (read the skill's SKILL.md when relevant to the user's task; \
         reading files under ~/vicuna/skills/ is permitted even though it is outside the workspace):\n",
    );
    for s in enabled {
        if s.description.is_empty() {
            out.push_str(&format!("- {} (`{}`) — {}\n", s.name, s.slug, s.path));
        } else {
            out.push_str(&format!(
                "- {} (`{}`): {} — {}\n",
                s.name, s.slug, s.description, s.path
            ));
        }
    }
    out.push_str("</vicuna_skills>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "vicuna-skills-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("skills");
        let cfg = base.join("config").join("skills.json");
        (base, root, cfg)
    }

    #[test]
    fn slug_validation_rules() {
        assert!(validate_slug("a").is_ok());
        assert!(validate_slug("code-review").is_ok());
        assert!(validate_slug("deploy_v2").is_ok());
        assert!(validate_slug("0-skill").is_ok());
        assert!(validate_slug(&"a".repeat(64)).is_ok());

        assert!(validate_slug("").is_err());
        assert!(validate_slug(&"a".repeat(65)).is_err());
        assert!(validate_slug("-leading").is_err());
        assert!(validate_slug("_leading").is_err());
        assert!(validate_slug("Upper").is_err());
        assert!(validate_slug("../escape").is_err());
        assert!(validate_slug("a/b").is_err());
        assert!(validate_slug("a\\b").is_err());
        assert!(validate_slug("has space").is_err());
        assert!(validate_slug("dot.name").is_err());
    }

    #[test]
    fn frontmatter_parsing() {
        let doc = "---\nname: Code Review\ndescription: \"Review PRs carefully\"\n---\n\n# Body\n";
        assert_eq!(
            parse_frontmatter(doc),
            (Some("Code Review".into()), Some("Review PRs carefully".into()))
        );

        let folded = "---\nname: Deploy\ndescription: >\n  Deploy the app\n  safely.\n---\nBody";
        assert_eq!(
            parse_frontmatter(folded),
            (Some("Deploy".into()), Some("Deploy the app safely.".into()))
        );

        let no_fm = "# Just markdown\nNo frontmatter here.";
        assert_eq!(parse_frontmatter(no_fm), (None, None));
    }

    #[test]
    fn crud_lifecycle_and_prompt_formatting() {
        let (base, root, cfg) = temp_env("crud");
        let mut selection = EnabledMap::new();

        // Empty initially -> empty list and empty prompt
        assert!(list_skills_in(&root, &selection).unwrap().is_empty());
        assert_eq!(format_skills_prompt_in(&root, &selection), "");

        // Save new skill (defaults to enabled)
        let md = "---\nname: PR Review\ndescription: Thorough code review checklist\n---\n\n# Steps\n1. Read diff\n";
        let created = save_skill_in(&root, &selection, "pr-review", md).unwrap();
        assert_eq!(created.slug, "pr-review");
        assert_eq!(created.name, "PR Review");
        assert_eq!(created.description, "Thorough code review checklist");
        assert!(created.enabled);
        assert!(created.path.ends_with("pr-review/SKILL.md") || created.path.ends_with(r"pr-review\SKILL.md"));

        // Read back
        let detail = read_skill_in(&root, &selection, "pr-review").unwrap();
        assert_eq!(detail.content, md);
        assert!(detail.enabled);

        // Prompt includes enabled skill and ~/vicuna/skills/ note
        let prompt = format_skills_prompt_in(&root, &selection);
        assert!(prompt.contains("<vicuna_skills>"));
        assert!(prompt.contains("PR Review (`pr-review`): Thorough code review checklist"));
        assert!(prompt.contains("~/vicuna/skills/"));

        // A session switch leaves another session and the defaults alone.
        let old_session = defaults_in(&cfg);
        selection.insert("pr-review".into(), false);
        assert_eq!(format_skills_prompt_in(&root, &selection), "");
        assert_eq!(format_skills_prompt_in(&root, &old_session), prompt);
        assert!(defaults_in(&cfg).is_empty());
        assert_ne!(crate::acp::skills_scope(""), crate::acp::skills_scope(&prompt));

        set_defaults_in(&root, &cfg, &selection).unwrap();
        let new_session = defaults_in(&cfg);
        assert_eq!(new_session, selection);
        assert_eq!(format_skills_prompt_in(&root, &new_session), "");
        assert_eq!(format_skills_prompt_in(&root, &old_session), prompt);
        save_skill_in(&root, &new_session, "another", "# New").unwrap();
        assert!(list_skills_in(&root, &new_session).unwrap()[0].enabled);

        // Remove cleans directory and config
        remove_skill_in(&root, &cfg, "pr-review").unwrap();
        assert!(defaults_in(&cfg).is_empty());
        assert!(read_skill_in(&root, &selection, "pr-review").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_skills_absolute_blocks_escapes() {
        let (base, root, _cfg) = temp_env("abs");
        std::fs::create_dir_all(root.join("my-skill")).unwrap();
        let ok_file = root.join("my-skill").join("SKILL.md");
        std::fs::write(&ok_file, "hi").unwrap();

        assert!(resolve_skills_absolute_in(&root, &ok_file).is_some());
        assert!(resolve_skills_absolute_in(&root, &root.join("my-skill/../etc/passwd")).is_none());
        assert!(resolve_skills_absolute_in(&root, &root).is_none());
        assert!(resolve_skills_absolute_in(&root, Path::new("/etc/hosts")).is_none());

        let _ = std::fs::remove_dir_all(&base);
    }
}
