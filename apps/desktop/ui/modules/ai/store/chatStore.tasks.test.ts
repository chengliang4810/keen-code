import { usePreferencesStore } from "@/modules/settings/preferences";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import type { UIMessage } from "@ai-sdk/react";
import {
  chats,
  seedMessages,
  useChatStore,
  flushPersist,
  manageArchivedSessions,
} from "./chatStore";
import * as persistence from "../lib/sessions";

vi.mock("../lib/sessions", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/sessions")>()),
  loadAll: vi.fn(),
  loadMessages: vi.fn(),
  saveActiveId: vi.fn(),
  saveSessionsList: vi.fn(),
  saveSessionsListAndFlush: vi.fn(),
  deleteArchivedSessionData: vi.fn(),
  saveMessages: vi.fn(),
  deleteSessionData: vi.fn(),
}));
vi.mock("./planStore", () => ({
  usePlanStore: { getState: () => ({ queue: [] }) },
}));
vi.mock("../lib/modelPrefs", () => ({ pushRecentModel: vi.fn() }));
vi.mock("./todoStore", () => ({
  useTodosStore: { getState: () => ({ clearSession: vi.fn() }) },
}));
vi.mock("@/modules/ai/tools/shell", () => ({
  releaseSessionShells: vi.fn().mockResolvedValue(undefined),
}));

const oldTask = {
  id: "old",
  title: "Implement login",
  projectId: "project-a",
  workspaceRoot: "D:/a",
  workspaceScope: "local",
  createdAt: 1,
  updatedAt: 2,
};
const otherTask = { ...oldTask, id: "other", projectId: "project-b" };
const message: UIMessage = {
  id: "user",
  role: "user",
  parts: [{ type: "text", text: "Implement login" }],
};

beforeEach(() => {
  vi.useFakeTimers();
  chats.clear();
  seedMessages.clear();
  vi.clearAllMocks();
  useChatStore.setState({
    sessions: [oldTask, otherTask],
    draftSession: null,
    activeSessionId: "old",
    sessionsHydrated: false,
    sessionsLoading: false,
    sessionsError: null,
    sessionLoading: false,
    sessionSubmitting: false,
  });
  useChatStore.getState().resetAgentMeta();
  vi.mocked(persistence.loadMessages).mockResolvedValue([message]);
  vi.mocked(persistence.saveSessionsListAndFlush)
    .mockReset()
    .mockResolvedValue();
  vi.mocked(persistence.deleteArchivedSessionData)
    .mockReset()
    .mockResolvedValue();
});

describe("conversation permissions", () => {
  it("isolates permissions per task and promotes draft permissions with its first message", () => {
    const state = useChatStore.getState();
    state.setSessionPermissionMode("old", "full-access");
    expect(useChatStore.getState().sessions[0].permissionMode).toBe(
      "full-access",
    );
    expect(useChatStore.getState().sessions[1].permissionMode).toBeUndefined();
    const draftId = state.newSession(null);
    expect(
      useChatStore.getState().draftSession?.permissionMode,
    ).toBeUndefined();
    state.setSessionPermissionMode(draftId, "edit");
    expect(state.newSession(null)).toBe(draftId);
    expect(useChatStore.getState().draftSession?.permissionMode).toBe("edit");
    state.persistMessages(draftId, [message]);
    expect(
      useChatStore.getState().sessions.find((session) => session.id === draftId)
        ?.permissionMode,
    ).toBe("edit");
    const nextId = state.newSession(null);
    expect(nextId).not.toBe(draftId);
    expect(
      useChatStore.getState().draftSession?.permissionMode,
    ).toBeUndefined();
  });

  it("defaults invalid permissions to auto approval and freezes running turns", () => {
    useChatStore.getState().setSessionPermissionMode("old", "invalid" as "ask");
    expect(useChatStore.getState().sessions[0].permissionMode).toBe("edit");
    useChatStore.getState().patchAgentMeta({ status: "streaming" });
    useChatStore.getState().setSessionPermissionMode("old", "full-access");
    expect(useChatStore.getState().sessions[0].permissionMode).toBe("edit");
  });
});

describe("conversation loading and activity", () => {
  it.each(["metadata", "messages"] as const)(
    "retains unread archives after a %s failure and retries without writes",
    async (failure) => {
      useChatStore.setState({ sessions: [], activeSessionId: null });
      vi.mocked(persistence.loadAll).mockResolvedValue({
        sessions: [oldTask],
        activeId: oldTask.id,
      });
      if (failure === "metadata")
        vi.mocked(persistence.loadAll).mockRejectedValueOnce(
          new Error("read failed"),
        );
      else
        vi.mocked(persistence.loadMessages).mockRejectedValueOnce(
          new Error("read failed"),
        );
      await useChatStore.getState().hydrateSessions();
      expect(useChatStore.getState()).toMatchObject({
        sessionsHydrated: false,
        sessionsLoading: false,
        sessionsError: "read failed",
        activeSessionId: null,
        sessions: [],
      });
      useChatStore.getState().newSession();
      useChatStore.getState().renameSession(oldTask.id, "Unread");
      useChatStore.getState().persistMessages(oldTask.id, [message]);
      expect(persistence.saveSessionsList).not.toHaveBeenCalled();
      expect(persistence.saveMessages).not.toHaveBeenCalled();
      await useChatStore.getState().hydrateSessions();
      expect(persistence.loadAll).toHaveBeenLastCalledWith(true);
      expect(useChatStore.getState()).toMatchObject({
        sessionsHydrated: true,
        sessionsLoading: false,
        sessionsError: null,
        activeSessionId: oldTask.id,
        sessions: [oldTask],
      });
      expect(seedMessages.get(oldTask.id)).toEqual([message]);
      expect(persistence.saveSessionsList).not.toHaveBeenCalled();
    },
  );

  it("coalesces simultaneous loading and retry requests", async () => {
    let finish!: (loaded: {
      sessions: (typeof oldTask)[];
      activeId: string;
    }) => void;
    vi.mocked(persistence.loadAll).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const first = useChatStore.getState().hydrateSessions();
    const second = useChatStore.getState().hydrateSessions();
    expect(first).toBe(second);
    expect(persistence.loadAll).toHaveBeenCalledOnce();
    expect(useChatStore.getState().sessionsLoading).toBe(true);
    finish({ sessions: [oldTask], activeId: oldTask.id });
    await first;
    expect(useChatStore.getState().sessionsLoading).toBe(false);
  });

  it("advances a titled task once at a run boundary while streaming and history replay keep the same activity", () => {
    vi.setSystemTime(1000);
    useChatStore.getState().persistMessages(oldTask.id, [message]);
    expect(useChatStore.getState().sessions[0].updatedAt).toBe(
      oldTask.updatedAt,
    );
    useChatStore.getState().recordSessionActivity(oldTask.id);
    expect(useChatStore.getState().sessions[0]).toMatchObject({
      title: oldTask.title,
      updatedAt: 1000,
    });
    const writes = vi.mocked(persistence.saveSessionsList).mock.calls.length;
    vi.setSystemTime(2000);
    for (let i = 0; i < 20; i++)
      useChatStore
        .getState()
        .persistMessages(oldTask.id, [
          message,
          {
            id: "assistant",
            role: "assistant",
            parts: [{ type: "text", text: `token-${i}` }],
          },
        ]);
    expect(useChatStore.getState().sessions[0].updatedAt).toBe(1000);
    expect(persistence.saveSessionsList).toHaveBeenCalledTimes(writes);
    useChatStore.getState().recordSessionActivity(oldTask.id);
    expect(useChatStore.getState().sessions[0].updatedAt).toBe(2000);
    useChatStore.getState().recordSessionActivity("deleted");
    expect(useChatStore.getState().sessions).toHaveLength(2);
  });
});

describe("settings archive management", () => {
  const archivedTask = { ...otherTask, archived: true };
  beforeEach(() => {
    useChatStore.setState({
      sessions: [oldTask, archivedTask],
      sessionsHydrated: true,
    });
  });
  it("restores a background conversation without switching the active task or its project", async () => {
    await manageArchivedSessions("restore", ["other"]);
    expect(useChatStore.getState().sessions).toEqual([
      oldTask,
      { ...otherTask, archived: false },
    ]);
    expect(useChatStore.getState().activeSessionId).toBe("old");
    expect(persistence.deleteArchivedSessionData).not.toHaveBeenCalled();
  });
  it("keeps the UI unchanged on save failure and allows retry", async () => {
    vi.mocked(persistence.saveSessionsListAndFlush).mockRejectedValueOnce(
      new Error("disk full"),
    );
    await expect(manageArchivedSessions("delete", ["other"])).rejects.toThrow(
      "disk full",
    );
    expect(useChatStore.getState().sessions).toEqual([oldTask, archivedTask]);
    expect(persistence.deleteArchivedSessionData).not.toHaveBeenCalled();
    expect(persistence.saveSessionsList).toHaveBeenCalledWith([
      oldTask,
      archivedTask,
    ]);
    await manageArchivedSessions("delete", ["other"]);
    expect(useChatStore.getState().sessions).toEqual([oldTask]);
  });
  it("preserves concurrent metadata updates and newly archived conversations", async () => {
    vi.mocked(persistence.saveSessionsListAndFlush).mockImplementationOnce(
      async () => {
        useChatStore.setState({
          sessions: [
            { ...oldTask, title: "Renamed" },
            archivedTask,
            { ...archivedTask, id: "new" },
          ],
        });
      },
    );
    await manageArchivedSessions("delete", ["other"]);
    expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual([
      "old",
      "new",
    ]);
    expect(useChatStore.getState().sessions[0].title).toBe("Renamed");
    expect(persistence.saveSessionsListAndFlush).toHaveBeenCalledTimes(2);
  });
  it("cleans the deleted conversation cache and message storage, allowing cleanup retry", async () => {
    seedMessages.set("other", [message]);
    vi.mocked(persistence.deleteArchivedSessionData).mockRejectedValueOnce(
      new Error("cleanup failed"),
    );
    await expect(manageArchivedSessions("delete", ["other"])).rejects.toThrow(
      "cleanup failed",
    );
    expect(seedMessages.has("other")).toBe(false);
    expect(useChatStore.getState().sessions).toEqual([oldTask]);
    await manageArchivedSessions("delete", ["other"]);
    expect(persistence.deleteArchivedSessionData).toHaveBeenLastCalledWith([
      "other",
    ]);
  });
  it.each(["thinking", "streaming", "awaiting-approval"] as const)(
    "rejects changes while %s",
    async (status) => {
      useChatStore.getState().patchAgentMeta({ status });
      await expect(manageArchivedSessions("delete", ["other"])).rejects.toThrow(
        "Finish the current agent operation",
      );
      expect(persistence.saveSessionsListAndFlush).not.toHaveBeenCalled();
    },
  );
  it("rejects before hydration, while loading/submitting, and for the active task", async () => {
    useChatStore.setState({ sessionsHydrated: false });
    await expect(manageArchivedSessions("restore", ["other"])).rejects.toThrow(
      "still loading",
    );
    useChatStore.setState({ sessionsHydrated: true, sessionLoading: true });
    await expect(manageArchivedSessions("restore", ["other"])).rejects.toThrow(
      "Finish",
    );
    useChatStore.setState({ sessionLoading: false, sessionSubmitting: true });
    await expect(manageArchivedSessions("restore", ["other"])).rejects.toThrow(
      "Finish",
    );
    useChatStore.setState({ sessionSubmitting: false });
    await expect(manageArchivedSessions("delete", ["old"])).rejects.toThrow(
      "current conversation",
    );
    expect(persistence.saveSessionsListAndFlush).not.toHaveBeenCalled();
  });
  it("serializes management and locks normal navigation during persistence", async () => {
    let finish!: () => void;
    vi.mocked(persistence.saveSessionsListAndFlush).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const pending = manageArchivedSessions("restore", ["other"]);
    await expect(manageArchivedSessions("delete", ["other"])).rejects.toThrow(
      "Finish",
    );
    expect(await useChatStore.getState().switchSession("other")).toBe(false);
    finish();
    await pending;
    expect(useChatStore.getState().activeSessionId).toBe("old");
  });
});
afterEach(() => {
  flushPersist();
  vi.useRealTimers();
});

describe("agent task lifecycle", () => {
  it("keeps a projectless draft out of history and persists it only on first submission", () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    expect(store.newSession(null)).toBe(id);
    store.bindSessionWorkspace(id, "D:/previous-project", "local");
    expect(useChatStore.getState().draftSession).toMatchObject({
      projectless: true,
      workspaceScope: "local",
    });
    expect(useChatStore.getState().draftSession?.projectId).toBeUndefined();
    expect(useChatStore.getState().draftSession?.workspaceRoot).toBeUndefined();
    expect(useChatStore.getState().sessions).toHaveLength(2);
    expect(persistence.saveSessionsList).not.toHaveBeenCalled();
    expect(store.newSession()).toBe(id);
    store.persistMessages(id, [message]);
    store.persistMessages(id, [message]);
    store.bindSessionWorkspace(id, "D:/previous-project", "local");
    const session = useChatStore
      .getState()
      .sessions.find((task) => task.id === id);
    expect(session).toMatchObject({
      id,
      projectless: true,
      workspaceScope: "local",
      title: "Implement login",
    });
    expect(session?.projectId).toBeUndefined();
    expect(session?.workspaceRoot).toBeUndefined();
    expect(
      useChatStore.getState().sessions.filter((task) => task.id === id),
    ).toHaveLength(1);
  });
  it("can reassign an unsent independent draft to a project without retaining the projectless flag", () => {
    const store = useChatStore.getState();
    const id = store.newSession(null);
    expect(
      store.newSession({ id: "project-b", root: "D:/b", scope: "local" }),
    ).toBe(id);
    expect(useChatStore.getState().draftSession?.projectless).toBeUndefined();
    expect(useChatStore.getState().draftSession?.workspaceRoot).toBe("D:/b");
  });
  it("restores, archives and deletes independent conversations without assigning a project", async () => {
    const independent = {
      ...oldTask,
      id: "independent",
      projectless: true,
      projectId: undefined,
      workspaceRoot: undefined,
    };
    vi.mocked(persistence.loadAll).mockResolvedValue({
      sessions: [oldTask, independent],
      activeId: independent.id,
    });
    await useChatStore.getState().hydrateSessions();
    expect(useChatStore.getState().activeSessionId).toBe(independent.id);
    useChatStore.getState().archiveSession(independent.id, true);
    expect(useChatStore.getState().draftSession?.projectless).toBe(true);
    useChatStore.getState().archiveSession(independent.id, false);
    expect(await useChatStore.getState().switchSession(independent.id)).toBe(
      true,
    );
    useChatStore.getState().deleteSession(independent.id);
    expect(useChatStore.getState().draftSession?.projectless).toBe(true);
    expect(useChatStore.getState().draftSession?.workspaceRoot).toBeUndefined();
  });
  it("creates a replacement in the same project when archiving its last active task", () => {
    useChatStore.getState().archiveSession("old", true);
    const state = useChatStore.getState();
    expect(state.activeSessionId).not.toBe("other");
    expect(state.draftSession).toMatchObject({
      projectId: "project-a",
      workspaceRoot: "D:/a",
      workspaceScope: "local",
    });
  });
  it("never restores an archived placeholder when all tasks are archived", async () => {
    vi.mocked(persistence.loadAll).mockResolvedValue({
      sessions: [{ ...oldTask, title: "New chat", archived: true }],
      activeId: "old",
    });
    await useChatStore.getState().hydrateSessions();
    expect(useChatStore.getState().activeSessionId).not.toBe("old");
    expect(
      useChatStore.getState().sessions.find((s) => s.id === "old")?.archived,
    ).toBe(true);
  });
  it("loads a same-project replacement's history when deleting the active task", async () => {
    useChatStore.setState({
      sessions: [oldTask, { ...oldTask, id: "same" }, otherTask],
    });
    useChatStore.getState().deleteSession("old");
    await Promise.resolve();
    expect(useChatStore.getState().activeSessionId).toBe("same");
    expect(seedMessages.get("same")).toEqual([message]);
  });
  it("restores the selected task and its messages instead of adding an empty task", async () => {
    vi.mocked(persistence.loadAll).mockResolvedValue({
      sessions: [oldTask],
      activeId: "old",
    });
    await useChatStore.getState().hydrateSessions();
    expect(useChatStore.getState().sessions).toEqual([oldTask]);
    expect(useChatStore.getState().activeSessionId).toBe("old");
    expect(seedMessages.get("old")).toEqual([message]);
  });
  it("creates multiple independent tasks within one project and preserves their binding", () => {
    const project = { id: "project-a", root: "D:/a", scope: "local" };
    const one = useChatStore.getState().newSession(project);
    useChatStore.getState().persistMessages(one, [message]);
    const two = useChatStore.getState().newSession(project);
    useChatStore.getState().persistMessages(two, [message]);
    expect(one).not.toBe(two);
    expect(
      useChatStore
        .getState()
        .sessions.filter((task) => task.id === one || task.id === two)
        .map((task) => task.projectId),
    ).toEqual([project.id, project.id]);
    useChatStore.getState().bindSessionWorkspace(one, "D:/b", "local");
    expect(
      useChatStore.getState().sessions.find((task) => task.id === one)
        ?.workspaceRoot,
    ).toBe("D:/a");
  });
  it("does not switch, archive, or create tasks while an approval is pending", () => {
    useChatStore.getState().patchAgentMeta({ status: "awaiting-approval" });
    useChatStore.getState().switchSession("other");
    useChatStore.getState().archiveSession("old", true);
    useChatStore.getState().newSession();
    expect(useChatStore.getState().activeSessionId).toBe("old");
    expect(useChatStore.getState().sessions).toEqual([oldTask, otherTask]);
  });
  it("ignores a late history response after the user creates a different task", async () => {
    let resolveHistory!: (messages: UIMessage[]) => void;
    vi.mocked(persistence.loadMessages).mockReturnValue(
      new Promise((resolve) => {
        resolveHistory = resolve;
      }),
    );
    useChatStore.getState().switchSession("other");
    const fresh = useChatStore
      .getState()
      .newSession({ id: "project-a", root: "D:/a", scope: "local" });
    resolveHistory([message]);
    await Promise.resolve();
    expect(useChatStore.getState().activeSessionId).toBe(fresh);
  });
  it("archives and restores a task without deleting its message history", () => {
    useChatStore.getState().archiveSession("other", true);
    expect(
      useChatStore.getState().sessions.find((task) => task.id === "other")
        ?.archived,
    ).toBe(true);
    useChatStore.getState().archiveSession("other", false);
    expect(
      useChatStore.getState().sessions.find((task) => task.id === "other")
        ?.archived,
    ).toBe(false);
    expect(persistence.deleteSessionData).not.toHaveBeenCalled();
  });
  it("reports completion only after the requested history has become active", async () => {
    let resolveHistory!: (messages: UIMessage[]) => void;
    vi.mocked(persistence.loadMessages).mockReturnValue(
      new Promise((resolve) => {
        resolveHistory = resolve;
      }),
    );
    const switched = useChatStore.getState().switchSession("other");
    expect(useChatStore.getState().activeSessionId).toBe("old");
    resolveHistory([message]);
    expect(await switched).toBe(true);
    expect(useChatStore.getState().activeSessionId).toBe("other");
  });
  it("reports a failed load without replacing the active conversation", async () => {
    vi.mocked(persistence.loadMessages).mockRejectedValue(
      new Error("unavailable"),
    );
    expect(await useChatStore.getState().switchSession("other")).toBe(false);
    expect(useChatStore.getState().activeSessionId).toBe("old");
    expect(useChatStore.getState().sessionLoading).toBe(false);
  });
  it("keeps repeated new-conversation entries and empty message updates out of history and disk", () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    expect(store.newSession()).toBe(id);
    store.persistMessages(id, []);
    store.persistMessages(id, [
      { ...message, parts: [{ type: "text", text: "   " }] },
    ]);
    store.persistMessages(id, [{ ...message, role: "assistant" }]);
    flushPersist();
    expect(useChatStore.getState().sessions).toEqual([oldTask, otherTask]);
    expect(persistence.saveSessionsList).not.toHaveBeenCalled();
    expect(persistence.saveActiveId).not.toHaveBeenCalled();
    expect(persistence.saveMessages).not.toHaveBeenCalled();
  });
  it("promotes a draft once on the first submitted user message, retaining its ID, title and project", () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    store.persistMessages(id, [message]);
    store.persistMessages(id, [
      message,
      { ...message, id: "reply", role: "assistant" },
    ]);
    expect(useChatStore.getState().draftSession).toBeNull();
    expect(
      useChatStore.getState().sessions.filter((session) => session.id === id),
    ).toEqual([
      expect.objectContaining({
        id,
        title: "Implement login",
        projectId: "project-a",
        workspaceRoot: "D:/a",
        workspaceScope: "local",
      }),
    ]);
    expect(persistence.saveSessionsList).toHaveBeenCalledTimes(1);
    expect(persistence.saveActiveId).toHaveBeenCalledWith(id);
    flushPersist();
    expect(persistence.saveMessages).toHaveBeenCalledWith(
      id,
      expect.arrayContaining([message]),
    );
  });
  it("changes the draft's project before submission without creating a second conversation", () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    expect(
      store.newSession({ id: "project-b", root: "D:/b", scope: "local" }),
    ).toBe(id);
    expect(useChatStore.getState().draftSession).toMatchObject({
      id,
      projectId: "project-b",
      workspaceRoot: "D:/b",
    });
    expect(useChatStore.getState().sessions).toHaveLength(2);
    store.persistMessages(id, [message]);
    expect(useChatStore.getState().sessions[0]).toMatchObject({
      projectId: "project-b",
      workspaceRoot: "D:/b",
    });
  });
  it("can create a conversation from an image-only first message", () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    store.persistMessages(id, [
      {
        ...message,
        parts: [
          {
            type: "file",
            mediaType: "image/png",
            url: "data:image/png;base64,preview",
          },
        ],
      },
    ]);
    expect(useChatStore.getState().draftSession).toBeNull();
    expect(useChatStore.getState().sessions[0]).toMatchObject({
      id,
      projectId: "project-a",
    });
  });
  it("locks navigation while the first submission runtime is loading", async () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    useChatStore.setState({ sessionSubmitting: true });
    expect(await store.switchSession("other")).toBe(false);
    expect(store.newSession()).toBe(id);
    expect(useChatStore.getState().sessions).toHaveLength(2);
    expect(useChatStore.getState().activeSessionId).toBe(id);
  });
  it("can resume an unsent draft after visiting an existing conversation", async () => {
    const store = useChatStore.getState();
    const id = store.newSession({
      id: "project-a",
      root: "D:/a",
      scope: "local",
    });
    expect(await store.switchSession("other")).toBe(true);
    expect(await store.switchSession(id)).toBe(true);
    expect(useChatStore.getState().activeSessionId).toBe(id);
    expect(useChatStore.getState().sessions).toHaveLength(2);
    expect(persistence.saveActiveId).not.toHaveBeenCalledWith(id);
  });
  it("starts on the new-conversation page with an empty history on a clean install", async () => {
    vi.mocked(persistence.loadAll).mockResolvedValue({
      sessions: [],
      activeId: null,
    });
    await useChatStore.getState().hydrateSessions();
    expect(useChatStore.getState().sessions).toEqual([]);
    expect(useChatStore.getState().draftSession?.id).toBe(
      useChatStore.getState().activeSessionId,
    );
    expect(persistence.saveSessionsList).not.toHaveBeenCalled();
  });
  it("ignores message updates from an already deleted conversation", () => {
    useChatStore.getState().persistMessages("deleted", [message]);
    flushPersist();
    expect(persistence.saveMessages).not.toHaveBeenCalled();
  });
});

describe("draft tool workspace ownership", () => {
  it("preserves a draft with tools when changing project and promotes each id only once", () => {
    const id = useChatStore.getState().newSession(null);
    const nextId = useChatStore
      .getState()
      .newSession({ id: "project-b", root: "D:/b", scope: "local" }, true);
    expect(nextId).not.toBe(id);
    expect(useChatStore.getState().sessions.filter((s) => s.id === id)).toEqual(
      [expect.objectContaining({ projectless: true, workspaceScope: "local" })],
    );
    useChatStore.getState().persistMessages(nextId, [message]);
    useChatStore.getState().persistMessages(nextId, [message]);
    expect(
      useChatStore.getState().sessions.filter((s) => s.id === nextId),
    ).toHaveLength(1);
    expect(
      useChatStore.getState().sessions.filter((s) => s.id === id),
    ).toHaveLength(1);
  });
  it("reuses the live draft id and tool record when reopening the same owner", () => {
    const id = useChatStore.getState().newSession(null);
    expect(useChatStore.getState().newSession(null, true)).toBe(id);
    expect(useChatStore.getState().sessions.some((s) => s.id === id)).toBe(
      false,
    );
    useChatStore.getState().persistMessages(id, [message]);
    expect(
      useChatStore.getState().sessions.filter((s) => s.id === id),
    ).toHaveLength(1);
  });
});

describe("conversation reasoning selection", () => {
  beforeEach(() => {
    usePreferencesStore.setState({
      customEndpoints: [
        {
          id: "reasoning",
          name: "Test",
          baseURL: "https://example.com/v1",
          modelId: "gpt-5.6",
          contextLimit: 128000,
          models: [{ id: "gpt-5.6", reasoningLevels: ["low", "high"] }],
        },
      ],
    });
  });
  it("isolates a level per session and promotes draft settings with its first message", () => {
    const state = useChatStore.getState();
    state.setSessionReasoningLevel("old", "compat-reasoning/gpt-5.6", "low");
    expect(useChatStore.getState().sessions[0].reasoningSelection?.level).toBe(
      "low",
    );
    expect(
      useChatStore.getState().sessions[1].reasoningSelection,
    ).toBeUndefined();
    const id = state.newSession(null);
    state.setSessionReasoningLevel(id, "compat-reasoning/gpt-5.6", "high");
    state.persistMessages(id, [message]);
    expect(
      useChatStore.getState().sessions.find((session) => session.id === id)
        ?.reasoningSelection?.level,
    ).toBe("high");
    state.newSession(null);
    expect(
      useChatStore.getState().draftSession?.reasoningSelection,
    ).toBeUndefined();
  });
  it("ignores unknown model levels and prevents changes during a running turn", () => {
    const state = useChatStore.getState();
    state.setSessionReasoningLevel(
      "old",
      "compat-reasoning/gpt-5.6",
      "invalid",
    );
    expect(
      useChatStore.getState().sessions[0].reasoningSelection,
    ).toBeUndefined();
    useChatStore.setState({ sessionSubmitting: true });
    state.setSessionReasoningLevel("old", "compat-reasoning/gpt-5.6", "low");
    expect(
      useChatStore.getState().sessions[0].reasoningSelection,
    ).toBeUndefined();
  });
});
