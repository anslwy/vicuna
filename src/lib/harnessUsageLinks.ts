/** Provider usage pages (manual resets, plan upgrades). */
export function harnessUsageUrl(engine: string): string | null {
  if (engine === "claude") return "https://claude.ai/new#settings/usage";
  if (engine === "codex") return "https://chatgpt.com/settings/usage?tab=overview";
  if (engine === "agy") return "https://antigravity.google.com/remote?settingsOpen=true&settingsScreen=Models";
  return null;
}
