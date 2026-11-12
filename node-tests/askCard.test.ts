// Ask card helpers for vicuna-mcp ask_user questions (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { isAgentQuestion, toggleChoice, multiAnswer } from "../src/lib/askCard.ts";

test("agent questions are told apart from permission requests", () => {
  assert.equal(isAgentQuestion("ask-1"), true);
  assert.equal(isAgentQuestion("perm-3"), false);
});

test("multi-select toggles keep the choices' order", () => {
  const opts = ["Ham", "Olives", "Basil"];
  let picked = toggleChoice(opts, [], "Basil");
  picked = toggleChoice(opts, picked, "Ham");
  assert.deepEqual(picked, ["Ham", "Basil"]);
  assert.deepEqual(toggleChoice(opts, picked, "Ham"), ["Basil"]);
});

test("multi-select answers go to the backend as a JSON array", () => {
  assert.deepEqual(multiAnswer(["Ham", "Basil"]), { answer: '["Ham","Basil"]', display: "Ham, Basil" });
  assert.deepEqual(multiAnswer([]), { answer: "[]", display: "(none)" });
});
