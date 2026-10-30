// Built-in MCP gallery for the Work-mode popup (first tab).
//
// Every entry is one-click: local commands spawn with zero input, remote
// URLs are OAuth endpoints that sign in through the browser flow (or need
// no auth at all, like deepwiki). Nothing here needs an API key typed in,
// so Add writes straight to ~/vicuna/config/vicuna.json and connects.
// Icons are generic hand-drawn glyphs, not vendor logos.

import type { McpServerConfig } from "./mcp";

export interface BuiltinMcp {
  /** stable key for selection state */
  key: string;
  /** display name — also the vicuna.json map key when added */
  name: string;
  blurb: string;
  transport: "stdio" | "http";
  command?: string[];
  url?: string;
  /** inner SVG for a 24x24 stroke icon (currentColor, fill none) */
  icon: string;
}

export const BUILT_IN_MCPS: BuiltinMcp[] = [
  {
    key: "searchapi",
    name: "searchapi",
    blurb: "Real-time web search, flights, hotels",
    transport: "http",
    url: "https://www.searchapi.io/mcp",
    icon: '<circle cx="11" cy="11" r="7"/><line x1="16.5" y1="16.5" x2="21" y2="21"/>'
  },
  {
    key: "browsermcp",
    name: "browsermcp",
    blurb: "Automate Chromium via extension",
    transport: "stdio",
    command: ["npx", "@browsermcp/mcp@latest"],
    icon: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17"/><path d="M12 3.5c3.2 3.6 3.2 13.4 0 17"/><path d="M12 3.5c-3.2 3.6-3.2 13.4 0 17"/>'
  },
  {
    key: "open-computer-use",
    name: "open-computer-use",
    blurb: "Full OS control: mouse, keyboard, screen",
    transport: "stdio",
    command: ["open-computer-use", "mcp"],
    icon: '<rect x="3" y="4" width="18" height="12" rx="2"/><line x1="9" y1="20" x2="15" y2="20"/><line x1="12" y1="16" x2="12" y2="20"/><path d="M9.5 8.5l3.5 3.5.7-2.3 2.3-.7z"/>'
  },
  {
    key: "playwright",
    name: "playwright",
    blurb: "Script real browsers with snapshots",
    transport: "stdio",
    command: ["npx", "@playwright/mcp@latest"],
    icon: '<rect x="3" y="5" width="18" height="14" rx="3"/><path d="M10.5 9.3l5 2.7-5 2.7z"/>'
  },
  {
    key: "deepwiki",
    name: "deepwiki",
    blurb: "Ask questions of any public repo",
    transport: "http",
    url: "https://mcp.deepwiki.com/mcp",
    icon: '<path d="M12 6.5C10 5 7 4.5 4 4.5v14c3 0 6 .5 8 2 2-1.5 5-2 8-2v-14c-3 0-6 .5-8 2z"/><line x1="12" y1="6.5" x2="12" y2="20.5"/>'
  },
  {
    key: "github",
    name: "github",
    blurb: "Repos, issues, PRs and actions",
    transport: "http",
    url: "https://api.githubcopilot.com/mcp/",
    icon: '<circle cx="6" cy="6" r="2.4"/><circle cx="6" cy="18" r="2.4"/><circle cx="18" cy="8" r="2.4"/><path d="M6 8.4v7.2"/><path d="M18 10.4c0 3.6-5.5 2.9-9.2 5.4"/>'
  },
  {
    key: "linear",
    name: "linear",
    blurb: "Issues, projects and cycles",
    transport: "http",
    url: "https://mcp.linear.app/mcp",
    icon: '<path d="M4 7.5l8-3.5 8 3.5-8 3.5z"/><path d="M4 12.5l8 3.5 8-3.5"/><path d="M4 17.5l8 3.5 8-3.5"/>'
  },
  {
    key: "notion",
    name: "notion",
    blurb: "Search, read and write pages",
    transport: "http",
    url: "https://mcp.notion.com/mcp",
    icon: '<path d="M6 3h8.5L20 8.5V21H6z"/><path d="M14.5 3v5.5H20"/><line x1="9" y1="13" x2="17" y2="13"/><line x1="9" y1="16.5" x2="17" y2="16.5"/>'
  },
  {
    key: "higgsfield",
    name: "higgsfield",
    blurb: "Generate video from text",
    transport: "http",
    url: "https://mcp.higgsfield.ai/mcp",
    icon: '<path d="M12 3.5l1.7 5 5 1.7-5 1.7-1.7 5-1.7-5-5-1.7 5-1.7z"/><path d="M18.5 15.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z"/>'
  }
];

/** Config entry an Add writes to vicuna.json (enabled, straight to connect). */
export function builtinToConfig(b: BuiltinMcp): McpServerConfig {
  return {
    id: b.name,
    name: b.name,
    transport: b.transport,
    command: b.command ? [...b.command] : undefined,
    env: undefined,
    url: b.url,
    headers: undefined,
    enabled: true
  };
}
