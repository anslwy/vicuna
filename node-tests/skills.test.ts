import assert from "node:assert/strict";
import test from "node:test";
import { chatExtraJson, parseChatExtra } from "../src/lib/leadSessions.ts";
import {
  buildSkillTemplate,
  enabledSkillCount,
  newSkillSelection,
  resolveSkills,
  skillSwitches,
  slugifySkillName,
  validateSkillSlug,
  type SkillSummary
} from "../src/lib/skills.ts";

test("slugifySkillName produces valid lowercase slugs", () => {
  assert.equal(slugifySkillName("Code Review!"), "code-review");
  assert.equal(slugifySkillName("  PR_Helper v2  "), "pr_helper-v2");
  assert.equal(slugifySkillName("---Leading & Trailing---"), "leading-trailing");
  assert.equal(slugifySkillName("a".repeat(80)).length, 64);
});

test("validateSkillSlug enforces strict slug rules", () => {
  assert.equal(validateSkillSlug("code-review"), null);
  assert.equal(validateSkillSlug("deploy_v2"), null);
  assert.equal(validateSkillSlug("0skill"), null);

  assert.notEqual(validateSkillSlug(""), null);
  assert.notEqual(validateSkillSlug("-bad"), null);
  assert.notEqual(validateSkillSlug("Upper"), null);
  assert.notEqual(validateSkillSlug("../escape"), null);
  assert.notEqual(validateSkillSlug("a/b"), null);
  assert.notEqual(validateSkillSlug("a".repeat(65)), null);
});

test("buildSkillTemplate produces valid YAML frontmatter", () => {
  const md = buildSkillTemplate("PR Review", "Review pull requests: check tests & edge cases");
  assert.ok(md.startsWith("---\n"));
  assert.ok(md.includes("name: PR Review\n"));
  assert.ok(md.includes('description: "Review pull requests: check tests & edge cases"\n'));
  assert.ok(md.includes("# PR Review\n"));
});

test("enabledSkillCount counts only enabled skills", () => {
  const list: SkillSummary[] = [
    { slug: "a", name: "A", description: "", enabled: true, path: "/tmp/a/SKILL.md" },
    { slug: "b", name: "B", description: "", enabled: false, path: "/tmp/b/SKILL.md" },
    { slug: "c", name: "C", description: "", enabled: true, path: "/tmp/c/SKILL.md" }
  ];
  assert.equal(enabledSkillCount(list), 2);
  assert.equal(enabledSkillCount([]), 0);
});


test("draft choices and saved session switches stay separate from defaults", () => {
  const skill = (slug: string, enabled: boolean): SkillSummary => ({
    slug, enabled, name: slug, description: "", path: `/tmp/${slug}/SKILL.md`
  });
  const defaults = [skill("review", true), skill("release", false)];
  const sessionA = newSkillSelection(defaults, { review: false, release: true, removed: false });
  const sessionB = newSkillSelection(defaults);
  assert.deepEqual(sessionA, { review: false, release: true });
  assert.deepEqual(sessionB, { review: true, release: false });
  const metadata = { skillsEnabled: sessionA, mcpEnabled: { browser: true }, engine: "pi" };
  assert.deepEqual(parseChatExtra(chatExtraJson(metadata)), metadata);
  assert.deepEqual(parseChatExtra(chatExtraJson({ skillsEnabled: {} })), { skillsEnabled: {} });

  const savedDefaults = resolveSkills(defaults, sessionA);
  assert.deepEqual(skillSwitches(savedDefaults), sessionA);
  assert.deepEqual(newSkillSelection(savedDefaults), sessionA);
  assert.deepEqual(skillSwitches(resolveSkills(savedDefaults, sessionB)), sessionB);

  const catalogAfterAdd = [...savedDefaults, skill("new", true)];
  assert.deepEqual(skillSwitches(resolveSkills(catalogAfterAdd, sessionB)), { ...sessionB, new: true });
});
