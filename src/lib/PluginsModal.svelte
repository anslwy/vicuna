<script lang="ts">
  // Standalone Plugins modal (sidebar footer / collapsed rail) hosting PluginsPanel.
  import PluginsPanel from "$lib/PluginsPanel.svelte";
  import type { InstalledPlugin } from "$lib/pluginTheme";

  let {
    plugins,
    activeThemeKey,
    onChanged,
    onHarnessesChanged,
    onMcpChanged,
    onUseTheme,
    onOpenPanel,
    onClose
  }: {
    plugins: InstalledPlugin[];
    activeThemeKey: string | null;
    onChanged: (list: InstalledPlugin[]) => void;
    onHarnessesChanged: () => void;
    onMcpChanged?: () => void;
    onUseTheme: (pluginId: string, themeId: string) => void;
    onOpenPanel: (key: string) => void;
    onClose: () => void;
  } = $props();
</script>

<button
  class="confirm-backdrop"
  data-testid="plugins-backdrop"
  aria-label="Close plugins"
  onclick={onClose}
  tabindex="-1"
></button>
<div
  class="confirm-card settings-card"
  data-testid="plugins-modal"
  role="dialog"
  aria-modal="true"
  aria-label="Plugins"
>
  <button class="modal-x" data-testid="close-plugins-btn" aria-label="Close plugins" onclick={onClose}>×</button>
  <p class="confirm-text">Plugins</p>
  <PluginsPanel {plugins} {activeThemeKey} {onChanged} {onHarnessesChanged} {onMcpChanged} {onUseTheme} {onOpenPanel} />
</div>

<style>
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .settings-card { width: 80vw; max-width: 900px; height: 80vh; max-height: 820px; top: 50%; display: flex; flex-direction: column; overflow-y: auto; }
  @media (max-width: 700px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
