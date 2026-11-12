import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldWarnOllamaUnavailable } from "../src/lib/ollamaWarning.ts";

test("warn only for an Ollama model when Ollama is unreachable", () => {
  assert.equal(shouldWarnOllamaUnavailable(true, false), true);
  assert.equal(shouldWarnOllamaUnavailable(true, true), false);
  assert.equal(shouldWarnOllamaUnavailable(false, false), false);
});
