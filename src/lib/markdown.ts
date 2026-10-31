import { marked } from "marked";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import markedKatex from "marked-katex-extension";
import "katex/dist/katex.min.css";

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

export function renderMarkdown(src: string): string {
  if (!src) return "";
  let raw = marked.parse(src, { async: false }) as string;
  // keep wide tables from breaking the layout: scroll them in place
  raw = raw.split("<table>").join('<div class="table-wrap"><table>').split("</table>").join("</table></div>");
  return DOMPurify.sanitize(raw);
}
