<script lang="ts">
  // In-app code editor for the explorer (click / double-click / right-click →
  // Open). CodeMirror 6, dynamically imported by the parent so the initial
  // bundle stays lean. Cmd/Ctrl+S saves; a ● replaces × while the buffer
  // differs from disk; closing dirty asks Save / Don't Save / Cancel; saving
  // re-reads disk first and offers Overwrite / Reload on a conflict.
  import { onDestroy, onMount, tick } from "svelte";
  import { EditorView, keymap } from "@codemirror/view";
  import { EditorState, Compartment } from "@codemirror/state";
  import { basicSetup } from "codemirror";
  import { indentWithTab } from "@codemirror/commands";
  import { HighlightStyle, LanguageDescription, syntaxHighlighting } from "@codemirror/language";
  import { languages } from "@codemirror/language-data";
  import { tags as t } from "@lezer/highlight";
  import { vim, Vim } from "@replit/codemirror-vim";
  import { readWorkFile, writeWorkFile } from "./work";
  import { escapeAction, fileBase, isDirty, readOnlyReason, readVimPref, saveAction, writeVimPref } from "./editorModel";

  // Vim ex commands are global to the library; route them to whichever editor
  // instance issued them (cm.cm6 is that instance's EditorView).
  type ExHandlers = { save: () => Promise<boolean>; close: () => void };
  const exRoutes = new WeakMap<EditorView, ExHandlers>();
  let exDefined = false;
  function defineExCommands() {
    if (exDefined) return;
    exDefined = true;
    const route = (cm: any) => exRoutes.get(cm?.cm6);
    Vim.defineEx("write", "w", (cm: any) => void route(cm)?.save());
    Vim.defineEx("quit", "q", (cm: any) => route(cm)?.close());
    Vim.defineEx("wq", "wq", async (cm: any) => {
      const r = route(cm);
      if (r && (await r.save())) r.close();
    });
    Vim.defineEx("xit", "x", async (cm: any) => {
      const r = route(cm);
      if (r && (await r.save())) r.close();
    });
  }

  let {
    root,
    path,
    onclose,
    onsaved,
    onopenexternal
  }: {
    root: string;
    path: string;
    onclose: () => void;
    onsaved?: (path: string) => void;
    onopenexternal?: (path: string) => void;
  } = $props();

  let host: HTMLDivElement | undefined = $state();
  let view: EditorView | null = null;
  const langConf = new Compartment();
  const vimConf = new Compartment();
  const vimStore = typeof localStorage !== "undefined" ? localStorage : null;
  let vimOn = $state(readVimPref(vimStore));

  function toggleVim() {
    vimOn = !vimOn;
    writeVimPref(vimStore, vimOn);
    view?.dispatch({ effects: vimConf.reconfigure(vimOn ? vim({ status: true }) : []) });
    view?.focus();
  }

  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let roReason = $state<string | null>(null);
  let saved = ""; // disk text as of last load/save (dirty baseline)
  let dirty = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let savedFlash = $state(false);
  let confirmClose = $state(false);
  let conflict = $state(false);
  let langName = $state("Plain text");
  let cursor = $state({ line: 1, col: 1 });
  // same detection as ChatApp's other shortcut hints (⌘N vs Ctrl+N)
  const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.userAgent ?? "");
  const saveKey = isMac ? "⌘S" : "Ctrl+S";

  // syntax colors come from the app's existing --code-* palette, so light/dark
  // and theme changes apply without a CodeMirror theme swap
  const highlight = HighlightStyle.define([
    { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.operatorKeyword, t.definitionKeyword], color: "var(--code-keyword)" },
    { tag: [t.string, t.special(t.string), t.regexp, t.character], color: "var(--code-string)" },
    { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: "var(--code-comment)", fontStyle: "italic" },
    { tag: [t.number, t.bool, t.null, t.atom], color: "var(--code-number)" },
    { tag: [t.function(t.variableName), t.function(t.propertyName), t.className, t.typeName, t.definition(t.typeName)], color: "var(--code-title)" },
    { tag: [t.standard(t.variableName), t.self], color: "var(--code-builtin)" },
    { tag: [t.attributeName, t.propertyName, t.tagName], color: "var(--code-attr)" },
    { tag: [t.heading], fontWeight: "bold", color: "var(--code-title)" },
    { tag: [t.link, t.url], color: "var(--link)", textDecoration: "underline" },
    { tag: [t.emphasis], fontStyle: "italic" },
    { tag: [t.strong], fontWeight: "bold" },
    { tag: [t.invalid], color: "var(--danger)" }
  ]);

  const theme = EditorView.theme({
    "&": { height: "100%", backgroundColor: "var(--bg)", color: "var(--text)", fontSize: "13px" },
    ".cm-scroller": { fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace', lineHeight: "1.55" },
    ".cm-content": { caretColor: "var(--accent)" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)" },
    ".cm-gutters": { backgroundColor: "var(--bg)", color: "var(--faint)", border: "none", borderRight: "1px solid var(--border)" },
    ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--hover-3) 60%, transparent)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--text-dim)" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
      backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent) !important"
    },
    ".cm-selectionMatch": { backgroundColor: "color-mix(in srgb, var(--accent) 12%, transparent)" },
    "&.cm-focused .cm-matchingBracket": { backgroundColor: "color-mix(in srgb, var(--accent) 20%, transparent)", outline: "none" },
    ".cm-panels": { backgroundColor: "var(--card)", color: "var(--text)", borderColor: "var(--border)" },
    ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
    ".cm-textfield": { backgroundColor: "var(--bg)", color: "var(--text)", border: "1px solid var(--input-border)", borderRadius: "6px" },
    ".cm-button": { backgroundImage: "none", backgroundColor: "var(--item-bg)", color: "var(--text)", border: "1px solid var(--border-2)", borderRadius: "6px" },
    ".cm-searchMatch": { backgroundColor: "color-mix(in srgb, var(--accent) 25%, transparent)" },
    ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "color-mix(in srgb, var(--accent) 45%, transparent)" },
    ".cm-tooltip": { backgroundColor: "var(--card)", border: "1px solid var(--border-2)", color: "var(--text)" },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "color-mix(in srgb, var(--accent) 25%, transparent)", color: "var(--text)" },
    ".cm-foldPlaceholder": { backgroundColor: "var(--chip)", border: "none", color: "var(--muted)" }
  });

  function errText(e: any): string {
    return typeof e === "string" ? e : (e?.message ?? String(e));
  }

  function docText(): string {
    return view ? view.state.doc.toString() : "";
  }

  function refreshDirty() {
    dirty = view ? isDirty(docText(), saved) : false;
  }

  async function loadLanguage() {
    const desc = LanguageDescription.matchFilename(languages, fileBase(path));
    if (!desc || !view) return;
    try {
      const support = await desc.load();
      if (!view) return;
      view.dispatch({ effects: langConf.reconfigure(support) });
      langName = desc.name;
    } catch {
      /* stay plain text */
    }
  }

  function setDoc(text: string) {
    if (!view) return;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
  }

  async function load() {
    loading = true;
    loadError = null;
    try {
      const f = await readWorkFile(root, path);
      saved = f.content;
      roReason = readOnlyReason(f.truncated, f.size);
      await tick();
      if (!host) return;
      view = new EditorView({
        parent: host,
        state: EditorState.create({
          doc: f.content,
          extensions: [
            // must precede the other keymaps so Vim sees keys first
            vimConf.of(vimOn ? vim({ status: true }) : []),
            keymap.of([
              { key: "Mod-s", preventDefault: true, run: () => (void save(), true) },
              indentWithTab
            ]),
            basicSetup,
            theme,
            syntaxHighlighting(highlight),
            langConf.of([]),
            EditorState.readOnly.of(roReason !== null),
            EditorView.updateListener.of((u) => {
              if (u.docChanged) refreshDirty();
              if (u.selectionSet || u.docChanged) {
                const head = u.state.selection.main.head;
                const line = u.state.doc.lineAt(head);
                cursor = { line: line.number, col: head - line.from + 1 };
              }
            })
          ]
        })
      });
      defineExCommands();
      exRoutes.set(view, { save: () => save(), close: requestClose });
      view.focus();
      void loadLanguage();
    } catch (e) {
      const msg = errText(e);
      loadError = /binary/i.test(msg) ? "This looks like a binary file, so it can't be edited here." : `Couldn't open: ${msg}`;
    } finally {
      loading = false;
    }
  }

  /** Re-read disk, then write unless it moved under us (force skips the check). */
  async function save(force = false): Promise<boolean> {
    if (!view || roReason || saving) return false;
    saveError = null;
    saving = true;
    try {
      const doc = docText();
      if (!force) {
        let onDisk: string | null = null;
        try {
          onDisk = (await readWorkFile(root, path)).content;
        } catch {
          onDisk = null; // deleted/unreadable: saving recreates it
        }
        const act = saveAction(doc, saved, onDisk);
        if (act === "clean") return true;
        if (act === "conflict") {
          conflict = true;
          return false;
        }
      }
      await writeWorkFile(root, path, doc);
      saved = doc;
      conflict = false;
      refreshDirty();
      savedFlash = true;
      setTimeout(() => (savedFlash = false), 1200);
      onsaved?.(path);
      return true;
    } catch (e) {
      saveError = `Couldn't save: ${errText(e)}`;
      return false;
    } finally {
      saving = false;
    }
  }

  async function reloadFromDisk() {
    conflict = false;
    try {
      const f = await readWorkFile(root, path);
      saved = f.content;
      setDoc(f.content);
      refreshDirty();
    } catch (e) {
      saveError = `Couldn't reload: ${errText(e)}`;
    }
  }

  // a double-click opens on its first click; its second click must not land
  // on the fresh backdrop and close the editor again
  const mountedAt = Date.now();
  function onBackdrop() {
    if (Date.now() - mountedAt < 500) return;
    requestClose();
  }

  function requestClose() {
    if (dirty) {
      confirmClose = true;
      return;
    }
    onclose();
  }

  async function saveAndClose() {
    confirmClose = false;
    if (await save()) onclose();
  }

  function onKey(e: KeyboardEvent) {
    // the global Esc (double-Esc interrupt) must never see editor keys
    e.stopPropagation();
    if (e.defaultPrevented) return; // CodeMirror handled it (search panel, autocomplete…)
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void save();
      return;
    }
    if (e.key === "Escape") {
      const act = escapeAction(vimOn, conflict, confirmClose);
      if (act === "none") return; // Vim owns Esc (leave insert/visual mode)
      e.preventDefault();
      if (act === "dismiss-conflict") conflict = false;
      else if (act === "dismiss-confirm") confirmClose = false;
      else requestClose();
    }
  }

  onMount(() => {
    void load();
  });
  onDestroy(() => {
    view?.destroy();
    view = null;
  });
</script>

<div class="ed-shell" data-testid="code-editor" role="presentation" onkeydown={onKey}>
  <button class="ed-backdrop" aria-label="Close editor" tabindex="-1" onclick={onBackdrop}></button>
  <div class="ed-card" role="dialog" aria-modal="true" aria-label="Edit {path}">
    <div class="ed-head">
      <div class="ed-title" title={path}>
        <span class="ed-name">{fileBase(path)}</span>
        <span class="ed-path">{path}</span>
      </div>
      {#if roReason}<span class="ed-badge">Read-only</span>{/if}
      {#if savedFlash}<span class="ed-saved" data-testid="editor-saved">Saved</span>{/if}
      {#if !loadError}
        <button
          class="ed-toggle"
          class:on={vimOn}
          data-testid="editor-vim-toggle"
          aria-pressed={vimOn}
          title={vimOn ? "Vim keybindings on (:w save, :q close)" : "Turn on Vim keybindings"}
          onclick={toggleVim}
        >Vim</button>
      {/if}
      {#if onopenexternal}
        <button class="ed-btn ghost" data-testid="editor-open-external" onclick={() => onopenexternal?.(path)}>Open in default app</button>
      {/if}
      {#if !roReason && !loadError}
        <button class="ed-btn" data-testid="editor-save" disabled={!dirty || saving} onclick={() => save()}>
          {saving ? "Saving…" : "Save"}
        </button>
      {/if}
      <button
        class="ed-x {dirty ? 'dirty' : ''}"
        data-testid="editor-close"
        data-dirty={dirty ? "true" : "false"}
        aria-label={dirty ? "Unsaved changes. Close editor" : "Close editor"}
        title={dirty ? "Unsaved changes" : "Close (Esc)"}
        onclick={requestClose}
      >
        <span class="dot" aria-hidden="true">●</span><span class="x" aria-hidden="true">×</span>
      </button>
    </div>
    {#if roReason}<div class="ed-banner">{roReason}</div>{/if}
    {#if saveError}<div class="ed-banner err" data-testid="editor-error">{saveError}</div>{/if}
    {#if conflict}
      <div class="ed-banner warn" data-testid="editor-conflict">
        <span>This file changed on disk since you opened it.</span>
        <button class="ed-btn" data-testid="editor-overwrite" onclick={() => save(true)}>Overwrite</button>
        <button class="ed-btn ghost" data-testid="editor-reload" onclick={reloadFromDisk}>Reload (discard mine)</button>
        <button class="ed-btn ghost" onclick={() => (conflict = false)}>Cancel</button>
      </div>
    {/if}
    <div class="ed-body">
      {#if loading}<div class="ed-msg">Loading…</div>{/if}
      {#if loadError}
        <div class="ed-msg" data-testid="editor-load-error">
          <p>{loadError}</p>
          {#if onopenexternal}<button class="ed-btn" onclick={() => onopenexternal?.(path)}>Open in default app</button>{/if}
        </div>
      {/if}
      <div class="ed-cm" bind:this={host} class:hidden={loading || !!loadError}></div>
    </div>
    {#if !loading && !loadError}
      <div class="ed-foot">
        <span>Ln {cursor.line}, Col {cursor.col}</span>
        <span>{langName}</span>
        <span class="ed-hint">{dirty ? "Unsaved" : "Saved"} · {vimOn ? `:w or ${saveKey}` : saveKey} to save</span>
      </div>
    {/if}
    {#if confirmClose}
      <div class="ed-confirm-wrap">
        <div class="ed-confirm" role="alertdialog" aria-label="Unsaved changes" data-testid="editor-confirm-close">
          <p>Save changes to <b>{fileBase(path)}</b> before closing?</p>
          <div class="ed-confirm-row">
            <button class="ed-btn ghost" data-testid="editor-cancel-close" onclick={() => (confirmClose = false)}>Cancel</button>
            <button class="ed-btn ghost" data-testid="editor-discard" onclick={onclose}>Don't Save</button>
            <button class="ed-btn" data-testid="editor-save-close" onclick={saveAndClose}>Save</button>
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .ed-shell { position: fixed; inset: 0; z-index: 1100; display: grid; place-items: center; padding: 20px; }
  .ed-backdrop { position: absolute; inset: 0; border: 0; background: rgba(0, 0, 0, 0.45); cursor: default; }
  .ed-card {
    position: relative; width: min(80vw, 1200px); height: min(84vh, 920px); display: flex; flex-direction: column;
    background: var(--bg); color: var(--text); border: 1px solid var(--border-2); border-radius: 14px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.28); overflow: hidden;
  }
  @media (max-width: 700px) { .ed-card { width: 100%; height: 100%; } }
  .ed-head { display: flex; align-items: center; gap: 8px; padding: 10px 10px 10px 16px; border-bottom: 1px solid var(--border); }
  .ed-title { flex: 1; min-width: 0; display: flex; align-items: baseline; gap: 10px; }
  .ed-name { font-weight: 600; font-size: 14px; white-space: nowrap; }
  .ed-path { color: var(--muted); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ed-badge { font-size: 11px; padding: 2px 8px; border-radius: 999px; background: var(--chip); color: var(--muted); }
  .ed-saved { font-size: 12px; color: var(--ok); }
  .ed-btn {
    appearance: none; border: 1px solid transparent; background: var(--accent); color: var(--accent-fg);
    font: inherit; font-size: 12.5px; padding: 5px 12px; border-radius: 8px; cursor: pointer; white-space: nowrap;
  }
  .ed-btn:hover:not(:disabled) { background: var(--accent-hover); }
  .ed-btn:disabled { background: var(--send-disabled); cursor: default; }
  .ed-btn.ghost { background: var(--cancel-bg); color: var(--text); }
  .ed-btn.ghost:hover:not(:disabled) { background: var(--cancel-hover); }
  .ed-toggle {
    appearance: none; font: inherit; font-size: 12px; font-weight: 600; padding: 4px 10px; border-radius: 8px; cursor: pointer;
    border: 1px solid var(--border-2); background: transparent; color: var(--muted);
  }
  .ed-toggle:hover { background: var(--hover); color: var(--text); }
  .ed-toggle.on { background: color-mix(in srgb, var(--accent) 16%, transparent); border-color: var(--accent); color: var(--text); }
  /* Vim status panel (mode / pending keys / ex prompt) */
  .ed-cm :global(.cm-vim-panel) { padding: 2px 8px; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; }
  .ed-cm :global(.cm-vim-panel input) { background: transparent; color: var(--text); border: none; outline: none; font: inherit; }
  .ed-cm :global(.cm-fat-cursor) { background: color-mix(in srgb, var(--accent) 55%, transparent) !important; }
  /* VS Code tab affordance: ● while dirty, × on hover (or when clean) */
  .ed-x {
    width: 30px; height: 30px; border: none; background: transparent; border-radius: 50%; cursor: pointer;
    color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center;
  }
  .ed-x:hover { background: var(--hover); color: var(--text); }
  .ed-x .dot { display: none; font-size: 12px; color: var(--text); }
  .ed-x.dirty .dot { display: inline; }
  .ed-x.dirty .x { display: none; }
  .ed-x.dirty:hover .dot { display: none; }
  .ed-x.dirty:hover .x { display: inline; }
  .ed-banner { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 7px 16px; font-size: 12.5px; background: var(--item-bg); color: var(--text-dim); border-bottom: 1px solid var(--border); }
  .ed-banner.err { background: var(--err-bg); color: var(--err-text); }
  .ed-banner.warn { background: color-mix(in srgb, var(--accent) 12%, var(--bg)); color: var(--text); }
  .ed-body { position: relative; flex: 1; min-height: 0; }
  .ed-cm { position: absolute; inset: 0; -webkit-user-select: text; user-select: text; }
  .ed-cm.hidden { visibility: hidden; }
  .ed-msg { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: var(--muted); font-size: 13px; }
  .ed-msg p { margin: 0; }
  .ed-foot { display: flex; gap: 16px; padding: 5px 16px; font-size: 11.5px; color: var(--muted); border-top: 1px solid var(--border); }
  .ed-hint { margin-left: auto; }
  .ed-confirm-wrap { position: absolute; inset: 0; display: grid; place-items: center; background: rgba(0, 0, 0, 0.25); }
  .ed-confirm { background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; padding: 18px 20px; width: min(380px, 90%); box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25); }
  .ed-confirm p { margin: 0 0 14px; font-size: 14px; }
  .ed-confirm-row { display: flex; justify-content: flex-end; gap: 8px; }
</style>
