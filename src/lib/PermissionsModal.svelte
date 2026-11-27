<script lang="ts">
  // Per-session cross-session permissions (read / message / remote servers).
  import type { Chat } from "$lib/store";
  import { sessionPermissions } from "$lib/leadSessions";

  let {
    chat,
    busy,
    onSet,
    onClose
  }: {
    chat: Chat;
    busy: boolean;
    onSet: (key: "read" | "message" | "remote", value: boolean) => Promise<void>;
    onClose: () => void;
  } = $props();
</script>

<button class="confirm-backdrop" data-testid="permissions-backdrop" aria-label="Close permissions" onclick={onClose} tabindex="-1"></button>
<div class="confirm-card permissions-modal" role="dialog" aria-modal="true" aria-labelledby="permissions-title" data-testid="permissions-modal">
  <button class="modal-x" aria-label="Close permissions" onclick={onClose}>×</button>
  <h2 id="permissions-title">Permissions · {chat.title}</h2>
  <label class="permission-row"><input type="checkbox" data-testid="permission-read" checked={sessionPermissions(chat).read} disabled={busy} onchange={(e) => void onSet("read", e.currentTarget.checked)} /><span><b>Read other sessions</b><small>Read another session’s history or state.</small></span></label>
  <label class="permission-row"><input type="checkbox" data-testid="permission-message" checked={sessionPermissions(chat).message} disabled={busy} onchange={(e) => void onSet("message", e.currentTarget.checked)} /><span><b>Message other sessions</b><small>Send messages to other sessions and start turns.</small></span></label>
  <label class="permission-row"><input type="checkbox" data-testid="permission-remote" checked={sessionPermissions(chat).remote} disabled={busy} onchange={(e) => void onSet("remote", e.currentTarget.checked)} /><span><b>Use sessions on other servers</b><small>List, read, search, and (with Message other sessions) message sessions on servers saved on this machine. As the lead, also start, stop and archive sessions there{chat.role === "lead" ? "" : " (creating/stopping/archiving needs the lead)"}.</small></span></label>
</div>

<style>
  .confirm-card.permissions-modal { top: 50%; width: min(520px, calc(100vw - 48px)); padding: 24px; border: 1px solid var(--border); border-radius: 18px; background: var(--card); }
  .confirm-card.permissions-modal h2 { margin: 0 36px 22px 0; font-size: 18px; }
  .permission-row { display: flex; align-items: flex-start; gap: 12px; padding: 14px 0; border-top: 1px solid var(--border); cursor: pointer; }
  .permission-row input { margin-top: 3px; accent-color: var(--accent); }
  .permission-row span { display: grid; gap: 4px; }
  .permission-row small { color: var(--muted); }
  input { -webkit-user-select: text; user-select: text; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  @media (max-width: 700px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
