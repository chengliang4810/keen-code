import type { RuntimeTodo } from "@/modules/ai/lib/todos";
import type { UIMessageChunk } from "ai";
import type { AgentUsageDelta } from "@/modules/ai/lib/agent";

type Usage = {
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
};
type ModelEvent =
  | { type: "message_start" }
  | {
      type: "text_delta" | "reasoning_delta" | "reasoning_summary_delta";
      index: number;
      delta: string;
    }
  | { type: "message_end" }
  | {
      type:
        | "usage"
        | "decode_timing"
        | "reasoning_continuation"
        | "tool_call_start"
        | "tool_call_arguments_delta"
        | "tool_call_end";
    };

export type NativeAgentEvent =
  | { type: "model"; round: number; event: ModelEvent }
  | { type: "tool_input"; id: string; name: string; input: unknown }
  | {
      type: "tool_result";
      result: {
        toolCallId: string;
        isError: boolean;
        content: { type: string; text?: string }[];
      };
    }
  | {
      type: "approval";
      id: string;
      toolCallId: string;
      name: string;
      input: unknown;
    }
  | { type: "transcript"; id: string; messages: unknown[] }
  | { type: "usage"; usage: Usage }
  | { type: "todos"; sessionId: string; todos: RuntimeTodo[]; revision: number }
  | { type: "compact" }
  | { type: "diagnostic"; message: string }
  | { type: "client_tool"; id: string; name: string; input: unknown }
  | { type: "finish"; cancelled: boolean; hitStepCap?: boolean }
  | { type: "error"; message: string }
  | { type: "end" };

const DISPLAY_NAMES: Record<string, string> = {
  Read: "read_file",
  Write: "write_file",
  Edit: "edit",
  MultiEdit: "multi_edit",
  Glob: "glob",
  Grep: "grep",
  Bash: "bash_run",
  PowerShell: "bash_run",
};

function displayInput(input: unknown): unknown {
  if (!input || typeof input !== "object") return input;
  const value = input as Record<string, unknown>;
  return "file_path" in value ? { ...value, path: value.file_path } : input;
}

export function createNativeEventMapper(callbacks: {
  onTodos?: (sessionId: string, todos: RuntimeTodo[]) => void;
  onStep?: (step: string | null) => void;
  onUsage?: (usage: AgentUsageDelta) => void;
  onCompact?: (info: { droppedCount: number }) => void;
  onFinishMeta?: (info: { hitStepCap: boolean; finishReason: string }) => void;
}) {
  const blocks = new Map<string, "text" | "reasoning">();
  const tools = new Set<string>();
  const pending = new Set<string>();
  const transcripts = new Set<string>();
  let stepOpen = false;
  let finished = false;
  const closeBlocks = (chunks: UIMessageChunk[]) => {
    for (const [id, kind] of blocks) chunks.push({ type: `${kind}-end`, id });
    blocks.clear();
  };
  const closeStep = (chunks: UIMessageChunk[]) => {
    closeBlocks(chunks);
    if (stepOpen) chunks.push({ type: "finish-step" });
    stepOpen = false;
  };
  return (event: NativeAgentEvent): UIMessageChunk[] => {
    const chunks: UIMessageChunk[] = [];
    switch (event.type) {
      case "model": {
        const model = event.event;
        if (model.type === "message_start") {
          closeStep(chunks);
          chunks.push({ type: "start-step" });
          stepOpen = true;
          callbacks.onStep?.(null);
        } else if (
          model.type === "text_delta" ||
          model.type === "reasoning_delta" ||
          model.type === "reasoning_summary_delta"
        ) {
          const kind = model.type === "text_delta" ? "text" : "reasoning";
          const id = `${event.round}:${kind}:${model.index}`;
          if (!blocks.has(id)) {
            chunks.push({ type: `${kind}-start`, id });
            blocks.set(id, kind);
          }
          chunks.push({ type: `${kind}-delta`, id, delta: model.delta });
        } else if (model.type === "message_end") closeBlocks(chunks);
        break;
      }
      case "tool_input":
        if (!tools.has(event.id)) {
          tools.add(event.id);
          pending.add(event.id);
          chunks.push({
            type: "tool-input-available",
            toolCallId: event.id,
            toolName: DISPLAY_NAMES[event.name] ?? event.name,
            input: displayInput(event.input),
          });
          callbacks.onStep?.(event.name);
        }
        break;
      case "approval":
        chunks.push({
          type: "tool-approval-request",
          approvalId: event.id,
          toolCallId: event.toolCallId,
        });
        break;
      case "tool_result": {
        pending.delete(event.result.toolCallId);
        const text = event.result.content
          .map((c) => c.text ?? "")
          .filter(Boolean)
          .join("\n");
        let output: unknown = text;
        try {
          output = JSON.parse(text);
        } catch {
          /* 普通工具文本按原样展示。 */
        }
        chunks.push(
          event.result.isError
            ? {
                type: "tool-output-error",
                toolCallId: event.result.toolCallId,
                errorText: text || "工具执行失败",
              }
            : {
                type: "tool-output-available",
                toolCallId: event.result.toolCallId,
                output,
              },
        );
        break;
      }
      case "transcript":
        if (!transcripts.has(event.id)) {
          transcripts.add(event.id);
          chunks.push({
            type: "data-rcode-messages",
            id: event.id,
            data: { messages: event.messages },
          });
        }
        break;
      case "usage":
        callbacks.onUsage?.({
          inputTokens: event.usage.inputTokens ?? 0,
          outputTokens: event.usage.outputTokens ?? 0,
          cachedInputTokens: event.usage.cacheReadTokens ?? 0,
          lastInputTokens: event.usage.inputTokens ?? 0,
          lastCachedTokens: event.usage.cacheReadTokens ?? 0,
        });
        break;
      case "todos":
        callbacks.onTodos?.(event.sessionId, event.todos);
        break;
      case "compact":
        callbacks.onCompact?.({ droppedCount: 0 });
        break;
      case "diagnostic":
        chunks.push({
          type: "data-rcode-diagnostic",
          data: { message: event.message },
        });
        break;
      case "error":
        closeStep(chunks);
        for (const toolCallId of pending)
          chunks.push({
            type: "tool-output-error",
            toolCallId,
            errorText: event.message,
          });
        pending.clear();
        chunks.push({ type: "error", errorText: event.message });
        break;
      case "finish":
        closeStep(chunks);
        for (const toolCallId of pending)
          chunks.push({
            type: "tool-output-error",
            toolCallId,
            errorText: event.cancelled ? "操作已取消" : "工具未完成",
            // SDK 将结果元数据保存在消息中，让取消状态随历史回放恢复。
            providerMetadata: event.cancelled
              ? { rcode: { cancelled: true } }
              : undefined,
          });
        pending.clear();
        if (!finished) {
          chunks.push(
            event.cancelled
              ? { type: "abort" }
              : { type: "finish", finishReason: "stop" },
          );
          finished = true;
          callbacks.onFinishMeta?.({
            hitStepCap: !!event.hitStepCap,
            finishReason: event.cancelled ? "abort" : "stop",
          });
          callbacks.onStep?.(null);
        }
        break;
      case "end":
        closeStep(chunks);
        if (!finished) {
          chunks.push({ type: "finish", finishReason: "error" });
          finished = true;
        }
        break;
    }
    return chunks;
  };
}
