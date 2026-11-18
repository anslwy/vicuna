import { marked } from "marked";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import markedKatex from "marked-katex-extension";
// NOTE: katex.min.css is imported by ChatApp.svelte instead of here so this
// module stays importable from headless node checks (see scripts/check-markdown.mjs).

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

const COPY_SVG = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;

marked.setOptions({
  breaks: true,
  gfm: true
});

// math first ($...$ inline, $$...$$ block), then the code renderer below
marked.use(markedKatex({ throwOnError: false }));

marked.use({
  renderer: {
    // inline code (`...`): marked pink + click-to-copy in ChatApp, so tag it
    // here. `text` arrives RAW (marked escapes at render time), hence the
    // explicit escape — fenced blocks go through `code()` below instead.
    codespan({ text }: { text: string }) {
      return `<code class="inl">${escapeHtml(text)}</code>`;
    },
    // links: external ones carry target/rel so they open outside the app
    // even without JS (middle-click, no-JS contexts). The ChatApp click
    // delegation on message containers is the primary route — it sends
    // http(s)/mailto links to the system browser instead of navigating the
    // app webview itself (which strands the session with no way back).
    // Internal links (/#… routes, vicuna:// deep links) keep default
    // behavior for the router / deep-link handlers.
    link({ href, title, tokens }: { href: string; title?: string | null; tokens?: unknown[] }) {
      const body = (this as unknown as { parser: { parseInline(t: unknown): string } }).parser.parseInline(tokens ?? []);
      const safe = escapeHtml(href);
      const t = title ? ` title="${escapeHtml(title)}"` : "";
      if (/^(https?:\/\/|mailto:)/i.test(href)) {
        return `<a href="${safe}"${t} target="_blank" rel="noopener noreferrer">${body}</a>`;
      }
      return `<a href="${safe}"${t}>${body}</a>`;
    },
    code({ text, lang }: { text: string; lang?: string }) {
      let html: string;
      let cls = "hljs";
      if (text.length > 8000) {
        // huge dumps (full-file reads pasted as fences): auto-detect costs
        // ~100ms+ per render and re-runs on every streamed token — plain
        // escaped text instead, highlighting buys nothing here
        html = escapeHtml(text);
      } else if (lang && hljs.getLanguage(lang)) {
        html = hljs.highlight(text, { language: lang }).value;
        cls += ` language-${escapeHtml(lang)}`;
      } else {
        // unlabeled or unknown fence: best-effort auto-detect
        html = hljs.highlightAuto(text).value;
      }
      return (
        `<div class="codeblock">` +
        `<button class="codecopy" data-copy-code aria-label="Copy code" title="Copy code">${COPY_SVG}</button>` +
        `<pre><code class="${cls}">${html}</code></pre>` +
        `</div>`
      );
    }
  }
});

// Tool-result spans must reach the DOM untouched: their bodies are escaped
// tool output that routinely contains blank lines plus markdown constructs
// (``` fences, $math$, tables). marked ends an HTML block at the first blank
// line, so without masking the body remainder parses as Markdown — an odd
// ``` count in one fetched README then swallows the NEXT tool block's header
// (verified: 2 of 12 blocks vanished from a real transcript).
//
// Where a card ends: older builds appended harness notes (an approved shell
// command) raw inside the card, and a command can contain any markup —
// `</details>`, `</code></pre></details>`, `<!--`. Ending at the first
// `</details>` then closed cards early and spilled the rest as stray code.
// Real openers always start a line (`\n\n` + opener, toolFormat.ts), and a
// note sits before the card's output, so a card runs from one real opener to
// the LAST real closer before the next opener. An unclosed card (crash
// mid-tool, or still running) has no closer: at the end of the reply it is
// left as-is (a live card), but one followed by another opener is closed
// there — two tools started in parallel used to leave the first one open,
// and an open <details> swallows the rest of the reply.
const SPAN_OPEN = '<details class="tool-result">';
const SPAN_OPEN_LINE = /(^|\n)<details class="tool-result">/g;
const SPAN_BODY_OPEN = "<pre><code>";
const SPAN_BODY_CLOSE = "</code></pre></details>";

/** [start, end, closed] of every tool card that can be stashed: closed ones
 *  plus unclosed ones another card follows. Exported for headless checks. */
export function findToolSpans(src: string): Array<[number, number, boolean]> {
  const starts: number[] = [];
  for (const m of src.matchAll(SPAN_OPEN_LINE)) starts.push((m.index ?? 0) + m[1].length);
  const spans: Array<[number, number, boolean]> = [];
  for (let i = 0; i < starts.length; i++) {
    const last = i + 1 >= starts.length;
    const segEnd = last ? src.length : starts[i + 1];
    const close = src.lastIndexOf(SPAN_BODY_CLOSE, segEnd - SPAN_BODY_CLOSE.length);
    if (close >= starts[i] + SPAN_OPEN.length) spans.push([starts[i], close + SPAN_BODY_CLOSE.length, true]);
    else if (!last && src.indexOf(SPAN_BODY_OPEN, starts[i]) > -1 && src.indexOf(SPAN_BODY_OPEN, starts[i]) < segEnd) spans.push([starts[i], segEnd, false]);
  }
  return spans;
}

/** Re-escape stray markup inside a tool card's body. Bodies are written
 *  escaped, but older builds appended harness notes (an approved command)
 *  raw; one containing `<!--` opened an HTML comment that hid the rest of
 *  the reply. Only `<`/`>` are touched, so correct bodies (entities only)
 *  come out unchanged. Exported for headless checks. */
export function repairToolSpan(span: string): string {
  const start = span.indexOf(SPAN_BODY_OPEN);
  if (start < 0 || !span.endsWith(SPAN_BODY_CLOSE)) return span;
  const from = start + SPAN_BODY_OPEN.length;
  const to = span.length - SPAN_BODY_CLOSE.length;
  if (to < from) return span;
  const body = span.slice(from, to).replace(/</g, "&lt;").replace(/>/g, "&gt;");
  return span.slice(0, from) + body + SPAN_BODY_CLOSE;
}

/** Replace each tool-result span with a single-line div placeholder that
 *  marked passes through as an opaque HTML block. Returns the masked source
 *  plus the spans in placeholder order. Exported for headless checks. */
export function maskToolBlocks(src: string): { masked: string; stashed: string[]; nonce: string } {
  const stashed: string[] = [];
  // per-call nonce: tool/user text could otherwise forge a placeholder and
  // get it replaced with (or deleted for) another block's content
  const nonce = Math.random().toString(36).slice(2);
  let masked = "";
  let at = 0;
  for (const [s, e, closed] of findToolSpans(src)) {
    stashed.push(repairToolSpan(closed ? src.slice(s, e) : src.slice(s, e).trimEnd() + SPAN_BODY_CLOSE));
    masked += src.slice(at, s) + `\n\n<div data-vicuna-tool="${stashed.length - 1}-${nonce}"></div>\n\n`;
    at = e;
  }
  masked += src.slice(at);
  return { masked, stashed, nonce };
}

/** Restore stashed tool spans over their placeholders. Exported for checks. */
export function restoreToolBlocks(raw: string, stashed: string[], nonce: string): string {
  return raw.replace(new RegExp(`<div data-vicuna-tool="(\\d+)-${nonce}">\\s*</div>`, "g"), (_m, i: string) => {
    return stashed[Number(i)] ?? "";
  });
}

/** Full render pipeline minus sanitization (DOM-free, headless-testable). */
export function renderMarkdownUnsanitized(src: string): string {
  if (!src) return "";
  const { masked, stashed, nonce } = maskToolBlocks(src);
  let raw = marked.parse(masked, { async: false }) as string;
  // keep wide tables from breaking the layout: scroll them in place
  raw = raw.split("<table>").join('<div class="table-wrap"><table>').split("</table>").join("</table></div>");
  return restoreToolBlocks(raw, stashed, nonce);
}

export function renderMarkdown(src: string): string {
  if (!src) return "";
  return DOMPurify.sanitize(renderMarkdownUnsanitized(src));
}
