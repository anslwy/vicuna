/** Provider usage pages with manual reset controls. */
export function harnessUsageUrl(engine: string): string | null {
  if (engine === "claude") return "https://claude.ai/new#settings/usage";
  if (engine === "codex") return "https://chatgpt.com/settings/usage?tab=overview";
  return null;
}
