<script lang="ts">
  import type { Chat } from "$lib/store";

  let {
    leadDeleteAsk,
    pendingDeleteId = $bindable<string | null>(null),
    pendingMsgDelete = $bindable<number | null>(null),
    activeChat,
    pendingSaveConfirm,
    editingIndex,
    pendingSaveAction,
    onConfirmDelete,
    onConfirmMsgDelete,
    onCancelSaveConfirm,
    onConfirmSaveHere
  }: {
    leadDeleteAsk: { leadTitle: string; title: string; resolve: (ok: boolean) => void } | null;
    pendingDeleteId: string | null;
    pendingMsgDelete: number | null;
    activeChat: Chat | null;
    pendingSaveConfirm: boolean;
    editingIndex: number | null;
    pendingSaveAction: "regen" | "save";
    onConfirmDelete: () => void;
    onConfirmMsgDelete: () => void;
    onCancelSaveConfirm: () => void;
    onConfirmSaveHere: () => void;
  } = $props();
</script>

{#if leadDeleteAsk}
  <button class="confirm-backdrop" aria-label="Cancel" tabindex="-1" onclick={() => leadDeleteAsk?.resolve(false)}></button>
  <div class="confirm-card" data-testid="lead-delete-confirm" role="alertdialog" aria-modal="true" aria-label="Confirm lead delete">
    <p class="confirm-text">"{leadDeleteAsk.leadTitle}" wants to delete the session "{leadDeleteAsk.title}". Allow it?</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="lead-delete-deny" onclick={() => leadDeleteAsk?.resolve(false)}>Deny</button>
      <button class="confirm-ok" data-testid="lead-delete-allow" onclick={() => leadDeleteAsk?.resolve(true)}>Delete</button>
    </div>
  </div>
{/if}

{#if pendingDeleteId}
  <button
    class="confirm-backdrop"
    data-testid="delete-confirm-backdrop"
    aria-label="Cancel"
    onclick={() => (pendingDeleteId = null)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="delete-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm delete chat"
  >
    <p class="confirm-text">Are you sure you want to remove this chat?</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-delete-btn" onclick={() => (pendingDeleteId = null)}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-delete-btn" onclick={onConfirmDelete}>Confirm</button>
    </div>
  </div>
{/if}

{#if pendingMsgDelete != null && activeChat && activeChat.messages[pendingMsgDelete]}
  {@const delTarget = activeChat.messages[pendingMsgDelete]}
  {@const delCascadeNext = delTarget.role === "user" && activeChat.messages[pendingMsgDelete + 1]?.role === "assistant"}
  {@const delCascadePrev = delTarget.role === "assistant" && pendingMsgDelete > 0 && activeChat.messages[pendingMsgDelete - 1]?.role === "user"}
  <button
    class="confirm-backdrop"
    data-testid="delete-msg-backdrop"
    aria-label="Cancel"
    onclick={() => (pendingMsgDelete = null)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="delete-msg-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm delete message"
  >
    <p class="confirm-text">{delCascadeNext ? "Remove this message and the following reply?" : delCascadePrev ? "Remove this message and the question above it?" : "Are you sure you want to remove this message?"}</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-msg-delete-btn" onclick={() => (pendingMsgDelete = null)}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-msg-delete-btn" onclick={onConfirmMsgDelete}>Confirm</button>
    </div>
  </div>
{/if}

{#if pendingSaveConfirm && activeChat && editingIndex != null && editingIndex < activeChat.messages.length - 1}
  {@const saveAfterCount = activeChat.messages.length - 1 - editingIndex}
  <button
    class="confirm-backdrop"
    data-testid="edit-confirm-backdrop"
    aria-label="Cancel"
    onclick={onCancelSaveConfirm}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="edit-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm save"
  >
    <p class="confirm-text">{pendingSaveAction === "regen" ? "Regenerating" : "Saving"} here will permanently remove the {saveAfterCount} message{saveAfterCount === 1 ? "" : "s"} after it.</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-edit-choice-btn" onclick={onCancelSaveConfirm}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-save-btn" onclick={onConfirmSaveHere}>{pendingSaveAction === "regen" ? "Regenerate here" : "Save here"}</button>
    </div>
  </div>
{/if}

<style>
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
