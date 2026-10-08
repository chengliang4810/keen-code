import { beforeEach, describe, expect, it, vi } from "vitest";
import { testProviderModel } from "@/settings/lib/testProviderModel";

const mocks = vi.hoisted(() => ({
  buildLanguageModel: vi.fn(),
  generateText: vi.fn(),
}));
vi.mock("ai", () => ({ generateText: mocks.generateText }));
vi.mock("@/modules/ai/lib/agent", () => ({
  buildLanguageModel: mocks.buildLanguageModel,
}));

describe("provider model connectivity", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.buildLanguageModel.mockResolvedValue("test-language-model");
    mocks.generateText.mockResolvedValue({ text: "OK" });
  });

  it.each(["chat_completions", "responses", "messages"] as const)(
    "tests the configured %s protocol and exact model without a conversation",
    async (protocol) => {
      await testProviderModel({
        modelId: "team/exact-model",
        baseURL: "https://example.com/v1",
        protocol,
        apiKey: "test-only-key",
        signal: new AbortController().signal,
      });
      expect(mocks.buildLanguageModel).toHaveBeenCalledWith(
        "openai-compatible",
        expect.any(Object),
        "team/exact-model",
        { openaiCompatibleBaseURL: "https://example.com/v1", protocol },
        "test-only-key",
      );
      expect(mocks.generateText).toHaveBeenCalledOnce();
      const request = mocks.generateText.mock.calls[0][0];
      expect(request.model).toBe("test-language-model");
      expect(request.maxRetries).toBe(0);
      expect(request.tools).toBeUndefined();
      expect(request.messages).toBeUndefined();
    },
  );

  it("does not start a request after the row has been removed", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      testProviderModel({
        modelId: "test-model",
        baseURL: "https://example.com/v1",
        protocol: "responses",
        apiKey: null,
        signal: controller.signal,
      }),
    ).rejects.toThrow();
    expect(mocks.buildLanguageModel).not.toHaveBeenCalled();
    expect(mocks.generateText).not.toHaveBeenCalled();
  });
});
