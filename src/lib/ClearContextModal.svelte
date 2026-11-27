<script lang="ts">
  // "/clear" confirmation: the agent starts fresh below a divider.
  let { onCancel, onConfirm }: { onCancel: () => void; onConfirm: () => void } = $props();
</script>

<button class="confirm-backdrop" data-testid="clear-context-backdrop" aria-label="Cancel clear context" onclick={onCancel} tabindex="-1"></button>
<div class="confirm-card" data-testid="clear-context-modal" role="dialog" aria-modal="true" aria-label="Clear context">
  <p class="confirm-text">Clear context?</p>
  <p class="confirm-copy">The agent starts fresh from here. Earlier messages stay visible above the line but are no longer sent to the model.</p>
  <div class="confirm-actions"><button class="confirm-cancel" onclick={onCancel}>Cancel</button><button class="confirm-ok" data-testid="confirm-clear-context" onclick={onConfirm}>Clear</button></div>
</div>

<style>
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-copy { color: var(--muted); font-size: 13px; line-height: 1.5; margin: -10px 0 18px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  @media (max-width: 700px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
