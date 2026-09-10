import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { useSessionDraftSend, type UseSessionDraftSendOptions } from "./useSessionDraftSend";

it.each(["ready", "streaming"] as const)("%s 下非法 Goal 保留输入且不发送/排队", async (sessionState) => {
  const options: UseSessionDraftSendOptions = {
    locale: "zh", sessionId: "s", sessionState, connecting: false,
    draft: "中".repeat(22000), attachments: [], hasConfiguredModel: true,
    goalModeSessionKey: "s", planModeSessionKey: null, ultraModeSessionKey: null,
    executeSend: vi.fn(),
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
