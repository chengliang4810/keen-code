import type { ConversationTool } from "@/modules/ai/lib/conversationPresentation";

export type ConversationToolFamily =
  | "read"
  | "search"
  | "edit"
  | "terminal"
  | "agent"
  | "todo"
  | "other";

export function conversationToolFamily(name: string): ConversationToolFamily {
  if (["read_file", "list_directory"].includes(name)) return "read";
  if (["grep", "glob"].includes(name)) return "search";
  if (["write_file", "edit", "multi_edit", "create_directory"].includes(name))
    return "edit";
  if (
    [
      "bash_run",
      "bash_background",
      "bash_logs",
      "bash_list",
      "bash_kill",
    ].includes(name)
  )
    return "terminal";
  if (
    [
      "run_subagent",
      "spawn_coding_agent",
      "read_agent_output",
      "send_to_agent",
    ].includes(name)
  )
    return "agent";
  if (name === "todo_write") return "todo";
  return "other";
}

export function conversationToolPath(tool: ConversationTool): string {
  const value = tool.output.path ?? tool.input.path ?? tool.input.file_path;
  return typeof value === "string" ? value : "";
}

export function conversationFilePresentation(
  path: string,
  workspaceRoot: string | null,
) {
  const normalized = path.replace(/\\/g, "/").replace(/^\/\/\?\//, "");
  const root = workspaceRoot
    ?.replace(/\\/g, "/")
    .replace(/^\/\/\?\//, "")
    .replace(/\/$/, "");
  const compare = (value: string) =>
    /^[a-z]:\//i.test(value) ? value.toLowerCase() : value;
  const relative =
    root && compare(normalized).startsWith(`${compare(root)}/`)
      ? normalized.slice(root.length + 1)
      : normalized;
  const split = relative.lastIndexOf("/");
  return {
    name: relative.slice(split + 1),
    parent: split < 0 ? "" : relative.slice(0, split),
  };
}

export function conversationToolSummary(tool: ConversationTool): string {
  const family = conversationToolFamily(tool.name);
  const path = conversationToolPath(tool);
  if (family === "read" || family === "edit") return path;
  if (family === "search") {
    const query =
      tool.input.pattern ?? tool.input.query ?? tool.input.glob ?? "";
    return [typeof query === "string" ? query : "", path]
      .filter(Boolean)
      .join(" · ");
  }
  const value =
    family === "terminal"
      ? (tool.input.description ?? tool.input.command ?? tool.input.handle)
      : (tool.input.description ??
        tool.input.agent ??
        tool.input.type ??
        tool.input.name);
  return typeof value === "string" || typeof value === "number"
    ? String(value)
    : "";
}

export function conversationToolResultText(tool: ConversationTool): string {
  if (tool.errorText) return tool.errorText;
  if (typeof tool.result === "string") return tool.result;
  const output = tool.output;
  const body = [output.stdout, output.stderr]
    .filter((v): v is string => typeof v === "string")
    .join("\n");
  if (body) return body;
  for (const key of ["content", "text", "output", "summary", "error"])
    if (typeof output[key] === "string") return output[key];
  return tool.result == null ? "" : JSON.stringify(tool.result, null, 2);
}

export type ConversationDiffLine = {
  id: string;
  kind: "added" | "removed";
  text: string;
};
export type ConversationDiff = {
  lines: ConversationDiffLine[];
  added: number;
  removed: number;
  truncated: boolean;
};

// 这里只展示工具提交的变更，不把“建议的行数”冒充磁盘 diff 或 Git 统计。
export function conversationToolDiff(
  tool: Pick<ConversationTool, "name" | "input">,
): ConversationDiff {
  const lines: ConversationDiffLine[] = [];
  const preview: ConversationDiff = {
    lines,
    added: 0,
    removed: 0,
    truncated: false,
  };
  let remaining = 64 * 1024;
  let block = 0;
  const append = (value: unknown, kind: ConversationDiffLine["kind"]) => {
    if (typeof value !== "string" || !value) return;
    const end = value.endsWith("\n") ? value.length - 1 : value.length;
    let offset = 0;
    let lineNumber = 0;
    do {
      const newline = value.indexOf("\n", offset);
      const boundary = newline < 0 ? end : Math.min(newline, end);
      preview[kind]++;
      if (lines.length < 400 && remaining > 0) {
        const text = value.slice(
          offset,
          Math.min(boundary, offset + remaining),
        );
        lines.push({ id: `${block}:${kind}:${lineNumber}`, kind, text });
        remaining -= text.length;
        if (text.length < boundary - offset) preview.truncated = true;
      } else preview.truncated = true;
      offset = boundary + 1;
      lineNumber++;
    } while (offset <= end);
    block++;
  };
  if (tool.name === "write_file") append(tool.input.content, "added");
  else if (tool.name === "edit") {
    append(tool.input.old_string, "removed");
    append(tool.input.new_string, "added");
  } else if (tool.name === "multi_edit" && Array.isArray(tool.input.edits)) {
    for (const edit of tool.input.edits) {
      if (!edit || typeof edit !== "object") continue;
      append(edit.old_string, "removed");
      append(edit.new_string, "added");
    }
  }
  return preview;
}

// 仅在详情打开后格式化内容；展示有界，原始工具结果仍由 UIMessage 保存。
export function boundedConversationToolText(value: string): {
  text: string;
  truncated: boolean;
} {
  const limit = 64 * 1024;
  return { text: value.slice(0, limit), truncated: value.length > limit };
}
