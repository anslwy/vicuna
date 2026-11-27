<script lang="ts">
  // Empty thread / home starter: Chat|Work toggle, title, Work engine line and
  // the suggestion cards. ChatApp owns the mode state and applies the picks.
  import { SUGGESTIONS, WORK_SUGGESTIONS } from "$lib/chatAppData";
  import type { WorkEngineInfo } from "$lib/work";

  let {
    pendingMode,
    composerMode,
    workEngineInfo,
    workEngineLabel,
    onPickChat,
    onPickWork,
    onSuggestion
  }: {
    pendingMode: "chat" | "work";
    composerMode: "chat" | "work";
    workEngineInfo: WorkEngineInfo | null;
    workEngineLabel: string;
    onPickChat: () => void;
    onPickWork: () => void;
    onSuggestion: (prompt: string) => void;
  } = $props();
</script>

<div class="empty">
  <div class="seg mode-toggle" role="tablist" aria-label="Session mode" data-testid="mode-toggle">
    <button class="seg-btn {pendingMode === 'chat' ? 'sel' : ''}" data-testid="mode-chat-btn" role="tab" onclick={onPickChat}>Chat</button>
    <button class="seg-btn {pendingMode === 'work' ? 'sel' : ''}" data-testid="mode-work-btn" role="tab" onclick={onPickWork}>Work</button>
  </div>
  <h1 class="empty-title">{pendingMode === "work" ? "What should I work on?" : "How can I help you today?"}</h1>
  {#if pendingMode === "work"}
    <p class="engine-line" data-testid="work-engine-line">
      {#if workEngineInfo?.installed}
        Engine: {workEngineLabel} {workEngineInfo.version ?? ""}
      {:else if workEngineInfo}
        Engine: {workEngineLabel} not found
      {:else}
        Engine: checking…
      {/if}
    </p>
  {/if}
  <div class="suggest-grid" data-testid="empty-suggestions">
    {#each composerMode === "work" ? WORK_SUGGESTIONS : SUGGESTIONS as s, idx}
      <button class="suggest-card" data-testid="suggest-card" onclick={() => onSuggestion(s.prompt)}>
        <span class="suggest-text">
          <span class="suggest-title">{s.title}</span>
          <span class="suggest-sub">{s.prompt}</span>
        </span>
        <span class="suggest-icon">{@html s.icon}</span>
      </button>
    {/each}
  </div>
</div>

<style>
  .empty { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; padding: 24px; overflow-y: auto; }
  .empty-title { font-size: 24px; font-weight: 500; color: var(--text-dim); margin: 0 0 28px 0; text-align: center; }
  .suggest-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; width: 100%; max-width: 860px; }
  .suggest-card { display: flex; flex-direction: row; align-items: center; justify-content: space-between; gap: 12px; text-align: left; background: var(--card); border: 1px solid var(--border); border-radius: 16px; padding: 14px 14px 14px 18px; cursor: pointer; min-width: 0; }
  .suggest-card:hover { box-shadow: 0 4px 16px rgba(0,0,0,.08); border-color: var(--accent); }
  .suggest-text { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .suggest-title { font-size: 15px; font-weight: 650; color: var(--text); }
  .suggest-sub { font-size: 13.5px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .suggest-icon { flex-shrink: 0; width: 36px; height: 36px; border-radius: 10px; background: var(--accent); color: var(--accent-fg); display: flex; align-items: center; justify-content: center; }
  .seg { display: flex; background: var(--hover); border-radius: 12px; padding: 3px; gap: 2px; }
  .seg-btn { border: none; background: transparent; cursor: pointer; font: inherit; font-size: 13.5px; color: var(--text-dim); padding: 6px 16px; border-radius: 9px; }
  .seg-btn.sel { background: var(--card); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
  .mode-toggle { margin-bottom: 18px; }
  .mode-toggle .seg-btn { min-width: 90px; }
  .mode-toggle .seg-btn.sel { background: var(--accent); color: var(--accent-fg); box-shadow: none; font-weight: 700; }
  .engine-line { margin: -6px 0 18px; color: var(--muted-2); font-size: 12px; }
  @media (max-width: 700px) {
    .suggest-grid { grid-template-columns: 1fr; }
    .empty-title { font-size: 20px; margin-bottom: 20px; }
  }
</style>
