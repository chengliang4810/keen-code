import { describe, expect, it } from "vitest";
import { AcpRpcError } from "./acp/client";
import {
  applyTurnError,
  buildSegmentsFromFields,
  canSend,
  canStop,
  canType,
  clearPriorTurnErrors,
  clearPriorTurnStreaming,
  classifyAgentErrorCode,
  compactMessageSegments,
  errorCopy,
  formatTurnErrorBody,
  localizeSystemNotification,
  localizeUiError,
  isFailedToolStepMessage,
  messageSegments,
  splitThoughtPhases,
  isSessionBusy,
  isSessionLiveStreaming,
  parseCompactContent,
  parseToolStepContent,
  toolStepDisplayTitle,
  presentErrorBanner,
  snapshotOutgoingMessages,
  stripAnsi,
  type ChatMessage,
  type MessageFileChange,
} from "./session";

describe("session projection", () => {
  it("input matrix Ready / Streaming / Stop (draft ok while stream; send blocked)", () => {
    expect(canType("ready")).toBe(true);
    expect(canType("idle")).toBe(true);
    // Draft allowed while streaming so the box is never "stuck" on pauses.
    expect(canType("streaming")).toBe(true);
    expect(canSend("ready")).toBe(true);
    expect(canSend("idle")).toBe(true);
    expect(canStop("ready")).toBe(false);
    expect(canStop("streaming")).toBe(true);
    expect(canSend("streaming")).toBe(false);
  });

  it("isSessionBusy covers connect / stream", () => {
    expect(isSessionBusy("idle")).toBe(false);
    expect(isSessionBusy("ready")).toBe(false);
    expect(isSessionBusy("disconnected")).toBe(false);
    expect(isSessionBusy("connecting")).toBe(true);
    expect(isSessionBusy("streaming")).toBe(true);
  });

  it("isSessionLiveStreaming excludes connecting (sidebar spinner silent)", () => {
    expect(isSessionLiveStreaming("connecting")).toBe(false);
    expect(isSessionLiveStreaming("idle")).toBe(false);
    expect(isSessionLiveStreaming("ready")).toBe(false);
    expect(isSessionLiveStreaming("streaming")).toBe(true);
  });

  it("splitThoughtPhases separates multi-phase markers", () => {
    expect(splitThoughtPhases("a\n\n⟪phase⟫\n\nb")).toEqual(["a", "b"]);
    expect(splitThoughtPhases("only")).toEqual(["only"]);
  });

  it("isFailedToolStepMessage detects failed tools only", () => {
    expect(
      isFailedToolStepMessage({
        id: "tool-a",
        role: "tool",
        content: "Read x",
        marker: "tool_step",
        toolStatus: "completed",
      }),
    ).toBe(false);
    expect(
      isFailedToolStepMessage({
        id: "tool-b",
        role: "tool",
        content: "Bash",
        marker: "tool_step",
        toolStatus: "failed",
        isError: true,
      }),
    ).toBe(true);
  });

  it("buildSegmentsFromFields stacks multi-phase thought before body", () => {
    const segs = buildSegmentsFromFields(
      "answer body",
      "a\n\n⟪phase⟫\n\nb\n\n⟪phase⟫\n\nc",
      undefined,
    );
    // One thought block + body — never "body then 思考 2 / 3".
    expect(segs).toEqual([
      { kind: "thought", text: "a\n\nb\n\nc" },
      { kind: "content", text: "answer body" },
    ]);
  });

  it("compactMessageSegments merges adjacent thoughts", () => {
    expect(
      compactMessageSegments([
        { kind: "thought", text: "a" },
        { kind: "thought", text: "b" },
        { kind: "content", text: "hi" },
        { kind: "thought", text: "c" },
        { kind: "thought", text: "" },
      ]),
    ).toEqual([
      { kind: "thought", text: "a\n\nb" },
      { kind: "content", text: "hi" },
      { kind: "thought", text: "c" },
    ]);
  });

  it("compactMessageSegments keeps tools and coalesces same toolCallId", () => {
    const segs = compactMessageSegments([
      { kind: "thought", text: "t" },
      {
        kind: "tool",
        toolCallId: "x",
        title: "Read a",
        status: "running",
        streaming: true,
      },
      {
        kind: "tool",
        toolCallId: "x",
        title: "Read a",
        status: "completed",
        streaming: false,
      },
      { kind: "content", text: "done" },
    ]);
    expect(segs.map((s) => s.kind)).toEqual(["thought", "tool", "content"]);
    expect(segs[1]).toMatchObject({
      kind: "tool",
      toolCallId: "x",
      status: "completed",
      streaming: false,
    });
  });

  it("compactMessageSegments preserves fileChanges across duplicate tool updates", () => {
    const first = compactMessageSegments([
      {
        kind: "tool",
        toolCallId: "write-1",
        title: "Write",
        status: "completed",
        fileChanges: [
          { path: "empty.txt", oldText: null, newText: "" },
          {
            path: "bom-crlf.txt",
            oldText: "\uFEFFbefore\r\n",
            newText: "\uFEFFafter\r\n",
          },
        ],
      },
    ]);

    // 相同 toolId 的后续更新省略快照时，既不能丢弃 null/空文本，也不能改写 BOM/CRLF。
    const omitted = compactMessageSegments([
      first[0]!,
      {
        kind: "tool",
        toolCallId: "write-1",
        title: "Write",
        status: "completed",
      },
    ]);
    expect(omitted[0]).toMatchObject({
      fileChanges: [
        { path: "empty.txt", oldText: null, newText: "" },
        {
          path: "bom-crlf.txt",
          oldText: "\uFEFFbefore\r\n",
          newText: "\uFEFFafter\r\n",
        },
      ],
    });

    // 同一 toolId 携带新快照时替换旧快照，而不是累加历史调用结果。
    const replaced = compactMessageSegments([
      omitted[0]!,
      {
        kind: "tool",
        toolCallId: "write-1",
        title: "Write",
        status: "completed",
        fileChanges: [
          {
            path: "replacement.txt",
            oldText: "old\r\n",
            newText: "new\uFEFF\r\n",
          },
        ],
      },
    ]);
    expect(replaced[0]).toMatchObject({
      fileChanges: [
        {
          path: "replacement.txt",
          oldText: "old\r\n",
          newText: "new\uFEFF\r\n",
        },
      ],
    });

    // 显式空数组表示本次结果确认没有文件变更，必须清除已有快照。
    const cleared = compactMessageSegments([
      replaced[0]!,
      {
        kind: "tool",
        toolCallId: "write-1",
        title: "Write",
        status: "completed",
        fileChanges: [],
      },
    ]);
    expect(cleared[0]).toHaveProperty("fileChanges", []);
  });

  it("compactMessageSegments preserves reference file changes without materializing正文", () => {
    const referenceChange: MessageFileChange = {
      path: "large.txt",
      reference: {
        sessionId: "session-1",
        requestId: "write-1",
        path: "large.txt",
        before: null,
        after: {
          sizeBytes: 0,
          sha256:
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        },
        applied: true,
      },
    };
    const initial = compactMessageSegments([{
      kind: "tool",
      toolCallId: "write-1",
      title: "Write",
      status: "completed",
      fileChanges: [referenceChange],
    }]);
    const omitted = compactMessageSegments([
      initial[0]!,
      {
        kind: "tool",
        toolCallId: "write-1",
        title: "Write",
        status: "completed",
      },
    ]);
    expect(omitted[0]).toMatchObject({ fileChanges: [referenceChange] });
    expect(omitted[0]).not.toHaveProperty("fileChanges.0.oldText");
    expect(omitted[0]).not.toHaveProperty("fileChanges.0.newText");
  });

  it("messageSegments compacts live multi thought rows", () => {
    const segs = messageSegments({
      id: "a1",
      role: "assistant",
      content: "done",
      segments: [
        { kind: "thought", text: "p1" },
        { kind: "thought", text: "p2" },
        { kind: "content", text: "done" },
        { kind: "thought", text: "p3" },
      ],
    });
    expect(segs).toEqual([
      { kind: "thought", text: "p1\n\np2" },
      { kind: "content", text: "done" },
      { kind: "thought", text: "p3" },
    ]);
  });

  it("snapshotOutgoingMessages never clobbers a populated cache with an empty view", () => {
    const cached: ChatMessage[] = [
      { id: "u1", role: "user", content: "q" },
      { id: "a1", role: "assistant", content: "a" },
    ];
    // Workbench already cleared (user hit "new chat") — keep the real turn.
    expect(snapshotOutgoingMessages(cached, [])).toEqual(cached);
    // Normal case: the viewed thread is authoritative.
    const viewed: ChatMessage[] = [{ id: "u2", role: "user", content: "q2" }];
    expect(snapshotOutgoingMessages(cached, viewed)).toEqual(viewed);
    // Nothing anywhere → empty.
    expect(snapshotOutgoingMessages(undefined, [])).toEqual([]);
  });

  it("clearPriorTurnStreaming only clears assistants before last user", () => {
    const msgs: ChatMessage[] = [
      { id: "a0", role: "assistant", content: "x", streaming: true },
      { id: "u1", role: "user", content: "hi" },
      { id: "a1", role: "assistant", content: "", streaming: true },
    ];
    const next = clearPriorTurnStreaming(msgs);
    expect(next[0]!.streaming).toBe(false);
    expect(next[2]!.streaming).toBe(true);
  });

  it("新回合开始时移除上一轮瞬时错误回复", () => {
    const messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "hello" },
      {
        id: "session-1:turn-error",
        role: "assistant",
        content: "网络或模型服务异常",
        isError: true,
        errorBodyFormatted: true,
      },
      {
        id: "tool-failed",
        role: "tool",
        content: "permission denied",
        isError: true,
      },
    ];

    expect(clearPriorTurnErrors(messages)).toEqual([
      { id: "u1", role: "user", content: "hello" },
      {
        id: "tool-failed",
        role: "tool",
        content: "permission denied",
        isError: true,
      },
    ]);
  });

  it("next-send optimistic path does not leave prior turn streaming (no re-type history)", () => {
    // Simulate the ACP projection after turn 1 has finished, then user sends
    // turn 2. The legacy stream reducer is intentionally not part of this
    // path anymore.
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "first" },
      {
        id: "a1",
        role: "assistant",
        content: "answer one",
        streaming: true,
      },
    ];
    messages = messages.map((message) =>
      message.id === "a1" ? { ...message, streaming: false } : message,
    );
    expect(messages[1]!.streaming).toBe(false);
    expect(messages[1]!.content).toBe("answer one");

    // Same path as executeSend appendOptimistic: clear prior streaming flags
    // then append new user + pending assistant — prior content stays put once.
    const cleaned = clearPriorTurnStreaming(messages);
    const nextSend: ChatMessage[] = [
      ...cleaned,
      { id: "u2", role: "user", content: "second" },
      { id: "a-pending-2", role: "assistant", content: "", streaming: true },
    ];
    expect(nextSend.filter((m) => m.role === "assistant" && m.streaming)).toHaveLength(
      1,
    );
    expect(nextSend[1]!.content).toBe("answer one");
    expect(nextSend[1]!.streaming).toBe(false);
  });

  it("errorCopy distinguishes seven codes (English default)", () => {
    expect(errorCopy("RUNTIME_UNAVAILABLE")).toMatch(/runtime/i);
    expect(errorCopy("AUTH_FAILED")).toMatch(/Auth|sign.?in|credential/i);
    expect(errorCopy("NETWORK_PROVIDER")).toMatch(/Network|model|provider/i);
    expect(errorCopy("AGENT_CRASHED")).toMatch(/crash|process|agent/i);
    expect(errorCopy("QUOTA_EXCEEDED")).toMatch(/Quota|limit|usage/i);
    expect(errorCopy("CONNECT_FAILED")).toMatch(/connect/i);
    expect(errorCopy("PROCESS_LIMIT")).toMatch(/limit|process|concurrent/i);
  });

  it("classifyAgentErrorCode 将供应商 5xx 与上游错误归类为 NETWORK_PROVIDER", () => {
    const providerErrors = [
      "OpenAI API error 502 Bad Gateway",
      "HTTP 503 Service Unavailable",
      "HTTP 504 Gateway Timeout",
      '{"type":"upstream_error","message":"Upstream request failed"}',
      "model service unavailable",
      "provider_error: request failed",
    ];

    for (const message of providerErrors) {
      expect(classifyAgentErrorCode("internal", message)).toBe(
        "NETWORK_PROVIDER",
      );
    }
  });

  it("classifyAgentErrorCode 保持认证、配额与运行时错误的优先级", () => {
    expect(
      classifyAgentErrorCode(
        "internal",
        "OpenAI API error 401 Unauthorized from provider service",
      ),
    ).toBe("AUTH_FAILED");
    expect(
      classifyAgentErrorCode(
        "internal",
        "OpenAI provider returned 429 rate_limit",
      ),
    ).toBe("QUOTA_EXCEEDED");
    expect(
      classifyAgentErrorCode(
        "internal",
        "agent runtime unavailable while connecting to provider",
      ),
    ).toBe("RUNTIME_UNAVAILABLE");
  });

  it("classifyAgentErrorCode 只把未知 daemon 或进程退出归类为 AGENT_CRASHED", () => {
    expect(
      classifyAgentErrorCode("internal", "agent daemon 已退出：exit status 1"),
    ).toBe("AGENT_CRASHED");
    expect(
      classifyAgentErrorCode("internal", "unexpected internal failure"),
    ).toBe("AGENT_CRASHED");
    expect(
      classifyAgentErrorCode("internal", "unexpected internal failure", null),
    ).toBeNull();
  });

  it("formatTurnErrorBody maps connect / quota phrases", () => {
    expect(
      formatTurnErrorBody(
        {
          content:
            "Could not connect the agent for this session; edit aborted.",
        },
        "en",
      ),
    ).toMatch(/connect/i);
    expect(
      formatTurnErrorBody({ content: "rate limit exceeded (429)" }, "en"),
    ).toMatch(/quota|rate/i);
  });

  it("formatTurnErrorBody localizes interrupted model streams", () => {
    const payload = {
      message: "model stream interrupted from openai-compatible",
    };

    expect(formatTurnErrorBody(payload, "en")).toMatch(
      /response stream ended unexpectedly/i,
    );
    expect(formatTurnErrorBody(payload, "zh")).toContain("模型响应");
    expect(formatTurnErrorBody(payload, "zh-TW")).toContain("模型回應");
  });

  it("localizeUiError never exposes unknown technical text", () => {
    const secretDetail = "hyper_util connection failed at internal endpoint";
    expect(localizeUiError(secretDetail, "en")).toBe(
      "The operation failed. Retry the operation.",
    );
    expect(localizeUiError(secretDetail, "zh")).toBe("操作失败，请重试。");
    expect(localizeUiError(secretDetail, "zh-TW")).toBe("操作失敗，請重試。");
  });

  it("localizeUiError 只按 AcpRpcError reason 提供三语言安全文案，banner 保持具体文案", () => {
    const cases = [
      {
        reason: "provider_configuration_changed" as const,
        copy: {
          en: "This session’s model connection configuration changed. Select a model again at the bottom of the conversation and retry.",
          zh: "此会话的模型连接配置已改变。请在对话底部重新选择模型后重试。",
          "zh-TW": "此工作階段的模型連線設定已變更。請在對話底部重新選擇模型後重試。",
        },
      },
      {
        reason: "provider_not_configured" as const,
        copy: {
          en: "This session’s model is unavailable. Check the provider and model in Settings, then select a model again.",
          zh: "此会话的模型不可用。请在设置中检查供应商和模型，再重新选择模型。",
          "zh-TW": "此工作階段的模型無法使用。請在設定中檢查供應商和模型，再重新選擇模型。",
        },
      },
      {
        reason: "provider_reload_failed" as const,
        copy: {
          en: "The model configuration could not be loaded. Check the provider settings and retry.",
          zh: "无法加载模型配置。请在设置中检查供应商配置后重试。",
          "zh-TW": "無法載入模型設定。請在設定中檢查供應商設定後重試。",
        },
      },
    ];

    for (const testCase of cases) {
      const error = new AcpRpcError(-32603, testCase.reason);
      for (const locale of ["en", "zh", "zh-TW"] as const) {
        const localized = localizeUiError(error, locale);
        expect(localized, `${testCase.reason}:${locale}`).toBe(testCase.copy[locale]);

        const banner = presentErrorBanner(null, localized, locale);
        expect(banner, `${testCase.reason}:${locale}`).toMatchObject({
          code: null,
          summary: localized,
          cause: null,
          detail: null,
        });
        expect(banner?.summary, `${testCase.reason}:${locale}`).not.toContain(
          "sensitive provider detail",
        );
      }
    }

    expect(
      localizeUiError(new Error("provider_not_configured: sensitive provider detail"), "zh"),
    ).toBe("操作失败，请重试。");
    expect(localizeUiError(new AcpRpcError(-32603), "zh")).toBe("操作失败，请重试。");
  });

  it("ACP 配置诊断不改变其他错误的既有安全分类", () => {
    for (const locale of ["en", "zh", "zh-TW"] as const) {
      const classified = localizeUiError(new Error("HTTP 401 Unauthorized: private-token"), locale);
      expect(classified).toBe(errorCopy("AUTH_FAILED", locale));
      expect(classified).not.toContain("private-token");
      expect(localizeUiError("429 rate limit: private-response", locale))
        .toBe(errorCopy("QUOTA_EXCEEDED", locale));
    }
  });

  it("localizeSystemNotification renders MCP status in all interface languages", () => {
    const raw = "MCP: context7 failed: transport closed";
    expect(localizeSystemNotification(raw, "en")).toContain("context7 failed");
    expect(localizeSystemNotification(raw, "zh")).toContain("context7 运行失败");
    expect(localizeSystemNotification(raw, "zh-TW")).toContain("context7 執行失敗");
    expect(localizeSystemNotification(raw, "zh")).not.toContain("transport closed");
  });

  it("formatTurnErrorBody 不把供应商英文原文作为本地化错误主体", () => {
    const providerMessage =
      'Model "grok-4.6" is not supported by any configured account in this group';
    expect(
      formatTurnErrorBody(
        { message: `LLM HTTP error (404): ${providerMessage}` },
        "zh",
      ),
    ).toContain("网络或模型服务异常");

    const messages = applyTurnError(
      [],
      {
        messageId: "provider-error",
        code: "model_http_error",
        message: `LLM HTTP error (404): ${providerMessage}`,
      },
      "zh",
    );
    expect(messages[0]).toMatchObject({
      content: "模型服务拒绝了请求。请检查供应商或模型设置后重试。",
      isError: true,
      errorBodyFormatted: true,
    });
  });

  it("presentErrorBanner 保持结构化错误码权威", () => {
    const banner = presentErrorBanner(
      {
        code: "AGENT_CRASHED",
        message:
          'OpenAI API error 502 Bad Gateway: {"error":{"type":"upstream_error"}}',
      },
      null,
      "en",
    );

    expect(banner?.code).toBe("AGENT_CRASHED");
    expect(banner?.summary).toMatch(/agent|process/i);

    const localBanner = presentErrorBanner(
      null,
      "AGENT_CRASHED: HTTP 503 Service Unavailable from model service",
      "en",
    );
    expect(localBanner?.code).toBe("AGENT_CRASHED");
    expect(localBanner?.summary).toMatch(/agent|process/i);
  });

  it("formatTurnErrorBody 隐藏供应商原始 502 响应", () => {
    const body = formatTurnErrorBody(
      {
        code: "internal",
        message:
          'OpenAI API error 502 Bad Gateway: {"error":{"type":"upstream_error"}}',
      },
      "en",
    );

    expect(body).toMatch(/network|model|provider/i);
    expect(body).not.toMatch(/502|Bad Gateway|upstream_error/i);
  });

  it("presentErrorBanner shows friendly deck without MCP dumps", () => {
    const raw =
      'rpc timeout on session/prompt (id=4) after 600s; stderr: ...\nERROR worker quit with fatal: Connection refused';
    const fromAgent = presentErrorBanner(
      { code: "NETWORK_PROVIDER", message: raw },
      null,
      "en",
    );
    expect(fromAgent?.summary).toMatch(/timed?\s*out|timeout|network|model|provider/i);
    expect(fromAgent?.cause).toBeTruthy();
    expect(fromAgent?.summary).not.toMatch(/Connection refused/);
    expect(fromAgent?.summary).not.toMatch(/stderr/i);
    expect(fromAgent?.detail).toBeNull();
    expect(fromAgent?.primary?.id).toBeTruthy();
    expect(fromAgent?.reconnectHint).toBe(true);

    const fromLocal = presentErrorBanner(
      null,
      `NETWORK_PROVIDER: ${raw}`,
      "en",
    );
    expect(fromLocal?.code).toBe("NETWORK_PROVIDER");
    expect(fromLocal?.summary).toMatch(/timed?\s*out|timeout|network|model|provider/i);
    expect(fromLocal?.detail).toBeNull();
    expect(fromLocal?.primary?.label.length).toBeGreaterThan(0);

    const short = presentErrorBanner(null, "Select a project first", "en");
    expect(short?.summary).toBe("Select a project first");
    expect(short?.detail).toBeNull();
    expect(short?.primary?.id).toBe("dismiss");
  });

  it("presentErrorBanner decks the four product classes", () => {
    const runtime = presentErrorBanner(
      { code: "RUNTIME_UNAVAILABLE", message: "unavailable" },
      null,
      "en",
    );
    expect(runtime?.primary?.id).toBe("reconnect");
    expect(runtime?.secondary?.id).toBe("dismiss");

    const auth = presentErrorBanner(
      { code: "AUTH_FAILED", message: "401" },
      null,
      "en",
    );
    expect(auth?.primary?.id).toBe("open_account");

    const crash = presentErrorBanner(
      { code: "AGENT_CRASHED", message: "exit 1" },
      null,
      "en",
    );
    expect(crash?.primary?.id).toBe("reconnect");
  });

  it("formatTurnErrorBody maps turn_timeout tag", () => {
    const body = formatTurnErrorBody(
      {
        code: "NETWORK_PROVIDER",
        message: "turn_timeout",
        content: "**NETWORK_PROVIDER**\n\nturn_timeout",
      },
      "en",
    );
    expect(body).toMatch(/timed?\s*out|timeout/i);
    expect(body).not.toMatch(/NETWORK_PROVIDER|rpc timeout|stderr/i);
  });

  it("stripAnsi removes SGR sequences", () => {
    expect(stripAnsi("\u001b[31mERROR\u001b[0m boom")).toBe("ERROR boom");
  });

  it("applyTurnError replaces optimistic thinking with friendly error", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "hi" },
      { id: "a-pending", role: "assistant", content: "", streaming: true },
    ];
    messages = applyTurnError(
      messages,
      {
        messageId: "host-mid",
        code: "NETWORK_PROVIDER",
        message:
          'rpc timeout on session/prompt (id=6) after 600s; stderr: Connection refused',
        content:
          '**NETWORK_PROVIDER**\n\nrpc timeout on session/prompt (id=6) after 600s; stderr: Connection refused',
      },
      "en",
    );
    expect(messages).toHaveLength(2);
    const err = messages[1]!;
    expect(err.role).toBe("assistant");
    expect(err.isError).toBe(true);
    expect(err.streaming).toBe(false);
    expect(err.content).toMatch(/timed?\s*out|timeout/i);
    expect(err.content).not.toMatch(/Connection refused|stderr|rpc timeout/i);
  });

});

describe("context compact markers", () => {
  it("parseCompactContent reads host journal format", () => {
    const meta = parseCompactContent(
      "context_compact|auto|tokens:120000->40000\nkept auth design",
    );
    expect(meta?.trigger).toBe("auto");
    expect(meta?.tokensBefore).toBe(120000);
    expect(meta?.tokensAfter).toBe(40000);
    expect(meta?.summaryPreview).toBe("kept auth design");
  });

});

describe("tool activity", () => {
  it("parseToolStepContent", () => {
    const p = parseToolStepContent(
      "tool_step|completed|read|Read foo\n/tmp/foo",
    );
    expect(p?.status).toBe("completed");
    expect(p?.title).toBe("Read foo");
  });

  it("toolStepDisplayTitle prefers plain content title", () => {
    expect(
      toolStepDisplayTitle({
        id: "tool-1",
        role: "tool",
        content: "Listing files in private persona folder",
        marker: "tool_step",
      }),
    ).toBe("Listing files in private persona folder");
    expect(
      toolStepDisplayTitle({
        id: "tool-2",
        role: "tool",
        content: "tool_step|completed|read|Read foo",
        marker: "tool_step",
      }),
    ).toBe("Read foo");
  });

  it("never surfaces bare tool placeholder; prefers detail/path", () => {
    expect(
      toolStepDisplayTitle({
        id: "tool-3",
        role: "tool",
        content: "tool",
        toolDetail: "ls -la /tmp",
        marker: "tool_step",
      }),
    ).toBe("ls -la /tmp");
    expect(
      toolStepDisplayTitle({
        id: "tool-4",
        role: "tool",
        content: "tool",
        marker: "tool_step",
      }),
    ).toBe("");
  });
});
