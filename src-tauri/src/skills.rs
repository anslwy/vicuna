//! Vicuna-managed Agent Skills (`~/vicuna/skills/<slug>/SKILL.md`).
//!
//! Each skill lives in its own folder under `~/vicuna/skills/<slug>/` with a
//! `SKILL.md` file (YAML frontmatter `name:` + `description:` followed by
//! Markdown instructions). Enabled/disabled state is stored in
//! `~/vicuna/config/skills.json` (skills default to enabled when newly created
//! or added on disk).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsConfig {
    #[serde(default)]
    enabled: BTreeMap<String, bool>,
}

/// Root folder for Vicuna-managed skills: `~/vicuna/skills`.
pub fn skills_dir() -> Result<PathBuf, String> {
    let home = crate::opencode::home_dir().ok_or_else(|| "cannot locate home directory".to_string())?;
    Ok(home.join("vicuna").join("skills"))
}

/// Config file storing per-skill enabled state: `~/vicuna/config/skills.json`.
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
    std::fs::write(path, format!("{body}\n"))
        .map_err(|e| format!("cannot write skills config: {e}"))
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
    list_skills_in(&skills_dir()?, &skills_config_path()?)
}

pub fn list_skills_in(skills_root: &Path, config_path: &Path) -> Result<Vec<SkillSummary>, String> {
    if !skills_root.exists() {
        return Ok(Vec::new());
    }
    let canon_root = std::fs::canonicalize(skills_root)
        .map_err(|e| format!("cannot read skills directory: {e}"))?;
    let cfg = read_config(config_path);
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
        let enabled = *cfg.enabled.get(&fname).unwrap_or(&true);
        out.push(summary_from_content(&fname, &canon_file, &content, enabled));
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Read a single skill's `SKILL.md` content and metadata.
pub fn read_skill(slug: &str) -> Result<SkillDetail, String> {
    read_skill_in(&skills_dir()?, &skills_config_path()?, slug)
}

pub fn read_skill_in(
    skills_root: &Path,
    config_path: &Path,
    slug: &str,
) -> Result<SkillDetail, String> {
    let file = skill_file_path(skills_root, slug)?;
    let canon_file = verify_inside_root(skills_root, &file)?;
    if !canon_file.is_file() {
        return Err(format!("skill '{slug}' has no SKILL.md"));
    }
    let content = std::fs::read_to_string(&canon_file)
        .map_err(|e| format!("cannot read skill '{slug}': {e}"))?;
    let cfg = read_config(config_path);
    let enabled = *cfg.enabled.get(slug).unwrap_or(&true);
    let summary = summary_from_content(slug, &canon_file, &content, enabled);
    Ok(SkillDetail {
        slug: summary.slug,
        name: summary.name,
        description: summary.description,
        enabled: summary.enabled,
        path: summary.path,
        content,
    })
}

/// Create or overwrite a skill's `SKILL.md` and optionally set its enabled state.
/// Newly created skills default to enabled (`true`) when `enabled` is `None`.
pub fn save_skill(slug: &str, content: &str, enabled: Option<bool>) -> Result<SkillSummary, String> {
    save_skill_in(&skills_dir()?, &skills_config_path()?, slug, content, enabled)
}

pub fn save_skill_in(
    skills_root: &Path,
    config_path: &Path,
    slug: &str,
    content: &str,
    enabled: Option<bool>,
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
    std::fs::write(&file, content.as_bytes())
        .map_err(|e| format!("cannot write skill '{slug}': {e}"))?;
    let canon_file = verify_inside_root(skills_root, &file)?;

    let mut cfg = read_config(config_path);
    let is_enabled = match enabled {
        Some(v) => {
            cfg.enabled.insert(slug.to_string(), v);
            v
        }
        None => *cfg.enabled.entry(slug.to_string()).or_insert(true),
    };
    write_config(config_path, &cfg)?;
    Ok(summary_from_content(slug, &canon_file, content, is_enabled))
}

/// Enable or disable an existing skill.
pub fn toggle_skill(slug: &str, enabled: bool) -> Result<SkillSummary, String> {
    toggle_skill_in(&skills_dir()?, &skills_config_path()?, slug, enabled)
}

pub fn toggle_skill_in(
    skills_root: &Path,
    config_path: &Path,
    slug: &str,
    enabled: bool,
) -> Result<SkillSummary, String> {
    let file = skill_file_path(skills_root, slug)?;
    let canon_file = verify_inside_root(skills_root, &file)?;
    if !canon_file.is_file() {
        return Err(format!("skill '{slug}' not found"));
    }
    let content = std::fs::read_to_string(&canon_file)
        .map_err(|e| format!("cannot read skill '{slug}': {e}"))?;
    let mut cfg = read_config(config_path);
    cfg.enabled.insert(slug.to_string(), enabled);
    write_config(config_path, &cfg)?;
    Ok(summary_from_content(slug, &canon_file, &content, enabled))
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
    let canon_dir = verify_inside_root(skills_root, &dir)?;
    std::fs::remove_dir_all(&canon_dir)
        .map_err(|e| format!("cannot remove skill '{slug}': {e}"))?;
    let mut cfg = read_config(config_path);
    if cfg.enabled.remove(slug).is_some() {
        let _ = write_config(config_path, &cfg);
    }
    Ok(())
}

/// Build the compact `<vicuna_skills>` prompt block injected into every Work
/// harness's `system_prompt`. Returns an empty string when no skills are enabled.
pub fn format_skills_prompt() -> String {
    let (Ok(root), Ok(cfg)) = (skills_dir(), skills_config_path()) else {
        return String::new();
    };
    format_skills_prompt_in(&root, &cfg)
}

pub fn format_skills_prompt_in(skills_root: &Path, config_path: &Path) -> String {
    let Ok(all) = list_skills_in(skills_root, config_path) else {
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

        // Empty initially -> empty list and empty prompt
        assert!(list_skills_in(&root, &cfg).unwrap().is_empty());
        assert_eq!(format_skills_prompt_in(&root, &cfg), "");

        // Save new skill (defaults to enabled)
        let md = "---\nname: PR Review\ndescription: Thorough code review checklist\n---\n\n# Steps\n1. Read diff\n";
        let created = save_skill_in(&root, &cfg, "pr-review", md, None).unwrap();
        assert_eq!(created.slug, "pr-review");
        assert_eq!(created.name, "PR Review");
        assert_eq!(created.description, "Thorough code review checklist");
        assert!(created.enabled);
        assert!(created.path.ends_with("pr-review/SKILL.md") || created.path.ends_with(r"pr-review\SKILL.md"));

        // Read back
        let detail = read_skill_in(&root, &cfg, "pr-review").unwrap();
        assert_eq!(detail.content, md);
        assert!(detail.enabled);

        // Prompt includes enabled skill and ~/vicuna/skills/ note
        let prompt = format_skills_prompt_in(&root, &cfg);
        assert!(prompt.contains("<vicuna_skills>"));
        assert!(prompt.contains("PR Review (`pr-review`): Thorough code review checklist"));
        assert!(prompt.contains("~/vicuna/skills/"));

        // Disable -> prompt becomes empty again
        let disabled = toggle_skill_in(&root, &cfg, "pr-review", false).unwrap();
        assert!(!disabled.enabled);
        assert_eq!(format_skills_prompt_in(&root, &cfg), "");

        // Re-enable
        let enabled = toggle_skill_in(&root, &cfg, "pr-review", true).unwrap();
        assert!(enabled.enabled);
        assert!(!format_skills_prompt_in(&root, &cfg).is_empty());

        // Remove cleans directory and config
        remove_skill_in(&root, &cfg, "pr-review").unwrap();
        assert!(list_skills_in(&root, &cfg).unwrap().is_empty());
        assert!(read_skill_in(&root, &cfg, "pr-review").is_err());

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
