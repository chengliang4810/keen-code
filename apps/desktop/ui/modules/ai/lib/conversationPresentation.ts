import type { UIMessage } from "ai";
import type { GitStatusSnapshot } from "@/modules/ai/lib/native";

export type ConversationTurn = {
  id: string;
  messages: readonly UIMessage[];
};

export type ConversationEntry = {
  key: string;
  messageId: string;
  part: UIMessage["parts"][number];
};

export type ConversationPartGroup = {
  kind: "work" | "response" | "approval" | "notice";
  key: string;
  entries: ConversationEntry[];
};

export type ConversationWorkState =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type ConversationWork = {
  id: string;
  kind: "terminal" | "agent";
  title: string;
  state: ConversationWorkState;
  detail?: string;
  durationMs?: number;
  handle?: string;
};

export type ConversationFile = {
  path: string;
  state: "pending" | "applied" | "failed";
};

export type ConversationStatus = {
  files: ConversationFile[];
  terminals: ConversationWork[];
  agents: ConversationWork[];
};

export type ConversationTool = {
  name: string;
  id: string;
  state: string;
  input: Record<string, unknown>;
  output: Record<string, unknown>;
  result: unknown;
  errorText?: string;
  cancelled?: boolean;
};

const NAMES: Record<string, string> = {
  Read: "read_file",
  Write: "write_file",
  Edit: "edit",
  MultiEdit: "multi_edit",
  Bash: "bash_run",
  PowerShell: "bash_run",
  Glob: "glob",
  Grep: "grep",
  run_command: "bash_run",
  shell_session_run: "bash_run",
};

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

export function conversationTool(
  part: UIMessage["parts"][number],
): ConversationTool | null {
  const value = part as unknown as Record<string, unknown>;
  const name =
    part.type === "dynamic-tool"
      ? text(value.toolName)
      : part.type.startsWith("tool-")
        ? part.type.slice(5)
        : undefined;
  if (!name) return null;
  return {
    name: NAMES[name] ?? name,
    id: text(value.toolCallId) ?? "",
    state: text(value.state) ?? "input-streaming",
    input: record(value.input),
    output: record(value.output),
    result: value.output,
    errorText: text(value.errorText),
    cancelled:
      record(record(value.resultProviderMetadata).rcode).cancelled === true,
  };
}

// 完成的轮次复用引用，流式增量只使当前轮的 React 子树失效。
export function groupConversationTurns(
  messages: readonly UIMessage[],
  previous: readonly ConversationTurn[] = [],
): ConversationTurn[] {
  const turns: ConversationTurn[] = [];
  let current: UIMessage[] = [];
  const prior = new Map(previous.map((turn) => [turn.id, turn]));
  const flush = () => {
    if (!current.length) return;
    const id = current[0].id;
    const existing = prior.get(id);
    turns.push(
      existing &&
        existing.messages.length === current.length &&
        current.every((message, i) => message === existing.messages[i])
        ? existing
        : { id, messages: current },
    );
    current = [];
  };
  for (const message of messages) {
    if (message.role === "system") continue;
    if (message.role === "user") flush();
    current.push(message);
  }
  flush();
  return turns;
}

export function presentConversationTurn(
  turn: ConversationTurn,
): ConversationPartGroup[] {
  const entries: ConversationEntry[] = [];
  for (const message of turn.messages) {
    if (message.role !== "assistant") continue;
    message.parts.forEach((part, index) => {
      if (
        part.type === "text" ||
        part.type === "reasoning" ||
        part.type === "file" ||
        part.type === "data-rcode-diagnostic" ||
        conversationTool(part)
      ) {
        const tool = conversationTool(part);
        entries.push({
          key: `${message.id}:${tool?.id || index}`,
          messageId: message.id,
          part,
        });
      }
    });
  }

  // 只有执行过程之后的文本才是最终回复，工具前的说明不能提前提升为结果。
  let responseStart = entries.length;
  for (let i = entries.length - 1; i >= 0; i -= 1) {
    if (entries[i].part.type === "data-rcode-diagnostic") continue;
    if (entries[i].part.type !== "text" && entries[i].part.type !== "file")
      break;
    responseStart = i;
  }

  const groups: ConversationPartGroup[] = [];
  entries.forEach((entry, i) => {
    const tool = conversationTool(entry.part);
    const kind =
      tool?.state === "approval-requested"
        ? "approval"
        : entry.part.type === "data-rcode-diagnostic"
          ? "notice"
          : i >= responseStart
            ? "response"
            : "work";
    const last = groups[groups.length - 1];
    if (last?.kind === kind && kind !== "approval") {
      last.entries.push(entry);
    } else {
      groups.push({ kind, key: entry.key, entries: [entry] });
    }
  });
  return groups;
}

export function conversationWorkState(
  tool: ConversationTool,
): ConversationWorkState {
  if (tool.state === "output-denied" || tool.cancelled) return "cancelled";
  if (
    tool.state === "output-error" ||
    tool.output.error ||
    tool.output.is_error === true ||
    tool.output.timed_out === true ||
    (typeof tool.output.exit_code === "number" && tool.output.exit_code !== 0)
  )
    return "failed";
  if (tool.state === "output-available") return "completed";
  if (tool.state === "approval-responded") {
    return "pending";
  }
  if (tool.state === "input-streaming" || tool.state === "approval-requested")
    return "pending";
  return "running";
}

export function buildConversationStatus(
  messages: readonly UIMessage[],
): ConversationStatus {
  const files = new Map<string, ConversationFile>();
  const terminals = new Map<string, ConversationWork>();
  const agents = new Map<string, ConversationWork>();
  const backgroundByHandle = new Map<string, ConversationWork>();
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    for (const part of message.parts) {
      const tool = conversationTool(part);
      if (!tool) continue;
      const state = conversationWorkState(tool);
      if (["write_file", "edit", "multi_edit"].includes(tool.name)) {
        const path =
          text(tool.output.path) ??
          text(tool.input.path) ??
          text(tool.input.file_path);
        if (
          path &&
          state !== "cancelled" &&
          !tool.output.queued_for_plan_review
        ) {
          const key = path.replace(/\\/g, "/");
          const old = files.get(key);
          // 后续审批或失败不会抹掉同一路径先前已经落盘的事实。
          if (old?.state !== "applied") {
            files.set(key, {
              path,
              state:
                state === "completed"
                  ? "applied"
                  : state === "failed"
                    ? "failed"
                    : "pending",
            });
          }
        }
      }
      if (tool.name === "bash_run" || tool.name === "bash_background") {
        const title = text(tool.input.command) ?? text(tool.output.command);
        if (!title) continue;
        const handle =
          typeof tool.output.handle === "string"
            ? tool.output.handle
            : undefined;
        const work: ConversationWork = {
          id: tool.id || `${message.id}:${terminals.size}`,
          kind: "terminal",
          title,
          state:
            tool.name === "bash_background" &&
            state === "completed" &&
            handle !== undefined
              ? "running"
              : state,
          detail:
            text(tool.output.stderr) ??
            text(tool.output.stdout) ??
            text(tool.result) ??
            tool.errorText,
          handle,
        };
        terminals.set(work.id, work);
        if (handle !== undefined) backgroundByHandle.set(handle, work);
      } else if (tool.name === "run_subagent") {
        const title =
          text(tool.input.description) ??
          text(tool.input.task) ??
          text(tool.input.type) ??
          text(tool.input.agent);
        if (!title) continue;
        const work: ConversationWork = {
          id: tool.id || `${message.id}:${agents.size}`,
          kind: "agent",
          title,
          state,
          detail: text(tool.output.summary) ?? tool.errorText,
          durationMs:
            typeof tool.output.durationMs === "number"
              ? tool.output.durationMs
              : undefined,
        };
        agents.set(work.id, work);
      }
      // 后台启动工具完成表示进程已启动；只有日志、目录或停止结果能确认退出。
      if (tool.state === "output-available" && !tool.output.error) {
        if (tool.name === "bash_logs" || tool.name === "bash_kill") {
          const work = backgroundByHandle.get(String(tool.input.handle));
          if (work) {
            if (tool.name === "bash_kill") work.state = "cancelled";
            else if (tool.output.exited === true) {
              work.state =
                typeof tool.output.exit_code === "number" &&
                tool.output.exit_code !== 0
                  ? "failed"
                  : "completed";
            }
            work.detail = text(tool.output.bytes) ?? work.detail;
          }
        } else if (
          tool.name === "bash_list" &&
          Array.isArray(tool.output.processes)
        ) {
          for (const process of tool.output.processes) {
            const value = record(process);
            const work = backgroundByHandle.get(String(value.handle));
            if (work && value.exited === true)
              work.state =
                typeof value.exit_code === "number" && value.exit_code !== 0
                  ? "failed"
                  : "completed";
          }
        }
      }
    }
  }
  return {
    files: [...files.values()],
    terminals: [...terminals.values()],
    agents: [...agents.values()],
  };
}

// 文本 token 与大文件内容不改变状态摘要；缓存只比较会影响面板的工具字段。
export function createConversationStatusSelector() {
  const cache = new WeakMap<UIMessage, readonly (readonly unknown[])[]>();
  let previous: readonly (readonly unknown[])[] = [];
  let status: ConversationStatus = { files: [], terminals: [], agents: [] };
  return (messages: readonly UIMessage[]): ConversationStatus => {
    const signatures: (readonly unknown[])[] = [];
    for (const message of messages) {
      if (message.role !== "assistant") continue;
      let items = cache.get(message);
      if (!items) {
        const next: (readonly unknown[])[] = [];
        for (const part of message.parts) {
          const tool = conversationTool(part);
          if (!tool) continue;
          next.push([
            tool.name,
            tool.id,
            tool.state,
            tool.result,
            tool.errorText,
            tool.cancelled,
            tool.input.path,
            tool.input.file_path,
            tool.input.command,
            tool.input.description,
            tool.input.task,
            tool.input.type,
            tool.input.agent,
            tool.input.handle,
          ]);
        }
        items = next;
        cache.set(message, items);
      }
      signatures.push(...items);
    }
    if (
      signatures.length === previous.length &&
      signatures.every((values, index) =>
        values.every((value, field) => value === previous[index][field]),
      )
    )
      return status;
    previous = signatures;
    status = buildConversationStatus(messages);
    return status;
  };
}

export function workspaceGitStatus(
  root: string | null,
  status: GitStatusSnapshot | null | undefined,
): GitStatusSnapshot | null {
  if (!root || !status) return null;
  const normalize = (path: string) => {
    const canonical = path.replace(/\\/g, "/").replace(/\/+$/, "");
    return /^(?:[A-Za-z]:(?:\/|$)|\/\/)/.test(canonical)
      ? canonical.toLowerCase()
      : canonical;
  };
  const workspace = normalize(root);
  const repo = normalize(status.repoRoot);
  return workspace === repo || workspace.startsWith(`${repo}/`) ? status : null;
}

export function attachmentPreviewUrl(url: string): string | undefined {
  return /^(https?:\/\/|blob:|data:image\/(png|jpe?g|gif|webp);base64,)/i.test(
    url,
  )
    ? url
    : undefined;
}
