import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { useSessionNavigation, type UseSessionNavigationOptions } from "./useSessionNavigation";
import { createAcpWorkspaceState } from "@/lib/acp/store";
import { IDLE_SNAPSHOT } from "@/lib/session";

it("从备用模型会话新建草稿时恢复保存的主模型，保留原会话缓存", async () => {
  const setModelId = vi.fn();
  const options: UseSessionNavigationOptions = {
    locale: "zh",
    navigationRefs: {
      draftKeyRef: { current: 0 }, draftNavigationSnapshotRef: { current: null }, viewEpochRef: { current: 1 },
      viewingSessionIdRef: { current: "hy4-session" }, openingSessionIdRef: { current: null }, openingSessionEpochRef: { current: null },
    },
    route: { navigateWorkbench: vi.fn() },
    runtime: {
      isTauri: () => true, workspaceRef: { current: createAcpWorkspaceState() }, commitWorkspace: vi.fn(),
      connect: vi.fn(), observeHostActiveTurn: vi.fn(), replayHistory: vi.fn(), applyViewProjection: vi.fn(), refreshSessions: vi.fn(),
      liveHostRef: { current: { ...IDLE_SNAPSHOT, sessionId: "hy4-session", state: "ready" } },
      messagesRef: { current: [] }, messagesBySessionRef: { current: new Map() },
    },
    sidebar: { projects: [], activeProject: null, setActiveProject: vi.fn(), setExpandedProjects: vi.fn(), setHistoryOpen: vi.fn(), setCompletedUnreadIds: vi.fn(), pendingAskUserBySessionRef: { current: new Map() } },
    composer: { draftRef: { current: "" }, attachmentsRef: { current: [] }, setDraft: vi.fn(), setAttachments: vi.fn(), requestComposerFocus: vi.fn(), sendQueue: { clearDraftQueue: vi.fn() } },
    providers: { modelBySessionRef: { current: new Map([["hy4-session", "hy4-preview"]]) }, configuredModelsRef: { current: [
      { id: "hy3", label: "hy3", isDefault: true }, { id: "hy4-preview", label: "hy4-preview" },
    ] }, setModelId },
    ui: {
      session: { ...IDLE_SNAPSHOT, sessionId: "hy4-session" }, setSession: vi.fn(), setMessages: vi.fn(), setLiveHost: vi.fn(),
      setLiveMap: vi.fn(), setContextUsage: vi.fn(), setAskUser: vi.fn(), setRetryStatus: vi.fn(), setLocalError: vi.fn(), closeSummary: vi.fn(),
    },
  };
  let navigation!: ReturnType<typeof useSessionNavigation>;
  function Harness() { navigation = useSessionNavigation(options); return null; }
  renderToString(createElement(Harness));
  await navigation.newChat(null);
  expect(setModelId).toHaveBeenCalledWith("hy3");
  expect(options.providers.modelBySessionRef.current.get("hy4-session")).toBe("hy4-preview");
  expect(options.navigationRefs.viewingSessionIdRef.current).toBeNull();
  expect(options.runtime.connect).not.toHaveBeenCalled();
});
