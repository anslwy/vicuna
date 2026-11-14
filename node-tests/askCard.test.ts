// Ask card helpers for vicuna-mcp ask_user questions (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { isAgentQuestion, toggleChoice, multiAnswer, buildAskAnswer } from "../src/lib/askCard.ts";

test("answer box: typed / edited text goes as-is", () => {
  assert.deepEqual(buildAskAnswer({ text: "  Kiwi, but ripe ", images: [], multi: false, picked: [] }), {
    answer: "Kiwi, but ripe",
    display: "Kiwi, but ripe"
  });
  assert.equal(buildAskAnswer({ text: "   ", images: [], multi: false, picked: [] }), null);
});

test("answer box: pasted images ride the vicunaAskAnswer envelope", () => {
  const img = "data:image/jpeg;base64,QUJD";
  const r = buildAskAnswer({ text: "like this", images: [img], multi: false, picked: [] })!;
  assert.deepEqual(JSON.parse(r.answer), { vicunaAskAnswer: { text: "like this", images: [img] } });
  assert.equal(r.display, "like this (1 image)");
  const only = buildAskAnswer({ text: "", images: [img, img], multi: false, picked: [] })!;
  assert.equal(only.display, "(2 images)");
  assert.equal(JSON.parse(only.answer).vicunaAskAnswer.text, "");
});

test("answer box: multi-select picks plus a note", () => {
  // picks alone keep the JSON array the backend formats as "selected: …"
  assert.deepEqual(buildAskAnswer({ text: "", images: [], multi: true, picked: ["Ham", "Basil"] }), multiAnswer(["Ham", "Basil"]));
  assert.deepEqual(buildAskAnswer({ text: "extra basil", images: [], multi: true, picked: ["Ham", "Basil"] }), {
    answer: "Selected: Ham; Basil\nextra basil",
    display: "Selected: Ham; Basil\nextra basil"
  });
  assert.equal(buildAskAnswer({ text: "", images: [], multi: true, picked: [] }), null);
});

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
