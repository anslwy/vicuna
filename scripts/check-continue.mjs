// Headless checks for src/lib/continue.ts (pure logic). Run: node scripts/check-continue.mjs
import {
  CONTINUE_MODES,
  normalizeContinueMode,
  promptStyleForFamily,
  useRawContinue,
  buildRawContinuePrompt,
  normalizePromptStyle,
  isRunawayRepetition,
  stripRunawayTail
} from "../src/lib/continue.ts";

let failures = 0;
function eq(actual, expected, label) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    failures++;
    console.error(`FAIL ${label}: got ${a}, want ${e}`);
  } else {
    console.log(`ok ${label}`);
  }
}

// ---- mode plumbing ----
eq(normalizeContinueMode("raw"), "raw", "raw mode survives");
eq(normalizeContinueMode("native"), "native", "native mode survives");
eq(normalizeContinueMode("auto"), "auto", "auto mode survives");
eq(normalizeContinueMode(undefined), "raw", "missing mode -> raw default");
eq(normalizeContinueMode("bogus"), "raw", "unknown mode -> raw default");
eq(CONTINUE_MODES.map((m) => m.id), ["auto", "raw", "native"], "mode order");

// ---- family -> prompt style ----
eq(promptStyleForFamily("gemma4"), "gemma4", "gemma4 family");
eq(promptStyleForFamily("Gemma4"), "gemma4", "case insensitive");
eq(promptStyleForFamily("gemma3"), "gemma3", "gemma3 family");
eq(promptStyleForFamily("gemma2"), "gemma3", "gemma2 uses gemma3 format");
eq(promptStyleForFamily("qwen3_5"), null, "qwen has no raw format");
eq(promptStyleForFamily(null), null, "unknown family");
eq(normalizePromptStyle("gemma4"), "gemma4", "persisted style validated");
eq(normalizePromptStyle("nope"), null, "bogus persisted style rejected");

// ---- mode resolution ----
eq(useRawContinue("auto", "gemma4"), true, "auto: gemma4 raw");
eq(useRawContinue("auto", "gemma3"), false, "auto: gemma3 stays native");
eq(useRawContinue("raw", "gemma3"), true, "forced raw: gemma3");
eq(useRawContinue("raw", null), false, "forced raw without a format falls back");
eq(useRawContinue("native", "gemma4"), false, "native always native");

// ---- prompt construction ----
const convo = [
  { role: "system", content: "be nice" },
  { role: "user", content: "hey there" },
  { role: "assistant", content: "Yo! How's " }
];
const g4 = buildRawContinuePrompt(convo, "gemma4");
eq(
  g4?.prompt,
  "<bos><|turn>system\nbe nice<turn|>\n<|turn>user\nhey there<turn|>\n<|turn>model\nYo! How's ",
  "gemma4 prompt pre-fills the partial reply"
);
eq(g4?.stop, ["<turn|>", "<|turn>", "<|channel>", "<channel|>"], "gemma4 structural stops");

const g3 = buildRawContinuePrompt(
  [
    { role: "system", content: "be nice" },
    { role: "user", content: "hey there" },
    { role: "assistant", content: "Yo! How's " }
  ],
  "gemma3"
);
eq(
  g3?.prompt,
  "<bos><start_of_turn>user\nbe nice\n\nhey there<end_of_turn>\n<start_of_turn>model\nYo! How's ",
  "gemma3 folds the system prompt into the first user turn"
);

eq(buildRawContinuePrompt([{ role: "user", content: "hey" }], "gemma4"), null, "no partial reply -> null");

// trailing whitespace in the partial must reach the prompt untouched
const spaced = buildRawContinuePrompt([{ role: "user", content: "hi" }, { role: "assistant", content: "Yo! How's " }], "gemma4");
eq(spaced?.prompt.endsWith("model\nYo! How's "), true, "trailing space preserved");

// ---- runaway guard ----
eq(isRunawayRepetition("it going? What's on your mind today?"), false, "normal text is fine");
eq(isRunawayRepetition("hahaha"), false, "short laughter is fine");
eq(isRunawayRepetition("-".repeat(40)), false, "markdown rule is fine");
eq(isRunawayRepetition("___".repeat(20)), false, "underscore rule is fine");
eq(isRunawayRepetition("it going? " + "owns ".repeat(20)), true, "repeated word tail detected");
eq(isRunawayRepetition("ok " + "🚀".repeat(20)), true, "repeated emoji tail detected");
eq(isRunawayRepetition("ok " + "same-day ".repeat(8)), true, "repeated phrase tail detected");

eq(stripRunawayTail("it going? " + "owns ".repeat(20)), "it going? ", "repeated word tail stripped");
eq(stripRunawayTail("ok " + "🚀".repeat(20)), "ok ", "repeated emoji tail stripped");
eq(stripRunawayTail("it going? What's on your mind today?"), "it going? What's on your mind today?", "clean text untouched");
// a run longer than the 4000-char scan window is still removed end-to-end
const longRun = "done. " + "blah ".repeat(2000);
eq(stripRunawayTail(longRun), "done. ", "long runaway run fully stripped");

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("all continue checks passed");
