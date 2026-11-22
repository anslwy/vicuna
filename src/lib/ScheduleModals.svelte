<script lang="ts">
  import { dateInputValue, formatWhen, previewText, type ScheduledItem } from "$lib/schedule";

  export type ScheduleCustomDraft = {
    rescheduleId: string | null;
    date: string;
    time: string;
    error: string | null;
  };

  let {
    scheduleListOpen = $bindable(false),
    quitPromptOpen = $bindable(false),
    scheduleCustom = $bindable<ScheduleCustomDraft | null>(null),
    scheduled,
    scheduledSorted,
    scheduleNow,
    scheduleNote,
    scheduleTz,
    serverPersist,
    scheduleTarget,
    onDeliverScheduled,
    onOpenCustom,
    onRemoveScheduled,
    onConfirmCustom,
    onQuitAnyway
  }: {
    scheduleListOpen: boolean;
    quitPromptOpen: boolean;
    scheduleCustom: ScheduleCustomDraft | null;
    scheduled: ScheduledItem[];
    scheduledSorted: ScheduledItem[];
    scheduleNow: number;
    scheduleNote: string;
    scheduleTz: string;
    serverPersist: boolean;
    scheduleTarget: (it: ScheduledItem) => string;
    onDeliverScheduled: (id: string) => void;
    onOpenCustom: (id: string) => void;
    onRemoveScheduled: (id: string) => void;
    onConfirmCustom: () => void;
    onQuitAnyway: () => void;
  } = $props();
</script>

{#snippet scheduledRows(actions: boolean)}
  <div class="sched-list" data-testid="scheduled-list">
    {#each scheduledSorted as it (it.id)}
      <div class="sched-row {it.missed ? 'missed' : ''}" data-testid="scheduled-row">
        <div class="sched-row-main">
          <div class="sched-row-when">
            {#if it.missed}<span class="sched-missed-tag">{it.error ? "Not sent" : "Missed"}</span>{/if}
            {formatWhen(it.at, scheduleNow)}
            <span class="sched-row-target">· {scheduleTarget(it)}</span>
          </div>
          <div class="sched-row-text">{previewText(it, 140)}{#if it.images?.length && it.text}{" "}· {it.images.length} image{it.images.length === 1 ? "" : "s"}{/if}</div>
          {#if it.error}<div class="sched-row-err">{it.error}</div>{/if}
        </div>
        {#if actions}
          <div class="sched-row-actions">
            <button class="mini-btn primary" data-testid="scheduled-send-now" onclick={() => onDeliverScheduled(it.id)}>Send now</button>
            <button class="mini-btn" data-testid="scheduled-reschedule" onclick={() => onOpenCustom(it.id)}>Reschedule</button>
            <button class="mini-btn danger" data-testid="scheduled-delete" onclick={() => onRemoveScheduled(it.id)}>Delete</button>
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/snippet}

{#if scheduleListOpen}
  <button class="confirm-backdrop sched-dim" data-testid="scheduled-backdrop" aria-label="Close" onclick={() => (scheduleListOpen = false)} tabindex="-1"></button>
  <div class="confirm-card sched-card" data-testid="scheduled-dialog" role="dialog" aria-modal="true" aria-label="Scheduled messages">
    <button class="modal-x" data-testid="scheduled-close" aria-label="Close" onclick={() => (scheduleListOpen = false)}>×</button>
    <p class="confirm-text sched-title">Scheduled messages</p>
    <p class="sched-note" data-testid="scheduled-note">{scheduleNote}{serverPersist ? "" : " Closing the window keeps it running in the tray; quitting does not."}</p>
    {#if scheduled.some((x) => x.missed)}
      <p class="sched-warn" data-testid="scheduled-missed-note">Some messages were not sent at their time. Send them now, pick a new time, or delete them.</p>
    {/if}
    {#if scheduled.length === 0}
      <p class="sched-empty" data-testid="scheduled-empty">Nothing scheduled. In Work mode, use the arrow next to the send button to schedule a message.</p>
    {:else}
      {@render scheduledRows(true)}
    {/if}
  </div>
{/if}

{#if quitPromptOpen}
  <button class="confirm-backdrop sched-dim" data-testid="quit-prompt-backdrop" aria-label="Cancel" onclick={() => (quitPromptOpen = false)} tabindex="-1"></button>
  <div class="confirm-card sched-card" data-testid="quit-prompt" role="alertdialog" aria-modal="true" aria-label="Quit Vicuna">
    <p class="confirm-text sched-title">Quit Vicuna?</p>
    <p class="sched-warn" data-testid="quit-prompt-warning">{scheduled.length === 1 ? "1 scheduled message" : `${scheduled.length} scheduled messages`} will not be sent while Vicuna is closed. {scheduled.length === 1 ? "It stays" : "They stay"} saved, and Vicuna will ask what to do with {scheduled.length === 1 ? "it" : "them"} the next time it opens.</p>
    {@render scheduledRows(false)}
    <div class="confirm-actions sched-actions">
      <button class="confirm-cancel" data-testid="quit-prompt-cancel" onclick={() => (quitPromptOpen = false)}>Keep running</button>
      <button class="confirm-ok sched-quit" data-testid="quit-prompt-confirm" onclick={onQuitAnyway}>Quit anyway</button>
    </div>
  </div>
{/if}

{#if scheduleCustom}
  {@const sc = scheduleCustom}
  <button class="confirm-backdrop sched-dim sched-top" data-testid="schedule-custom-backdrop" aria-label="Cancel" onclick={() => (scheduleCustom = null)} tabindex="-1"></button>
  <div class="confirm-card sched-custom sched-top" data-testid="schedule-custom-dialog" role="dialog" aria-modal="true" aria-label="Schedule message">
    <button class="modal-x" data-testid="schedule-custom-close" aria-label="Close" onclick={() => (scheduleCustom = null)}>×</button>
    <p class="confirm-text sched-title">{sc.rescheduleId ? "Reschedule message" : "Schedule message"}</p>
    {#if scheduleTz}<p class="sched-note">Time zone: {scheduleTz.replace(/_/g, " ")}</p>{/if}
    <div class="sched-fields">
      <input type="date" class="sched-input" data-testid="schedule-date" aria-label="Date" min={dateInputValue(scheduleNow)} value={sc.date} oninput={(e) => (scheduleCustom = { ...sc, date: e.currentTarget.value, error: null })} />
      <input type="time" class="sched-input" data-testid="schedule-time" aria-label="Time" value={sc.time} oninput={(e) => (scheduleCustom = { ...sc, time: e.currentTarget.value, error: null })} onkeydown={(e) => { if (e.key === "Enter") onConfirmCustom(); }} />
    </div>
    {#if sc.error}<p class="sched-row-err" data-testid="schedule-custom-error">{sc.error}</p>{/if}
    <p class="sched-note">{scheduleNote}</p>
    <div class="confirm-actions sched-actions">
      <button class="confirm-cancel" data-testid="schedule-custom-cancel" onclick={() => (scheduleCustom = null)}>Cancel</button>
      <button class="confirm-ok" data-testid="schedule-custom-confirm" onclick={onConfirmCustom}>{sc.rescheduleId ? "Reschedule" : "Schedule message"}</button>
    </div>
  </div>
{/if}

<style>
  input { -webkit-user-select: text; user-select: text; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-backdrop.sched-dim { background: rgba(0,0,0,.35); }
  .sched-top.confirm-backdrop { z-index: 52; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .sched-top.confirm-card { z-index: 53; }
  .confirm-card.sched-card { width: min(560px, calc(100vw - 48px)); max-height: 76vh; top: 50%; display: flex; flex-direction: column; }
  .confirm-card.sched-custom { width: min(420px, calc(100vw - 48px)); }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-text.sched-title { margin-bottom: 6px; }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  .confirm-ok.sched-quit { background: var(--danger); color: #fff; }
  .confirm-ok.sched-quit:hover { background: var(--danger); filter: brightness(.93); }
  .sched-note { font-size: 12.5px; line-height: 1.45; color: var(--muted); margin: 0 0 12px 0; }
  .sched-warn { font-size: 13px; line-height: 1.45; color: var(--danger); margin: 0 0 12px 0; }
  .sched-empty { font-size: 13.5px; line-height: 1.5; color: var(--text-dim); margin: 6px 0 4px 0; }
  .sched-list { display: flex; flex-direction: column; gap: 8px; overflow-y: auto; min-height: 0; margin: 0 -6px; padding: 0 6px 2px 6px; }
  .sched-row { border: 1px solid var(--border); border-radius: 14px; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; }
  .sched-row.missed { border-color: var(--danger); }
  .sched-row-main { min-width: 0; }
  .sched-row-when { font-size: 13px; font-weight: 600; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sched-row-target { font-weight: 400; color: var(--muted); }
  .sched-missed-tag { display: inline-block; font-size: 11px; font-weight: 600; color: #fff; background: var(--danger); border-radius: 6px; padding: 1px 6px; margin-right: 6px; }
  .sched-row-text { font-size: 13.5px; line-height: 1.45; color: var(--text-dim); margin-top: 3px; overflow-wrap: anywhere; user-select: text; }
  .sched-row-err { font-size: 12.5px; color: var(--danger); margin: 4px 0 0 0; overflow-wrap: anywhere; }
  .sched-row-actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .sched-fields { display: flex; gap: 10px; margin: 4px 0 12px 0; }
  .sched-input { flex: 1; min-width: 0; box-sizing: border-box; border: 1px solid var(--border-strong); border-radius: 12px; padding: 9px 12px; font: inherit; font-size: 14.5px; background: var(--card); color: var(--text); color-scheme: light dark; }
  .sched-input:focus { outline: none; border-color: var(--accent); }
  .sched-actions { margin-top: 14px; }
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .mini-btn.primary { background: var(--accent); border-color: transparent; color: var(--accent-fg); }
  .mini-btn.primary:hover:not(:disabled) { background: var(--accent-hover); }
  .mini-btn.danger { border-color: var(--danger); color: var(--danger); }
  .mini-btn.danger:hover:not(:disabled) { background: var(--err-bg); }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
