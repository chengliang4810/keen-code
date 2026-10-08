import { Channel, invoke } from "@tauri-apps/api/core";
import { jsonSchema, tool, type ToolSet, type UIMessageChunk } from "ai";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";
import {
  configuredSubagents,
  resolveSubagentModel,
  subagentSystem,
} from "@/modules/ai/agents/config";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import type { NativeAgentEvent } from "@/modules/ai/lib/nativeStream";
import type { RuntimeTodo } from "@/modules/ai/lib/todos";
import { useSubagentsStore } from "@/modules/ai/store/subagentsStore";
import { useTodosStore } from "@/modules/ai/store/todoStore";
import { currentWorkspaceEnv } from "@/modules/workspace";

export function projectRuntimeTodos(
  sessionId: string,
  todos: RuntimeTodo[],
): void {
  useTodosStore.getState().setTodos(
    sessionId,
    todos.map((todo) => ({
      id: todo.content,
      title: todo.content,
      description: todo.active_form,
      status: todo.status,
    })),
  );
}

export async function businessConfiguration(opts: RunAgentOptions) {
  const sessionId = opts.toolContext.getSessionId();
  if (!sessionId) throw new Error("没有活动对话");
  await Promise.all([
    useSubagentsStore.getState().hydrate(),
    useTodosStore.getState().hydrate(sessionId),
  ]);
  const todos: RuntimeTodo[] = (
    useTodosStore.getState().bySession[sessionId] ?? []
  ).map((todo) => ({
    content: todo.title,
    status: todo.status,
    active_form: todo.description || todo.title,
  }));
  const subagents = configuredSubagents(useSubagentsStore.getState().config)
    .filter((agent) => agent.enabled)
    .map((agent) => {
      const selection = resolveSubagentModel(
        agent,
        opts,
        opts.customEndpoints ?? [],
      );
      const model = resolveNativeModel({ ...opts, ...selection });
      if (!model) throw new Error("子 Agent 模型不支持当前协议");
      const { secretAccount, ...config } = model;
      return {
        template: {
          id: agent.id,
          label: agent.label,
          description: agent.description,
          tools: agent.tools,
          systemPrompt: subagentSystem(
            agent,
            opts.globalInstructions,
            opts.projectInstructions,
          ),
        },
        model: config,
        secretAccount,
      };
    });
  return { todos, subagents };
}

type Definition = {
  name: string;
  description: string;
  inputSchema: Parameters<typeof jsonSchema>[0];
};

// WSL 保留 AI SDK 模型循环，业务执行与审批使用相同的 Rust Runner。
export async function prepareBusinessBridge(opts: RunAgentOptions) {
  const model = resolveNativeModel(opts);
  const sessionId = opts.toolContext.getSessionId();
  if (!model || !sessionId) throw new Error("业务工具需要有效模型和对话");
  const configuration = await businessConfiguration(opts);
  const runId = crypto.randomUUID();
  const onEvent = new Channel<NativeAgentEvent>();
  const pending = new Map<string, UIMessageChunk[]>();
  const inputs = new Set<string>();
  let controller: ReadableStreamDefaultController<UIMessageChunk> | undefined;
  let closed = false;
  let disposal: Promise<void> | undefined;
  const cancel = () => {
    void invoke("agent_core_cancel", { runId }).catch(() => {});
  };
  const dispose = () => {
    if (!disposal) {
      opts.abortSignal?.removeEventListener("abort", cancel);
      disposal = invoke<void>("agent_business_finish", { runId });
    }
    return disposal;
  };
  onEvent.onmessage = (event) => {
    if (closed) return;
    if (event.type === "todos")
      projectRuntimeTodos(event.sessionId, event.todos);
    if (event.type === "approval") {
      const chunk: UIMessageChunk = {
        type: "tool-approval-request",
        approvalId: event.id,
        toolCallId: event.toolCallId,
      };
      if (controller && inputs.has(event.toolCallId)) controller.enqueue(chunk);
      else
        pending.set(event.toolCallId, [
          ...(pending.get(event.toolCallId) ?? []),
          chunk,
        ]);
    }
  };
  const definitions = await invoke<Definition[]>("agent_business_start", {
    request: {
      runId,
      sessionId,
      ...model,
      ...configuration,
      cwd: opts.toolContext.getWorkspaceRoot() ?? opts.toolContext.getCwd(),
      system: "",
      messages: [],
      clientTools: [],
      permissionMode: opts.permissionMode ?? "ask",
      planMode: !!opts.planMode,
    },
    workspace: currentWorkspaceEnv(),
    onEvent,
  });
  opts.abortSignal?.addEventListener("abort", cancel, { once: true });
  if (opts.abortSignal?.aborted) cancel();
  const tools: ToolSet = Object.fromEntries(
    definitions.map((definition) => [
      definition.name,
      tool({
        description: definition.description,
        inputSchema: jsonSchema(definition.inputSchema),
        execute: (input, { toolCallId }) =>
          invoke("agent_business_execute", {
            runId,
            callId: toolCallId,
            name: definition.name,
            input,
          }),
      }),
    ]),
  );
  return {
    tools,
    dispose,
    wrap(
      source: ReadableStream<UIMessageChunk>,
    ): ReadableStream<UIMessageChunk> {
      const reader = source.getReader();
      return new ReadableStream({
        async start(value) {
          controller = value;
          try {
            while (!closed) {
              const next = await reader.read();
              if (closed) break;
              if (next.done) {
                await dispose();
                break;
              }
              if (next.value.type === "finish" || next.value.type === "abort")
                await dispose();
              controller.enqueue(next.value);
              if (next.value.type === "tool-input-available") {
                const id = next.value.toolCallId;
                inputs.add(id);
                for (const chunk of pending.get(id) ?? [])
                  controller.enqueue(chunk);
                pending.delete(id);
              }
            }
            await dispose();
            if (!closed) {
              closed = true;
              controller.close();
            }
          } catch (error) {
            await dispose().catch(() => {});
            if (!closed) {
              closed = true;
              controller.error(error);
            }
          } finally {
            reader.releaseLock();
          }
        },
        async cancel(reason) {
          closed = true;
          cancel();
          try {
            await reader.cancel(reason);
          } finally {
            await dispose();
          }
        },
      });
    },
  };
}
