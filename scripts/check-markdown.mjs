// Headless checks for src/lib/markdown.ts tool-block masking.
// Run: node scripts/check-markdown.mjs
// A tool-result body with blank lines + an odd ``` count used to break out of
// its <details> HTML block during marked parsing and swallow the NEXT tool
// block's header (verified: 2 of 12 blocks vanished from a real transcript).
import {
  maskToolBlocks,
  restoreToolBlocks,
  renderMarkdownUnsanitized
} from "../src/lib/markdown.ts";

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

const block = (head, body) =>
  `<details class="tool-result"><summary>${head}</summary><pre><code>${body}</code></pre></details>`;

// block 1 mimics a fetched README: blank lines + an unclosed ``` fence
const body1 = `# Readme\n\nSome text.\n\n\`\`\`js\nconsole.log("hi");\n\`\`\`\n\nMore.\n\n\`\`\`unclosed`;
const body2 = `second body`;
const src = `Intro.\n\n${block("read — a.md", body1)}\n\n${block("read — b.md", body2)}\n\nTail.`;

// masking: both spans stashed, none left in the masked source
const { masked, stashed, nonce } = maskToolBlocks(src);
eq(stashed.length, 2, "masks both spans");
eq(masked.includes("<details"), false, "masked source has no details");
eq(masked.includes("unclosed"), false, "masked source has no body text");

// restore round-trips the exact spans
eq(
  restoreToolBlocks(masked, stashed, nonce).includes(body1),
  true,
  "restore brings back body 1"
);

// end to end (minus sanitization, which needs a DOM): both headers survive
const html = renderMarkdownUnsanitized(src);
const heads = [...html.matchAll(/<summary>([^<]*)/g)].map((m) => m[1]);
eq(heads, ["read — a.md", "read — b.md"], "both tool headers survive render");
eq(html.includes("unclosed"), true, "fence text kept verbatim in body");
eq(html.includes("<div data-vicuna-tool="), false, "no placeholder leaks");

// ordinary markdown still renders around the blocks
eq(
  renderMarkdownUnsanitized("a **bold** move").includes("<strong>bold</strong>"),
  true,
  "inline markdown unaffected"
);
eq(renderMarkdownUnsanitized(""), "", "empty input empty");

// forged placeholder text in user content is left alone (nonce mismatch)
const forged = `x\n\n<div data-vicuna-tool="0-abcdef"></div>\n\n${block("read — c.md", "c")}`;
eq(
  renderMarkdownUnsanitized(forged).includes("read — c.md"),
  true,
  "real block survives forged placeholder"
);

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("all markdown checks passed");
