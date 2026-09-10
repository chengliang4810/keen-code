import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { useSessionNavigation, type UseSessionNavigationOptions } from "./useSessionNavigation";
import { createAcpWorkspaceState } from "@/lib/acp/store";
import { IDLE_SNAPSHOT } from "@/lib/session";

function createHarness() {
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
  options.composer.setDraft = vi.fn((value) => {
    options.composer.draftRef.current = typeof value === "function" ? value(options.composer.draftRef.current) : value;
  });
  options.composer.setAttachments = vi.fn((value) => {
    options.composer.attachmentsRef.current = typeof value === "function" ? value(options.composer.attachmentsRef.current) : value;
  });
  return { navigation, options, setModelId };
}

afterEach(() => vi.unstubAllGlobals());

it("从备用模型会话新建草稿时恢复保存的主模型，保留原会话缓存", async () => {
  const { navigation, options, setModelId } = createHarness();
  await navigation.newChat(null);
  expect(setModelId).toHaveBeenCalledWith("hy3");
  expect(options.providers.modelBySessionRef.current.get("hy4-session")).toBe("hy4-preview");
  expect(options.navigationRefs.viewingSessionIdRef.current).toBeNull();
  expect(options.runtime.connect).not.toHaveBeenCalled();
});

it("连续导航隔离两会话草稿、附件和失败气泡，迟到恢复不清空新输入", async () => {
  vi.stubGlobal("window", {});
  const { navigation, options } = createHarness();
  const row = (id: string) => ({ id, title: id, projectId: null, updatedAt: "", archived: false, pinned: false });
  const file = { path: "D:/cart/a.txt", name: "a.txt", isDir: false };
  options.composer.draftRef.current = "CART";
  options.composer.attachmentsRef.current = [file];
  options.runtime.messagesRef.current = [{ id: "u-failed-cart", role: "user", content: "CART failed" }];
  let finish!: (value: Awaited<ReturnType<typeof options.runtime.connect>>) => void;
  options.runtime.connect = vi.fn(() => new Promise<Awaited<ReturnType<typeof options.runtime.connect>>>((resolve) => { finish = resolve; }));
  const opening = navigation.openSession(row("dashboard"));
  expect(options.composer.draftRef.current).toBe("");
  expect(options.composer.attachmentsRef.current).toEqual([]);
  expect(options.runtime.messagesRef.current).toEqual([]);
  options.composer.draftRef.current = "DASH";
  // dashboard 仍在连接，先回 cart；其迟到完成不能改写 cart 草稿。
  const finishDashboard = finish;
  options.runtime.connect = vi.fn().mockResolvedValue({ sessionId: "hy4-session", state: "ready" });
  await navigation.openSession(row("hy4-session"));
  expect(options.composer.draftRef.current).toBe("CART");
  expect(options.composer.attachmentsRef.current).toEqual([file]);
  finishDashboard({ sessionId: "dashboard", state: "ready" } as Awaited<ReturnType<typeof options.runtime.connect>>);
  await opening;
  expect(options.composer.draftRef.current).toBe("CART");
  await navigation.openSession(row("dashboard"));
  expect(options.composer.draftRef.current).toBe("DASH");
  expect(options.composer.attachmentsRef.current).toEqual([]);
  expect(options.runtime.messagesRef.current).toEqual([]);
  options.composer.draftRef.current = "";
  await navigation.newChat(null);
  await navigation.openSession(row("dashboard"));
  expect(options.composer.draftRef.current).toBe("");
});
