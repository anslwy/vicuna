// Headless checks for src/lib/markdown.ts tool-block masking.
// Run: node scripts/check-markdown.mjs
// A tool-result body with blank lines + an odd ``` count used to break out of
// its <details> HTML block during marked parsing and swallow the NEXT tool
// block's header (verified: 2 of 12 blocks vanished from a real transcript).
import {
  maskToolBlocks,
  restoreToolBlocks,
  repairToolSpan,
  findToolSpans,
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


// A harness note appended raw inside a card (older builds) carried an
// approved command with `<!--`: the browser opened a comment there and hid
// every later card plus the final answer (real 43-card reply showed 14).
{
  const bad = `<details class="tool-result"><summary>exec — cat</summary><pre><code>\n> Auto-approved: Bash: cat <<'EOF'\n<!-- agent body\nok &amp; done</code></pre></details>`;
  const fixed = repairToolSpan(bad);
  eq(fixed.includes("<!--"), false, "raw comment opener inside a card body is escaped");
  eq(fixed.includes("&lt;!-- agent body"), true, "the note text stays visible");
  eq(fixed.includes("ok &amp; done"), true, "existing entities are not double-escaped");
  const good = `<details class="tool-result"><summary>read — a</summary><pre><code>a &lt;b&gt; &amp; c</code></pre></details>`;
  eq(repairToolSpan(good), good, "correct bodies pass through unchanged");
  const html = renderMarkdownUnsanitized(`one\n\n${bad}\n\nafter the card`);
  eq(html.includes("<!--"), false, "rendered reply has no raw comment opener");
  eq(html.includes("after the card"), true, "text after the card still renders");
}


// Raw notes can carry any card markup: a bare `</details>` (screenshot 1) or
// a full `</code></pre></details>` + `<pre><code>` (screenshot 2). Cards run
// from a line-start opener to the last closer before the next opener.
{
  const card = (head, note, out) =>
    `\n\n<details class="tool-result"><summary>${head}</summary><pre><code>${note}${out}</code></pre></details>`;
  const n1 = `\n> Auto-approved: Bash: python3 -c "j=c.find('</details>',i)"\n`;
  const n2 = `\n> Auto-approved: Bash: cat <<'EOF'\n{"a":"<pre><code>","b":"</code></pre></details>"}\n`;
  const src = `start${card("exec — a", n1, "27725 32109")}Then prose.${card("exec — b", n2, "ok")}\n\nlast words`;
  eq(findToolSpans(src).map((x) => x[2]), [true, true], "fake closers inside notes don't split cards");
  const html = renderMarkdownUnsanitized(src);
  const outside = html.replace(/<details class="tool-result">[\s\S]*?<\/code><\/pre><\/details>/g, "");
  eq(/&lt;details|&lt;summary|<pre>/.test(outside), false, "no card markup leaks outside the cards");
  eq(outside.includes("Then prose.") && outside.includes("last words"), true, "prose between and after cards stays prose");
  eq((html.match(/<details class="tool-result">/g) || []).length, 2, "both cards render");
}
// Two tools started in parallel left the first card open mid-reply; an open
// <details> swallowed the rest. Close it at the next opener; a card still
// open at the very end is a live one and is left alone.
{
  const open1 = `\n\n<details class="tool-result"><summary>exec — one</summary><pre><code>`;
  const full2 = `\n\n<details class="tool-result"><summary>skill — two</summary><pre><code>done</code></pre></details>`;
  const src = `${open1}\n\n\n${full2}\n\nafter both`;
  eq(findToolSpans(src).map((x) => x[2]), [false, true], "mid-reply unclosed card is stashed");
  const html = renderMarkdownUnsanitized(src);
  eq((html.match(/<\/details>/g) || []).length, 2, "the unclosed card gets closed");
  eq(html.includes("after both"), true, "text after them still renders");
  eq(findToolSpans(`text${open1}partial output`).length, 0, "a live last card is left as-is");
}

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("all markdown checks passed");
