import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createAcpWorkspaceState, emptySession } from "@/lib/acp/store";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import type { UseSessionEditResendOptions } from "./useSessionEditResend";
import { useSessionEditResend } from "./useSessionEditResend";

/** 在合法 React SSR 上下文中捕获真实编辑重发 Hook。 */
function renderEditResend(
  options: UseSessionEditResendOptions,
): ReturnType<typeof useSessionEditResend> {
  let captured!: ReturnType<typeof useSessionEditResend>;

  /** 测试专用 Hook 宿主，不渲染可见节点。 */
  function Harness() {
    captured = useSessionEditResend(options);
    return null;
  }

  renderToString(createElement(Harness));
  return captured;
}

/** 构造真实 Hook 所需的最小 Session、ACP view 和边界端口。 */
function makeOptions(
  sessionState: SessionSnapshot["state"] = "ready",
) {
  const sessionId = "session-edit";
  const workspace = createAcpWorkspaceState();
  const view = emptySession(sessionId);
  view.replay.loaded = true;
  workspace.sessions[sessionId] = view;
  const rewind = vi.fn().mockResolvedValue({
    sessionId,
    archivedSessionId: "session-archived",
    throughJournalSequence: 1,
    revertedFiles: false,
  });
  const executeSend = vi.fn().mockResolvedValue(true);
  const options = {
    locale: "zh",
    session: {
      sessionId,
      state: sessionState,
      lastError: null,
      streamingMessageId: null,
      backend: "acp",
    },
    planModeSessionKey: null,
    ultraModeSessionKey: null,
    api: { rewind } as unknown as UseSessionEditResendOptions["api"],
    runtime: {
      acpWorkspaceRef: { current: workspace },
      applyViewProjectionRef: { current: vi.fn() },
      commitWorkspace: vi.fn(),
      patchSessionMessages: vi.fn(),
      refreshSessions: vi.fn().mockResolvedValue(undefined),
      updateSessionPreference: vi.fn(),
    },
    ui: { setLocalError: vi.fn() },
    state: { sendInFlightRef: { current: false } },
    executeSend,
  } as unknown as UseSessionEditResendOptions & {
    api: UseSessionEditResendOptions["api"] & { rewind: typeof rewind };
    executeSend: typeof executeSend;
  };
  return { options, view, rewind, executeSend };
}

/** 用于触发编辑重发的最小末条用户消息。 */
const message: ChatMessage = {
  id: "user-1",
  role: "user",
  content: "原始问题",
};

describe("useSessionEditResend recovery barrier", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it.each([
    ["恢复中", "connecting" as const],
    ["运行中", "streaming" as const],
    ["空闲", "idle" as const],
    ["已断开", "disconnected" as const],
  ])("非 ready 的 %s 不调用 rewind 或 executeSend", async (_label, state) => {
    const { options, rewind, executeSend } = makeOptions(state);
    const edit = renderEditResend(options);

    await expect(edit(message, "修改后")).resolves.toBe(false);
    expect(rewind).not.toHaveBeenCalled();
    expect(executeSend).not.toHaveBeenCalled();
  });

  it.each([
    ["未加载", (view: ReturnType<typeof emptySession>) => { view.replay.loaded = false; }],
    ["恢复中", (view: ReturnType<typeof emptySession>) => { view.replay.restoring = true; }],
    ["已冻结", (view: ReturnType<typeof emptySession>) => { view.delivery.frozen = true; }],
  ])("真实 view 为%s时不调用 rewind 或 executeSend", async (_label, mutateView) => {
    const { options, view, rewind, executeSend } = makeOptions("ready");
    mutateView(view);
    const edit = renderEditResend(options);

    await expect(edit(message, "修改后")).resolves.toBe(false);
    expect(rewind).not.toHaveBeenCalled();
    expect(executeSend).not.toHaveBeenCalled();
  });

  it("React session.state 仍为 ready 但最新真实 view 正在恢复时拒绝", async () => {
    const { options, view, rewind, executeSend } = makeOptions("ready");
    view.replay.restoring = true;
    const edit = renderEditResend(options);

    await expect(edit(message, "修改后")).resolves.toBe(false);
    expect(rewind).not.toHaveBeenCalled();
    expect(executeSend).not.toHaveBeenCalled();
  });

  it("ready 且真实 view 已加载时沿用 rewind 后 executeSend 一次", async () => {
    const { options, rewind, executeSend } = makeOptions("ready");
    const edit = renderEditResend(options);

    await expect(edit(message, "修改后")).resolves.toBe(true);
    expect(rewind).toHaveBeenCalledOnce();
    expect(executeSend).toHaveBeenCalledOnce();
    expect(executeSend).toHaveBeenCalledWith(expect.objectContaining({
      storedDisplay: "修改后",
      targetSessionId: "session-edit",
    }));
  });
});
