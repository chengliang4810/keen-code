import { buildMemoryTools } from "@/modules/ai/tools/memory";
import { buildManagedAgentTools } from "./agent";
import { buildEditTools } from "./edit";
import { buildFsTools } from "./fs";
import { buildSearchTools } from "./search";
import { buildShellTools } from "./shell";
import { buildSubagentTools } from "./subagent";
import { buildTerminalTools } from "./terminal";
import { buildTodoTools } from "./todo";

export { resolvePath, type ToolContext } from "./context";

export function buildTools(
  ctx: import("./context").ToolContext,
  options?: import("@/modules/ai/lib/agent").RunAgentOptions,
) {
  return {
    ...buildMemoryTools(options?.memory),
    ...buildFsTools(ctx),
    ...buildEditTools(ctx),
    ...buildSearchTools(ctx),
    ...buildShellTools(ctx),
    ...buildSubagentTools(ctx, options),
    ...buildTerminalTools(ctx),
    ...buildTodoTools(ctx),
    ...buildManagedAgentTools(ctx),
  } as const;
}

export type ChatTools = ReturnType<typeof buildTools>;
