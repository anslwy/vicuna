import { test } from "node:test";
import assert from "node:assert/strict";
import { harnessUsageUrl } from "../src/lib/harnessUsageLinks.ts";

test("provider usage URLs are available only for Claude Code and Codex", () => {
  assert.equal(harnessUsageUrl("claude"), "https://claude.ai/new#settings/usage");
  assert.equal(harnessUsageUrl("codex"), "https://chatgpt.com/settings/usage?tab=overview");
  assert.equal(harnessUsageUrl("agy"), null);
  assert.equal(harnessUsageUrl("opencode"), null);
});
