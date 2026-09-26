import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import type { ViewFocus } from "@/lib/viewFocus";
import { useSessionDraftSend, type UseSessionDraftSendOptions } from "./useSessionDraftSend";

function fakeRuntime(viewFocus: ViewFocus) {
  return { currentViewFocus: () => ({ ...viewFocus }) };
}

it.each(["ready", "streaming"] as const)("%s 下非法 Goal 保留输入且不发送/排队", async (sessionState) => {
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState, connecting: false,
    draft: "中".repeat(22000), attachments: [], hasConfiguredModel: true,
    goalModeSessionKey: "s", planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(),
    runtime: fakeRuntime({ sessionId: "s", epoch: 1 }),
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  expect(options.executeSend).not.toHaveBeenCalled();
  expect(options.sendQueue.enqueue).not.toHaveBeenCalled();
  expect(options.ui.setDraft).not.toHaveBeenCalled();
  expect(options.ui.setAttachments).not.toHaveBeenCalled();
  expect(options.ui.setGoalModeSessionKey).not.toHaveBeenCalled();
  expect(options.ui.setLocalError).toHaveBeenCalledWith(expect.stringContaining("65536"));
});

it("直接发送失败时回填原文与附件，避免输入丢失", async () => {
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "ready", connecting: false,
    draft: "hello", attachments: [{ name: "a.png" } as never], hasConfiguredModel: true,
    goalModeSessionKey: "s", planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn().mockResolvedValue(false),
    runtime: fakeRuntime({ sessionId: "s", epoch: 1 }),
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  expect(options.executeSend).toHaveBeenCalledTimes(1);
  // 先清空再回填：setDraft 被调用两次（"" 与原文）。
  expect(options.ui.setDraft).toHaveBeenNthCalledWith(1, "");
  expect(options.ui.setDraft).toHaveBeenNthCalledWith(2, "hello");
  expect(options.ui.setAttachments).toHaveBeenLastCalledWith(options.attachments);
  expect(options.ui.setGoalModeSessionKey).toHaveBeenLastCalledWith("s");
});

it("直接发送成功时不回填", async () => {
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "ready", connecting: false,
    draft: "hello", attachments: [], hasConfiguredModel: true,
    goalModeSessionKey: null, planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn().mockResolvedValue(true),
    runtime: fakeRuntime({ sessionId: "s", epoch: 1 }),
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  expect(options.ui.setDraft).toHaveBeenCalledTimes(1);
  expect(options.ui.setDraft).toHaveBeenCalledWith("");
});

it("附件仍在上传或失败时不发送也不入队", async () => {
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "ready", connecting: false,
    draft: "hello",
    attachments: [{ path: "pending://1", name: "clip.png", isDir: false, uploadStatus: "uploading" }],
    hasConfiguredModel: true,
    goalModeSessionKey: null, planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(),
    runtime: fakeRuntime({ sessionId: "s", epoch: 1 }),
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));

  await result.send();

  expect(options.executeSend).not.toHaveBeenCalled();
  expect(options.sendQueue.enqueue).not.toHaveBeenCalled();
  expect(options.ui.setDraft).not.toHaveBeenCalled();
});

it("发送期间切走视图时不回填旧会话草稿", async () => {
  // 连接耗时窗口内用户切到会话 b：失败回填不得写进 b 的输入框。
  const viewFocus: ViewFocus = { sessionId: "s", epoch: 1 };
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "ready", connecting: false,
    draft: "hello", attachments: [{ name: "a.png" } as never], hasConfiguredModel: true,
    goalModeSessionKey: "s", planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(async () => {
      viewFocus.sessionId = "b";
      return false;
    }),
    runtime: fakeRuntime(viewFocus),
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  // 只有 clearComposerAfterSubmit 的清空调用，没有回填。
  expect(options.ui.setDraft).toHaveBeenCalledTimes(1);
  expect(options.ui.setDraft).toHaveBeenCalledWith("");
  expect(options.ui.setAttachments).toHaveBeenCalledTimes(1);
  expect(options.ui.setGoalModeSessionKey).not.toHaveBeenCalledWith("s");
  expect(options.ui.setLocalError).not.toHaveBeenCalled();
});

it("活跃目标执行中的用户输入直接 steer，不停止也不另开队列轮次", async () => {
  const steerQueuedItem = vi.fn().mockResolvedValue(undefined);
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "streaming", connecting: false,
    draft: "接下来先核对文件内容", attachments: [], hasConfiguredModel: true,
    goalModeSessionKey: null, planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(),
    runtime: { ...fakeRuntime({ sessionId: "s", epoch: 1 }),
      acpWorkspaceRef: { current: { sessions: { s: { goal: { goal: { status: "active" } } } } } } as never },
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    steerQueuedItem,
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  expect(steerQueuedItem).toHaveBeenCalledWith(expect.objectContaining({
    storedDisplay: "接下来先核对文件内容", createGoal: false,
  }));
  expect(options.sendQueue.enqueue).not.toHaveBeenCalled();
  expect(options.executeSend).not.toHaveBeenCalled();
  expect(options.ui.setDraft).toHaveBeenCalledWith("");
});

it("暂停目标的当前 Turn 仍接受方向纠正", async () => {
  const steerQueuedItem = vi.fn().mockResolvedValue(undefined);
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState: "streaming", connecting: false,
    draft: "先检查文本", attachments: [], hasConfiguredModel: true,
    goalModeSessionKey: null, planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(),
    runtime: { ...fakeRuntime({ sessionId: "s", epoch: 1 }),
      acpWorkspaceRef: { current: { sessions: { s: { goal: { goal: { status: "paused" } } } } } } as never },
    sendQueue: { enqueue: vi.fn(), releaseFlushHold: vi.fn(), bindDraft: vi.fn() },
    steerQueuedItem,
    ui: {
      setDraft: vi.fn(), setAttachments: vi.fn(), setGoalModeSessionKey: vi.fn(), setLocalError: vi.fn(),
      promptHistoryIndexRef: { current: null }, setPromptHistoryIndex: vi.fn(), setPromptHistoryOpen: vi.fn(),
      setPromptHistoryFilter: vi.fn(), setPromptHistoryActive: vi.fn(), setPromptHistoryFocusFilter: vi.fn(),
    },
  };
  let result!: ReturnType<typeof useSessionDraftSend>;
  function Harness() { result = useSessionDraftSend(options); return null; }
  renderToString(createElement(Harness));
  await result.send();
  expect(steerQueuedItem).toHaveBeenCalledTimes(1);
  expect(options.sendQueue.enqueue).not.toHaveBeenCalled();
  expect(options.executeSend).not.toHaveBeenCalled();
});
