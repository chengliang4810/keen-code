import { describe, it, expect, vi } from "vitest";

vi.mock("@/modules/ai/lib/native", () => ({
  native: { defaultChatDirectory: vi.fn() },
}));

it("coalesces preparation, retries a failed creation and caches only an authorized directory", async () => {
  vi.resetModules();
  const { native } = await import("@/modules/ai/lib/native");
  const { ensureDefaultChatDirectory, getDefaultChatDirectory } = await import(
    "@/modules/ai/lib/defaultChatDirectory"
  );
  vi.mocked(native.defaultChatDirectory).mockRejectedValueOnce(
    new Error("denied"),
  );
  const first = ensureDefaultChatDirectory();
  expect(ensureDefaultChatDirectory()).toBe(first);
  await expect(first).rejects.toThrow("denied");
  expect(getDefaultChatDirectory()).toBeNull();
  vi.mocked(native.defaultChatDirectory).mockResolvedValue(
    "C:/Users/test/.rcode/chat/default",
  );
  expect(await ensureDefaultChatDirectory()).toBe(
    "C:/Users/test/.rcode/chat/default",
  );
  expect(await ensureDefaultChatDirectory()).toBe(getDefaultChatDirectory());
  expect(native.defaultChatDirectory).toHaveBeenCalledTimes(2);
});

describe("independent session binding", () => {
  it("binds only the prepared directory and refuses an unrelated project root", async () => {
    const { ensureDefaultChatDirectory } = await import(
      "@/modules/ai/lib/defaultChatDirectory"
    );
    const { useChatStore, getTaskWorkspace } = await import(
      "@/modules/ai/store/chatStore"
    );
    useChatStore.setState({
      sessions: [],
      draftSession: null,
      activeSessionId: null,
      sessionSubmitting: false,
      sessionLoading: false,
    });
    useChatStore.getState().resetAgentMeta();
    const root = await ensureDefaultChatDirectory();
    const id = useChatStore.getState().newSession(null);
    expect(useChatStore.getState().draftSession?.workspaceRoot).toBe(root);
    expect(getTaskWorkspace(id)).toBe(root);
    useChatStore
      .getState()
      .bindSessionWorkspace(id, "D:/other-project", "local");
    expect(useChatStore.getState().draftSession?.workspaceRoot).toBe(root);
  });
});
