import { describe, expect, it } from "vitest";
import {
  DEFAULT_SESSION_SORT_MODE,
  loadSessionOrderSafe,
  loadSessionSortModeSafe,
  loadSessionSortMode,
  moveId,
  orderedByIds,
  saveSessionSortMode,
  sortSessionRows,
} from "./sidebarOrder";

describe("sidebar order", () => {
  it("keeps new items first and moves an existing item before its target", () => {
    const items = [{ id: "new" }, { id: "a" }, { id: "b" }];
    expect(orderedByIds(items, ["b", "a"]).map(({ id }) => id)).toEqual(["new", "b", "a"]);
    expect(moveId(["new", "b", "a"], "a", "b")).toEqual(["new", "a", "b"]);
    expect(moveId(["a", "b", "c"], "a", "c", true)).toEqual(["b", "c", "a"]);
  });
});

const row = (
  id: string,
  updatedAt: string,
  lastUserMessageAt: string | null,
) => ({ id, updatedAt, lastUserMessageAt });

describe("sidebar sort mode", () => {
  it("默认按最后用户消息时间，后台更新不会让会话上浮", () => {
    const rows = [
      // 运行中的会话更新时间最新，但用户最后发消息的时间更早。
      row("running", "2026-09-10T00:00:00Z", "2026-09-01T00:00:00Z"),
      row("chatty", "2026-09-02T00:00:00Z", "2026-09-09T00:00:00Z"),
    ];
    expect(sortSessionRows(rows, [], "lastUserMessage").map(({ id }) => id)).toEqual([
      "chatty",
      "running",
    ]);
    expect(sortSessionRows(rows, [], "updatedAt").map(({ id }) => id)).toEqual([
      "running",
      "chatty",
    ]);
  });

  it("从未发送消息的会话回退到更新时间，不会沉到列表末尾", () => {
    const rows = [
      row("fresh", "2026-09-20T00:00:00Z", null),
      row("old", "2026-09-01T00:00:00Z", "2026-09-05T00:00:00Z"),
    ];
    expect(sortSessionRows(rows, [], "lastUserMessage").map(({ id }) => id)).toEqual([
      "fresh",
      "old",
    ]);
  });

  it("拖拽过的会话固定在前面，其余仍按所选时间排序", () => {
    const rows = [
      row("pinned-by-drag", "2026-09-01T00:00:00Z", "2026-09-01T00:00:00Z"),
      row("b", "2026-09-09T00:00:00Z", "2026-09-09T00:00:00Z"),
      row("a", "2026-09-05T00:00:00Z", "2026-09-05T00:00:00Z"),
    ];
    expect(
      sortSessionRows(rows, ["pinned-by-drag"], "lastUserMessage").map(({ id }) => id),
    ).toEqual(["pinned-by-drag", "b", "a"]);
  });

  it("同值时间按标识稳定排序", () => {
    const rows = [
      row("b", "2026-09-05T00:00:00Z", "2026-09-05T00:00:00Z"),
      row("a", "2026-09-05T00:00:00Z", "2026-09-05T00:00:00Z"),
    ];
    expect(sortSessionRows(rows, [], "lastUserMessage").map(({ id }) => id)).toEqual([
      "a",
      "b",
    ]);
  });

  it("排序方式只接受两个已知值并跨会话保留", () => {
    const values = new Map<string, string>();
    const storage = {
      get length() {
        return values.size;
      },
      clear: () => values.clear(),
      getItem: (key: string) => values.get(key) ?? null,
      key: (index: number) => [...values.keys()][index] ?? null,
      removeItem: (key: string) => {
        values.delete(key);
      },
      setItem: (key: string, value: string) => {
        values.set(key, value);
      },
    } satisfies Storage;
    expect(loadSessionSortMode(storage)).toBe("lastUserMessage");
    saveSessionSortMode("updatedAt", storage);
    expect(loadSessionSortMode(storage)).toBe("updatedAt");
    values.set("keencode.sidebar-session-sort-mode", JSON.stringify("bogus"));
    expect(() => loadSessionSortMode(storage)).toThrow();
  });
});

describe("损坏数据的降级加载", () => {
  it("loadSessionOrderSafe 删除坏 key 并回退为空", () => {
    const storage = new Map<string, string>([
      ["keencode.sidebar-session-order", "{ broken"],
    ]);
    const stub = {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => void storage.set(key, value),
      removeItem: (key: string) => void storage.delete(key),
    } as unknown as Storage;
    expect(loadSessionOrderSafe(stub)).toEqual([]);
    expect(storage.has("keencode.sidebar-session-order")).toBe(false);
  });

  it("loadSessionSortModeSafe 删除坏 key 并回退默认值", () => {
    const storage = new Map<string, string>([
      ["keencode.sidebar-session-sort-mode", "42"],
    ]);
    const stub = {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => void storage.set(key, value),
      removeItem: (key: string) => void storage.delete(key),
    } as unknown as Storage;
    expect(loadSessionSortModeSafe(stub)).toBe(DEFAULT_SESSION_SORT_MODE);
    expect(storage.has("keencode.sidebar-session-sort-mode")).toBe(false);
  });
});
