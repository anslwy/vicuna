export function escapeHtmlText(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

export function escapeHtmlAttr(s: string): string {
  return escapeHtmlText(s).replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/** join an upload dir and a relative file name with the dir's separator */
export function joinUploadPath(dir: string, name: string): string {
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}

export interface MentionToken {
  start: number;
  end: number;
  core: string;
}

/** @path tokens in text: @ + non-space run at start/after whitespace (so
 *  a@b.com stays plain), trailing [.,;:!?)] excluded. Shared by rendering,
 *  caret snapping, and unit deletion so all three always agree. */
export function findMentions(text: string): MentionToken[] {
  const out: MentionToken[] = [];
  const re = /(^|[\s(])(@[^\s]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const tok: string = m[2];
    const tail = tok.match(/[.,;:!?)]+$/);
    const core = tail ? tok.slice(0, -tail[0].length) : tok;
    if (core.length <= 1) continue;
    const start = m.index + m[1].length;
    out.push({ start, end: start + core.length, core });
  }
  return out;
}

export const mentionFolderIcon = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>';

export const mentionFileIcon = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><polyline points="13 2 13 9 20 9"/></svg>';

export function mentionIconFor(core: string): string {
  const base = core.slice(1).split(/[/\\]/).pop() ?? "";
  return base.includes(".") ? mentionFileIcon : mentionFolderIcon;
}

/** render @path mentions as green pills (ws-badge style) with a folder/file
 *  icon. Scans the escaped text (entities contain no @/space/paren, so
 *  boundaries match the raw text). Padding/margin cancel so the composer
 *  backdrop aligns exactly with the transparent textarea text. */
export function renderMentions(text: string): string {
  const esc = escapeHtmlText(text);
  const toks = findMentions(esc);
  if (!toks.length) return esc;
  let out = "";
  let last = 0;
  for (const t of toks) {
    out += esc.slice(last, t.start);
    out += `<span class="mention-pill">${mentionIconFor(t.core)}<span>${esc.slice(t.start, t.end)}</span></span>`;
    last = t.end;
  }
  return out + esc.slice(last);
}

/** user bubbles: @-mentions become pills, and attached-files notes collapse
 *  to a clickable file list at the top of the bubble (raw text keeps the
 *  full note so the agent still sees paths + tool instructions in history) */
export function renderUserMessage(text: string): string {
  const re = /\[Attached files saved to ([^\]]*?) \(use the read_upload tool[^)]*\):\r?\n((?:[ \t]*-[ \t]+[^\r\n]*\r?\n?)+)\]/g;
  const files: { name: string; path: string }[] = [];
  const body = text.replace(re, (_full, dir: string, list: string) => {
    for (const x of list.matchAll(/^[ \t]*-[ \t]*(.+?)[ \t]*$/gm)) {
      const name = x[1];
      if (name) files.push({ name, path: joinUploadPath(dir, name) });
    }
    return "";
  });
  if (!files.length) return renderMentions(text);
  const pills = files
    .map(
      (f) =>
        `<button type="button" class="mention-pill file-pill" data-reveal-path="${escapeHtmlAttr(f.path)}" title="Reveal in Finder">${mentionFileIcon}<span>${escapeHtmlText(f.name)}</span></button>`
    )
    .join("");
  const rest = renderMentions(body.replace(/^\s+/, "").replace(/\s+$/, ""));
  return `<div class="bubble-files">${pills}</div>${rest}`;
}
