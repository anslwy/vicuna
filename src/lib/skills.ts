// Vicuna Skills client (display-layer adapter, like mcp.ts).
//
// Skills live in ~/vicuna/skills/<slug>/SKILL.md (YAML frontmatter `name:` +
// `description:` followed by Markdown instructions); enabled/disabled state
// lives in Work session metadata. New sessions use ~/vicuna/config/skills.json.

export type SkillsEnabledMap = Record<string, boolean>;

/** A saved map is a snapshot. A definition added later starts enabled. */
export function resolveSkills(
  defaults: readonly SkillSummary[],
  selection?: SkillsEnabledMap
): SkillSummary[] {
  if (!selection) return [...defaults];
  return defaults.map((skill) => ({
    ...skill,
    enabled: Object.hasOwn(selection, skill.slug) ? selection[skill.slug] : true
  }));
}

export function skillSwitches(skills: readonly SkillSummary[]): SkillsEnabledMap {
  return Object.fromEntries(skills.map((skill) => [skill.slug, skill.enabled]));
}

export function newSkillSelection(
  defaults: readonly SkillSummary[],
  draft?: SkillsEnabledMap
): SkillsEnabledMap {
  return skillSwitches(defaults.map((skill) => ({
    ...skill,
    enabled: draft && Object.hasOwn(draft, skill.slug) ? draft[skill.slug] : skill.enabled
  })));
}

export interface SkillSummary {
  slug: string;
  name: string;
  description: string;
  enabled: boolean;
  path: string;
}

export interface SkillDetail extends SkillSummary {
  content: string;
}

const SLUG_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;

/** Turn a human skill title into a valid lowercase slug (`[a-z0-9][a-z0-9_-]{0,63}`). */
export function slugifySkillName(name: string): string {
  const clean = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^[-_]+|[-_]+$/g, "")
    .slice(0, 64);
  return clean;
}

/** Validate a skill slug; returns an error message or null when valid. */
export function validateSkillSlug(slug: string): string | null {
  const s = slug.trim();
  if (!s) return "Give the skill a slug (e.g. code-review).";
  if (s.length > 64) return "Slug must be at most 64 characters.";
  if (!SLUG_RE.test(s)) {
    return "Slug must start with a-z or 0-9 and use only lowercase letters, digits, hyphens, or underscores.";
  }
  return null;
}

function yamlScalar(v: string): string {
  const clean = v.replace(/\r?\n/g, " ").trim();
  if (!clean) return '""';
  if (/[:#"'`{}[\],&*?|<>=!%@\\]|^\s|\s$/.test(clean)) {
    return `"${clean.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
  }
  return clean;
}

/** Starter SKILL.md template with YAML frontmatter. */
export function buildSkillTemplate(name: string, description: string): string {
  const title = name.trim() || "New Skill";
  const desc = description.trim() || "Describe when and how to use this skill.";
  return `---
name: ${yamlScalar(title)}
description: ${yamlScalar(desc)}
---

# ${title}

## When to use
- Describe the situations where this skill applies.

## Instructions
1. Step-by-step guidance for the agent.
`;
}

/** Number of currently enabled skills (used for the toolbar badge). */
export function enabledSkillCount(skills: readonly SkillSummary[]): number {
  let n = 0;
  for (const s of skills) if (s.enabled) n++;
  return n;
}

export async function listSkills(): Promise<SkillSummary[]> {
  const { invokeOrBridge } = await import("./bridge");
  return await invokeOrBridge<SkillSummary[]>("skills_list", {});
}

export async function readSkill(slug: string): Promise<SkillDetail> {
  const { invokeOrBridge } = await import("./bridge");
  return await invokeOrBridge<SkillDetail>("skills_read", { slug });
}

export async function saveSkill(
  slug: string,
  content: string
): Promise<SkillSummary> {
  const { invokeOrBridge } = await import("./bridge");
  return await invokeOrBridge<SkillSummary>("skills_save", {
    slug,
    content
  });
}

export async function saveSkillDefaults(skillsEnabled: SkillsEnabledMap): Promise<void> {
  const { invokeOrBridge } = await import("./bridge");
  await invokeOrBridge("skills_set_defaults", { skillsEnabled });
}

export async function removeSkill(slug: string): Promise<void> {
  const { invokeOrBridge } = await import("./bridge");
  await invokeOrBridge("skills_remove", { slug });
}
