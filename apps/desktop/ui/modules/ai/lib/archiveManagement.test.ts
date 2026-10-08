import { beforeEach, describe, expect, it, vi } from "vitest";
import { create } from "zustand";
import {
  requestArchives,
  watchArchives,
} from "@/modules/ai/lib/archiveManagement";
import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type { SpaceMeta } from "@/modules/spaces";
import {
  useChatStore,
  manageArchivedSessions,
} from "@/modules/ai/store/chatStore";
import { useSpaces } from "@/modules/spaces";

vi.mock("@/modules/ai/store/chatStore", async () => {
  const { create } = await import("zustand");
  return {
    useChatStore: create(() => ({
      sessions: [] as SessionMeta[],
      sessionsHydrated: true,
      sessionLoading: false,
      sessionSubmitting: false,
      agentMeta: { status: "idle" },
    })),
    isSessionNavigationLocked: () => false,
    manageArchivedSessions: vi.fn(),
  };
});
vi.mock("@/modules/spaces", () => ({
  useSpaces: create(() => ({ spaces: [] as SpaceMeta[], hydrated: true })),
}));
const archived: SessionMeta = {
  id: "a",
  title: "A",
  archived: true,
  createdAt: 1,
  updatedAt: 2,
};
beforeEach(() => {
  vi.mocked(manageArchivedSessions).mockReset().mockResolvedValue();
  useChatStore.setState({
    sessions: [archived, { ...archived, id: "live", archived: false }],
    sessionsHydrated: true,
    sessionLoading: false,
  });
  useSpaces.setState({ spaces: [], hydrated: true });
});
describe("same-window archive management", () => {
  it("reads the shared task store and excludes active conversations", async () => {
    expect((await requestArchives()).sessions).toEqual([archived]);
    expect(manageArchivedSessions).not.toHaveBeenCalled();
  });
  it("returns fresh shared state after persistence completes", async () => {
    vi.mocked(manageArchivedSessions).mockImplementationOnce(async () => {
      useChatStore.setState({ sessions: [{ ...archived, archived: false }] });
    });
    expect((await requestArchives("restore", ["a"])).sessions).toEqual([]);
    expect(manageArchivedSessions).toHaveBeenCalledWith("restore", ["a"]);
  });
  it("propagates management errors without applying another write", async () => {
    vi.mocked(manageArchivedSessions).mockRejectedValueOnce(
      new Error("disk failed"),
    );
    await expect(requestArchives("delete", ["a"])).rejects.toThrow(
      "disk failed",
    );
    expect(useChatStore.getState().sessions[0]).toBe(archived);
  });
  it("waits for shared project and conversation hydration and exposes loading locks", async () => {
    useSpaces.setState({ hydrated: false });
    await expect(requestArchives()).rejects.toThrow("still loading");
    useSpaces.setState({ hydrated: true });
    useChatStore.setState({ sessionsHydrated: false });
    await expect(requestArchives()).rejects.toThrow("still loading");
    useChatStore.setState({ sessionsHydrated: true, sessionLoading: true });
    expect((await requestArchives()).locked).toBe(true);
  });
  it("watches shared metadata and execution state only while the page is mounted", async () => {
    const refresh = vi.fn();
    const stop = await watchArchives(refresh);
    useChatStore.setState({ sessions: [] });
    useChatStore.setState({ sessionLoading: true });
    useSpaces.setState({ spaces: [] });
    expect(refresh).toHaveBeenCalledTimes(3);
    stop();
    useChatStore.setState({ sessionLoading: false });
    expect(refresh).toHaveBeenCalledTimes(3);
  });
});
