// Vicuna's bridge extension for pi. Vicuna writes this file to
// ~/vicuna/config/pi/vicuna-bridge.ts and loads it with `pi -e` on every Work
// turn; nothing in the user's ~/.pi is changed.
//
// - MCP handover: VICUNA_PI_MCP names a JSON file `{name: mcpServers entry}`
//   (the chat's MCP servers); each is registered for this session only, with
//   direct exposure so the model sees the tools without a search step.
// - Approval: with VICUNA_PI_ASK=1 every tool call waits on an RPC
//   `select` whose title starts with `vicuna-approve:` + JSON; Vicuna answers
//   Allow / Always allow / Deny from the chat's ask card. Deny blocks the call
//   (pi reports it to the model as a failed tool result, the turn goes on).
import { readFileSync } from "node:fs";

export default function (pi: any) {
  const mcpFile = process.env.VICUNA_PI_MCP;
  if (mcpFile) {
    try {
      const servers = JSON.parse(readFileSync(mcpFile, "utf8"));
      for (const [name, cfg] of Object.entries(servers)) {
        try {
          pi.registerMcpServer(name, { exposure: "direct", ...(cfg as object) });
        } catch (e) {
          console.error(`vicuna: MCP server ${name} not registered: ${e}`);
        }
      }
    } catch (e) {
      console.error(`vicuna: cannot read ${mcpFile}: ${e}`);
    }
  }
  const preApproved = (process.env.VICUNA_PI_ALLOW || "").split(",").filter(Boolean);
  pi.on("tool_call", async (event: any, ctx: any) => {
    if (process.env.VICUNA_PI_ASK !== "1" || preApproved.includes(event.toolName)) return undefined;
    const title = "vicuna-approve:" + JSON.stringify({ tool: event.toolName, id: event.toolCallId, input: event.input });
    const choice = await ctx.ui.select(title, ["Allow", "Always allow", "Deny"]);
    if (choice === "Allow" || choice === "Always allow") return undefined;
    return { block: true, reason: "The user denied this tool call in Vicuna." };
  });
}
