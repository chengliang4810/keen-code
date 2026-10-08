import { describe, expect, it } from "vitest";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import { createNativeEventMapper } from "@/modules/ai/lib/nativeStream";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";

const options = (modelId: string): RunAgentOptions => ({
  keys: EMPTY_PROVIDER_KEYS,
  modelId,
  toolContext: {} as RunAgentOptions["toolContext"],
  uiMessages: [],
});

describe("native protocol routing", () => {
  it("chooses built-in protocols without model-name guessing", () => {
    expect(resolveNativeModel(options("gpt-5.6"))?.protocol).toBe("responses");
    const anthropic = resolveNativeModel(options("claude-sonnet-5"));
    expect(anthropic?.protocol).toBe("messages");
    expect(anthropic?.allowPrivateNetwork).toBe(false);
  });
  it("preserves explicit custom protocol and defaults legacy endpoints to Chat Completions", () => {
    const opts = {
      ...options("compat-local"),
      customEndpoints: [
        {
          id: "local",
          name: "test",
          modelId: "model",
          baseURL: "http://127.0.0.1:1234/v1",
          contextLimit: 128_000,
        },
      ],
    };
    expect(resolveNativeModel(opts)?.protocol).toBe("chat_completions");
    for (const protocol of [
      "messages",
      "responses",
      "chat_completions",
    ] as const) {
      expect(
        resolveNativeModel({
          ...opts,
          customEndpoints: [{ ...opts.customEndpoints[0], protocol }],
        })?.protocol,
      ).toBe(protocol);
    }
    expect(resolveNativeModel(opts)?.secretAccount).toBe(
      "compat-local-api-key",
    );
  });
});

describe("native event projection", () => {
  it("keeps reasoning and text blocks scoped to each round and closes them before the next round", () => {
    const map = createNativeEventMapper({});
    map({ type: "model", round: 1, event: { type: "message_start" } });
    expect(
      map({
        type: "model",
        round: 1,
        event: { type: "reasoning_delta", index: 0, delta: "think" },
      }),
    ).toEqual([
      { type: "reasoning-start", id: "1:reasoning:0" },
      { type: "reasoning-delta", id: "1:reasoning:0", delta: "think" },
    ]);
    expect(
      map({ type: "model", round: 2, event: { type: "message_start" } }),
    ).toEqual([
      { type: "reasoning-end", id: "1:reasoning:0" },
      { type: "finish-step" },
      { type: "start-step" },
    ]);
  });
  it("projects native edits into existing approval/diff cards and retains the authoritative transcript once", () => {
    const map = createNativeEventMapper({});
    expect(
      map({
        type: "tool_input",
        id: "c1",
        name: "Edit",
        input: { file_path: "src/a.ts", old_string: "a", new_string: "b" },
      })[0],
    ).toMatchObject({
      type: "tool-input-available",
      toolName: "edit",
      input: { path: "src/a.ts", file_path: "src/a.ts" },
    });
    expect(
      map({
        type: "approval",
        id: "rcode:run:c1",
        toolCallId: "c1",
        name: "Edit",
        input: {},
      })[0],
    ).toMatchObject({ approvalId: "rcode:run:c1" });
    expect(
      map({
        type: "tool_result",
        result: {
          toolCallId: "c1",
          isError: false,
          content: [{ type: "text", text: "done" }],
        },
      })[0],
    ).toMatchObject({ type: "tool-output-available", output: "done" });
    const transcript = {
      type: "transcript" as const,
      id: "commit-1",
      messages: [{ opaque: "signed-reasoning" }],
    };
    expect(map(transcript)[0]).toMatchObject({
      type: "data-rcode-messages",
      data: { messages: transcript.messages },
    });
    expect(map(transcript)).toEqual([]);
  });
  it("resolves pending tool cards on error and emits one finish", () => {
    const map = createNativeEventMapper({});
    map({ type: "tool_input", id: "c1", name: "Write", input: {} });
    expect(
      map({ type: "error", message: "connection closed" })[0],
    ).toMatchObject({ type: "tool-output-error", toolCallId: "c1" });
    expect(
      map({ type: "finish", cancelled: false }).filter(
        (c) => c.type === "finish",
      ),
    ).toHaveLength(1);
    expect(map({ type: "end" })).toEqual([]);
  });
  it("settles an awaiting native approval when the runner is cancelled", () => {
    const map = createNativeEventMapper({});
    map({ type: "tool_input", id: "c1", name: "Write", input: {} });
    map({
      type: "approval",
      id: "rcode:run:c1",
      toolCallId: "c1",
      name: "Write",
      input: {},
    });
    const chunks = map({ type: "finish", cancelled: true });
    expect(chunks).toContainEqual({
      type: "tool-output-error",
      toolCallId: "c1",
      errorText: "操作已取消",
      providerMetadata: { rcode: { cancelled: true } },
    });
    expect(chunks).toContainEqual({ type: "abort" });
    expect(map({ type: "end" })).toEqual([]);
  });
});
