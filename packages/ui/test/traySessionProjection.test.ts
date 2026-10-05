import { describe, expect, test } from "vitest";
import {
  buildTrayMenuProjection,
  findTraySession,
  isValidExitRequestedPayload,
  type TraySessionProjectionItem,
} from "../src/root/traySessionProjection.js";

const items: TraySessionProjectionItem[] = [
  {
    taskId: "task-1",
    title: "First task",
    workspacePath: "C:/work/one",
  },
  {
    taskId: "task-1",
    title: "Duplicate task id",
    workspacePath: "C:/work/two",
  },
  {
    taskId: "task-2",
    title: "Second task",
    workspacePath: "C:/work/two",
    workspaceIdentity: "local:C:/work/two",
    unreadAt: 12,
  },
  {
    taskId: "",
    title: "Invalid task",
    workspacePath: "C:/work/invalid",
  },
];

describe("tray session projection", () => {
  test("uses the two supported locale labels and deduplicates menu ids", () => {
    expect(buildTrayMenuProjection("zh-CN", items)).toEqual({
      labels: {
        newChat: "新建对话",
        show: "显示窗口",
        quit: "退出 KeenCode",
      },
      sessions: [
        { id: "task-1", title: "First task" },
        { id: "task-2", title: "Second task" },
      ],
    });
    expect(buildTrayMenuProjection("en-US", items).labels).toEqual({
      newChat: "New conversation",
      show: "Show KeenCode",
      quit: "Quit KeenCode",
    });
  });

  test("resolves a complete workspace target and rejects stale ids", () => {
    expect(findTraySession(items, "task-2")).toMatchObject({
      taskId: "task-2",
      workspacePath: "C:/work/two",
      workspaceIdentity: "local:C:/work/two",
      unreadAt: 12,
    });
    expect(findTraySession(items, "missing")).toBeNull();
    expect(findTraySession(items, " ")).toBeNull();
  });

  test("accepts only a positive integer exit activity count", () => {
    expect(isValidExitRequestedPayload({ activeCount: 2 })).toBe(true);
    expect(isValidExitRequestedPayload({ activeCount: 0 })).toBe(false);
    expect(isValidExitRequestedPayload({ activeCount: "2" })).toBe(false);
    expect(isValidExitRequestedPayload(null)).toBe(false);
  });
});
