import type { AcpSubagentInfo } from "./acp/store";
import type { SessionSubagentHistory } from "./acp/api";
import {
  compactMessageSegments,
  deriveFieldsFromSegments,
  type ChatMessage,
  type MessageSegment,
  type MessageToolSegment,
} from "./session";

/** peri ThreadStore 当前持久化消息的唯一结构。 */
interface PeriStoredMessage {
  /** 消息稳定标识。 */
  id: string;
  /** 当前协议声明的消息角色。 */
  role: "user" | "assistant" | "system" | "tool";
  /** 纯文本或 ACP 内容块。 */
  content: string | unknown[];
  /** OpenAI 线格式的外层工具调用（chatcmpl 供应商不落 tool_use 块）。 */
  toolCalls: StoredToolCall[];
  /** 工具结果关联的调用标识。 */
  toolCallId?: string;
  /** 工具结果是否表示失败。 */
  isError: boolean;
}

/** OpenAI 线格式的外层工具调用项。 */
interface StoredToolCall {
  id: string;
  name: string;
  arguments?: unknown;
}

/** 严格解析外层 tool_calls 数组；非法项跳过。 */
function parseStoredToolCalls(value: unknown): StoredToolCall[] {
  if (!Array.isArray(value)) return [];
  const calls: StoredToolCall[] = [];
  for (const item of value) {
    if (!isRecord(item)) continue;
    if (!item.id || typeof item.id !== "string") continue;
    if (!item.name || typeof item.name !== "string") continue;
    calls.push({
      id: item.id,
      name: item.name,
      ...(item.arguments !== undefined ? { arguments: item.arguments } : {}),
    });
  }
  return calls;
}

/** 判断未知值是否为可安全读取字段的普通对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

/** 严格解析 peri ThreadStore 返回的当前消息结构。 */
function parseStoredMessage(value: unknown): PeriStoredMessage | null {
  if (!isRecord(value)) return null;
  const role = String(value.role);
  if (
    typeof value.id !== "string" ||
    !["user", "assistant", "system", "tool"].includes(role) ||
    (typeof value.content !== "string" && !Array.isArray(value.content))
  ) {
    return null;
  }
  if (role === "tool" && typeof value.tool_call_id !== "string") {
    return null;
  }
  return {
    id: value.id,
    role: role as PeriStoredMessage["role"],
    content: value.content,
    toolCalls: parseStoredToolCalls(value.tool_calls),
    ...(typeof value.tool_call_id === "string"
      ? { toolCallId: value.tool_call_id }
      : {}),
    isError: value.is_error === true,
  };
}

/** 将未知值转换为可展示的紧凑 JSON 文本。 */
function stringifyValue(value: unknown): string | undefined {
  if (value === undefined || value === null) return undefined;
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** 从当前消息内容中按原始顺序投影文本、思考和工具调用。 */
function contentSegments(content: string | unknown[]): MessageSegment[] {
  if (typeof content === "string") {
    return content ? [{ kind: "content", text: content }] : [];
  }
  const segments: MessageSegment[] = [];
  for (const value of content) {
    if (!isRecord(value)) continue;
    if (value.type === "text" && typeof value.text === "string") {
      segments.push({ kind: "content", text: value.text });
      continue;
    }
    if (value.type === "reasoning" && typeof value.text === "string") {
      segments.push({ kind: "thought", text: value.text });
      continue;
    }
    if (
      value.type === "tool_use" &&
      typeof value.id === "string" &&
      typeof value.name === "string"
    ) {
      segments.push({
        kind: "tool",
        toolCallId: value.id,
        title: value.name,
        toolKind: value.name,
        status: "pending",
        streaming: false,
        input: stringifyValue(value.input),
      });
      continue;
    }
    if (Array.isArray(value.content)) {
      segments.push(...contentSegments(value.content));
    }
  }
  return compactMessageSegments(segments);
}

/** 从工具结果内容中读取可展示文本。 */
function toolResultText(content: string | unknown[]): string {
  return deriveFieldsFromSegments(contentSegments(content)).content;
}

/** 将 peri 持久化消息按真实块顺序投影，并把工具结果回填到对应调用。 */
export function projectPeriStoredMessages(values: unknown[]): ChatMessage[] {
  const messages: ChatMessage[] = [];
  const tools = new Map<string, MessageToolSegment>();
  // 同一用户回合内，工具调用之间的每段正文都是独立的 assistant 存储行；
  // live 路径一轮只产出一条消息，这里 likewise 按回合累积分段、回合边界
  // （下一条 user 行）统一投影，避免一轮冒出多个气泡和多组操作按钮。
  let pending: { id: string; segments: MessageSegment[] } | null = null;

  const registerToolSegments = (segments: MessageSegment[]) => {
    for (const segment of segments) {
      if (segment.kind === "tool") tools.set(segment.toolCallId, segment);
    }
  };

  const flushPending = () => {
    if (!pending) return;
    const { id, segments: accumulated } = pending;
    pending = null;
    const segments = compactMessageSegments(accumulated);
    if (segments.length === 0) return;
    const fields = deriveFieldsFromSegments(segments);
    messages.push({
      id,
      role: "assistant",
      content: fields.content,
      thought: fields.thought,
      thoughtPhases: fields.thoughtPhases,
      segments,
      streaming: false,
    });
  };

  for (const value of values) {
    const message = parseStoredMessage(value);
    if (!message || message.role === "system") continue;

    if (message.role === "tool") {
      const toolCallId = message.toolCallId!;
      const output = toolResultText(message.content);
      const tool = tools.get(toolCallId);
      if (tool) {
        tool.status = message.isError ? "failed" : "completed";
        tool.isError = message.isError || undefined;
        tool.output = output || undefined;
        tool.detail = output || tool.detail;
        continue;
      }
      // 无对应调用的结果行：并入当前回合时间线保持真实顺序；
      // 回合尚未开始（工具先于首条 assistant）时保留独立 tool_step 行。
      if (pending) {
        const orphan: MessageToolSegment = {
          kind: "tool",
          toolCallId,
          title: toolCallId,
          status: message.isError ? "failed" : "completed",
          streaming: false,
          isError: message.isError || undefined,
          output: output || undefined,
          detail: output || undefined,
        };
        pending.segments.push(orphan);
        tools.set(toolCallId, orphan);
        continue;
      }
      messages.push({
        id: message.id,
        role: "tool",
        content: output || toolCallId,
        marker: "tool_step",
        toolCallId,
        toolStatus: message.isError ? "failed" : "completed",
        toolDetail: output || undefined,
        isError: message.isError || undefined,
        streaming: false,
      });
      continue;
    }

    if (message.role === "user") {
      flushPending();
      messages.push({
        id: message.id,
        role: "user",
        content:
          typeof message.content === "string" ? message.content : "",
        streaming: false,
      });
      continue;
    }

    const segments = contentSegments(message.content);
    if (message.toolCalls.length) {
      // chatcmpl 供应商把工具调用存为外层 tool_calls 而非 tool_use 块；
      // 投影为工具段后，工具结果行按 toolCallId 回填状态与输出。
      const seen = new Set(
        segments
          .filter(
            (segment): segment is MessageToolSegment => segment.kind === "tool",
          )
          .map((segment) => segment.toolCallId),
      );
      for (const call of message.toolCalls) {
        if (seen.has(call.id)) continue;
        segments.push({
          kind: "tool",
          toolCallId: call.id,
          title: call.name,
          toolKind: call.name,
          status: "pending",
          streaming: false,
          input: stringifyValue(call.arguments),
        });
      }
    }
    if (!pending) pending = { id: message.id, segments: [] };
    pending.segments.push(...segments);
    registerToolSegments(segments);
  }
  flushPending();

  return messages;
}

/** 从已恢复的 Agent 工具段还原历史子 Agent，供时间线和详情侧栏共用。 */
export function projectPeriStoredSubagents(
  messages: ChatMessage[],
): AcpSubagentInfo[] {
  const agents: AcpSubagentInfo[] = [];
  for (const message of messages) {
    for (const segment of message.segments ?? []) {
      if (
        segment.kind !== "tool" ||
        segment.toolKind?.toLowerCase() !== "agent"
      ) {
        continue;
      }
      const output = segment.output ?? segment.detail ?? "";
      const id = output.match(/(?:^|\n)child_thread_id:\s*([^\s]+)/)?.[1];
      if (!id) continue;
      let input: Record<string, unknown> = {};
      try {
        const parsed = JSON.parse(segment.input ?? "{}");
        if (isRecord(parsed)) input = parsed;
      } catch {
        // 输入仅用于补充 Agent 类型；损坏时仍可恢复 child_thread_id。
      }
      const prompt = typeof input.prompt === "string" ? input.prompt : "";
      const inferred = prompt.match(
        /[（(]([\w-]+)[）)]\s*(?:智能体|agent)/i,
      )?.[1];
      const subagentType = [input.subagent_type, inferred].find(
        (value): value is string =>
          typeof value === "string" && value.trim().length > 0,
      );
      const result = output
        .replace(/^(?:\s*)child_thread_id:\s*[^\s]+\s*/, "")
        .trim();
      agents.push({
        agent_id: id,
        agent_name: subagentType?.trim() || "Agent",
        nickname: null,
        ...(typeof input.prompt === "string" && input.prompt.trim()
          ? { prompt: input.prompt.trim() }
          : {}),
        status:
          segment.status === "failed"
            ? "failed"
            : segment.status === "completed"
              ? "done"
              : "running",
        is_background: false,
        started_at: 0,
        stopped_at: segment.status === "pending" ? null : 0,
        result: result || null,
        segments: result ? [{ kind: "content", text: result }] : [],
      });
    }
  }
  return agents;
}

/** 从持久化子 Thread 恢复子 Agent 的完整正文、思考和工具时间线。 */
export function projectPeriStoredSubagentThreads(
  histories: SessionSubagentHistory[],
): AcpSubagentInfo[] {
  return histories.map((history) => {
    const messages = projectPeriStoredMessages(history.messages);
    const segments = messages.flatMap((message) => message.segments ?? []);
    const result = [...messages]
      .reverse()
      .find((message) => message.role === "assistant" && message.content.trim())
      ?.content.trim();
    return {
      agent_id: history.id,
      agent_name: history.name,
      nickname: history.nickname,
      status:
        history.status === "active"
          ? "running"
          : history.status === "done"
            ? "done"
            : "failed",
      is_background: false,
      started_at: Date.parse(history.createdAt) || 0,
      stopped_at:
        history.status === "active" ? null : Date.parse(history.updatedAt) || 0,
      result: result || null,
      segments,
    };
  });
}

/** 用主对话中的 Agent 工具输入补齐实时子 Agent 的委派任务。 */
export function withSubagentPrompts(
  messages: ChatMessage[],
  subagents: AcpSubagentInfo[],
): AcpSubagentInfo[] {
  if (subagents.length === 0) return subagents;
  const prompts = new Map<string, string>();
  for (const message of messages) {
    for (const segment of message.segments ?? []) {
      if (
        segment.kind !== "tool" ||
        segment.toolKind?.toLowerCase() !== "agent"
      ) {
        continue;
      }
      try {
        const input = JSON.parse(segment.input ?? "{}");
        if (!isRecord(input) || typeof input.prompt !== "string") continue;
        const prompt = input.prompt.trim();
        if (!prompt) continue;
        const evidence = `${segment.output ?? ""}\n${segment.detail ?? ""}`;
        const byId = subagents.find((agent) =>
          evidence.includes(agent.agent_id),
        );
        const requestedType =
          typeof input.subagent_type === "string" && input.subagent_type.trim()
            ? input.subagent_type.trim()
            : "";
        const candidates = requestedType
          ? subagents.filter((agent) => agent.agent_name === requestedType)
          : subagents;
        const agent =
          byId ?? (candidates.length === 1 ? candidates[0] : undefined);
        if (agent) prompts.set(agent.agent_id, prompt);
      } catch {
        // 非 JSON 工具输入无法提供委派任务。
      }
    }
  }
  if (prompts.size === 0) return subagents;
  return subagents.map((agent) => ({
    ...agent,
    prompt: prompts.get(agent.agent_id) ?? agent.prompt,
  }));
}
