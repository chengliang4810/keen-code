import {
  businessConfiguration,
  projectRuntimeTodos,
} from "@/modules/ai/lib/businessTools";
import { memoryPrompt } from "@/modules/ai/lib/memory";
import { buildMemoryTools } from "@/modules/ai/tools/memory";
import { Channel, invoke } from "@tauri-apps/api/core";
import type { Tool, UIMessageChunk } from "ai";
import { z } from "zod";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";
import { buildStableSystem } from "@/modules/ai/lib/agentInstructions";
import type { NativeModelConfig } from "@/modules/ai/lib/nativeProtocol";
import {
  createNativeEventMapper,
  type NativeAgentEvent,
} from "@/modules/ai/lib/nativeStream";
import { buildFsTools } from "@/modules/ai/tools/fs";
import { buildTerminalTools } from "@/modules/ai/tools/terminal";
import { buildManagedAgentTools } from "@/modules/ai/tools/agent";
import { boundClientToolResult } from "@/modules/ai/lib/clientToolResult";

export async function runNativeAgentStream(
  opts: RunAgentOptions,
  model: NativeModelConfig,
): Promise<ReadableStream<UIMessageChunk>> {
  const business = await businessConfiguration(opts);
  const sessionId = opts.toolContext.getSessionId();
  if (!sessionId) throw new Error("没有活动对话");
  const fs = buildFsTools(opts.toolContext);
  const tools = {
    ...buildMemoryTools(opts.memory),
    create_directory: fs.create_directory,
    ...buildTerminalTools(opts.toolContext),
    ...buildManagedAgentTools(opts.toolContext),
  } as unknown as Record<string, Tool<unknown, unknown>>;
  const clientTools = Object.entries(tools).map(([name, tool]) => ({
    name,
    description: tool.description ?? name,
    inputSchema: z.toJSONSchema(tool.inputSchema as z.ZodType, {
      io: "input",
      // 核心 Schema 子集不接受 format；执行前仍用原 Zod Schema 校验 URL。
      override: ({ jsonSchema }) => {
        delete jsonSchema.format;
      },
    }),
  }));
  const runId = crypto.randomUUID();
  const mapper = createNativeEventMapper({
    ...opts,
    onTodos: projectRuntimeTodos,
  });
  // ReadableStream 构造时同步调用 start，IPC 事件在控制器赋值之后才会送达。
  let controller!: ReadableStreamDefaultController<UIMessageChunk>;
  let closed = false;
  let started = false;
  let cancelled = false;
  const cancellation = new AbortController();
  const cancel = () => {
    cancelled = true;
    cancellation.abort();
    if (started) void invoke("agent_core_cancel", { runId }).catch(() => {});
  };
  const cleanup = () => opts.abortSignal?.removeEventListener("abort", cancel);
  const close = () => {
    if (closed) return;
    closed = true;
    cancellation.abort();
    cleanup();
    controller.close();
  };
  const stream = new ReadableStream<UIMessageChunk>({
    start(value) {
      controller = value;
    },
    cancel() {
      cancel();
      closed = true;
      cleanup();
    },
  });
  opts.abortSignal?.addEventListener("abort", cancel, { once: true });
  if (opts.abortSignal?.aborted) cancel();
  const onEvent = new Channel<NativeAgentEvent>();
  onEvent.onmessage = (event) => {
    if (closed) return;
    if (event.type === "client_tool") {
      void (async () => {
        let result: unknown;
        try {
          if (cancelled) throw new Error("操作已取消");
          const tool = tools[event.name];
          if (!tool?.execute) throw new Error("未注册该界面工具");
          const input = await (tool.inputSchema as z.ZodType).parseAsync(
            event.input,
          );
          if (cancellation.signal.aborted) throw new Error("操作已取消");
          result = await tool.execute(input, {
            toolCallId: event.id,
            messages: [],
            abortSignal: cancellation.signal,
          });
        } catch (error) {
          result = {
            error: error instanceof Error ? error.message : String(error),
          };
        }
        if (closed) return;
        try {
          await invoke("agent_core_tool_result", {
            requestId: event.id,
            result: boundClientToolResult(result),
          });
        } catch (error) {
          if (closed) return;
          const message =
            error instanceof Error ? error.message : String(error);
          try {
            await invoke("agent_core_tool_result", {
              requestId: event.id,
              result: boundClientToolResult({
                error: message.slice(0, 4096),
              }),
            });
          } catch (failure) {
            if (closed) return;
            cancel();
            controller.enqueue({
              type: "error",
              errorText:
                failure instanceof Error ? failure.message : String(failure),
            });
            close();
          }
        }
      })();
    } else {
      for (const chunk of mapper(event)) controller.enqueue(chunk);
      if (event.type === "end") close();
    }
  };
  controller.enqueue({ type: "start", messageId: `rcode-${runId}` });
  try {
    await invoke("agent_core_start", {
      request: {
        runId,
        sessionId,
        ...model,
        cwd: opts.toolContext.getWorkspaceRoot() ?? opts.toolContext.getCwd(),
        system: buildStableSystem(
          opts.agentPersona ?? null,
          opts.globalInstructions,
          opts.projectInstructions ?? null,
          memoryPrompt(opts.memory) +
            "\n\nUse native Read, Write, Edit, MultiEdit, Glob and Grep for files. Their file_path/path parameters stay inside the task workspace. Use PowerShell on Windows or Bash on Unix for foreground commands. Each command runs in a supervised process; pass cwd explicitly instead of relying on a previous cd. Use bash_background for long-running processes. Other registered tools retain their documented names.",
        ),
        messages: opts.uiMessages,
        planMode: !!opts.planMode,
        permissionMode: opts.permissionMode ?? "ask",
        clientTools,
        ...business,
      },
      onEvent,
    });
    started = true;
    // abort 可能先于 IPC 注册完成；收到 ACK 后再次取消，避免留下后台运行。
    if (cancelled) await invoke("agent_core_cancel", { runId });
  } catch (error) {
    if (!closed) {
      controller.enqueue({
        type: "error",
        errorText: error instanceof Error ? error.message : String(error),
      });
      close();
    }
  }
  return stream;
}
