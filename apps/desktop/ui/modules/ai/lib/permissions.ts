import type { ToolSet } from "ai";

export const PERMISSION_MODES = ["ask", "edit", "full-access"] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export function normalizePermissionMode(value: unknown): PermissionMode {
  return value === "full-access" ? value : "edit";
}

const FILE_MUTATIONS = new Set([
  "write_file",
  "edit",
  "multi_edit",
  "create_directory",
]);

const READ_ONLY_TOOLS = new Set([
  "memory_read",
  "read_file",
  "list_directory",
  "grep",
  "glob",
  "get_terminal_output",
  "read_agent_output",
  "suggest_command",
  "open_preview",
]);

export function applyToolPermissions<T extends ToolSet>(
  tools: T,
  permissionMode: PermissionMode = "ask",
  planMode = false,
): T {
  return Object.fromEntries(
    Object.entries(tools)
      .filter(([name]) => !planMode || READ_ONLY_TOOLS.has(name))
      .map(([name, tool]) => {
        if (READ_ONLY_TOOLS.has(name)) return [name, tool];
        const needsApproval =
          permissionMode !== "full-access" &&
          !(permissionMode === "edit" && FILE_MUTATIONS.has(name));
        return [name, { ...tool, needsApproval }];
      }),
  ) as T;
}
