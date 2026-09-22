import { describe, expect, it } from "vitest";
import {
  resolveSessionScrollTop,
  saveSessionScrollMemory,
  readSessionScrollMemory,
  SESSION_SCROLL_MEMORY_MAX_ENTRIES,
} from "./useSessionScrollMemory";

describe("useSessionScrollMemory", () => {
  it("按会话 key 保存滚动度量，并将位置钳制到当前内容范围", () => {
    const key = `scroll-memory-state-${Date.now()}-${Math.random()}`;
    saveSessionScrollMemory(key, {
      scrollTop: 480,
      scrollHeight: 1_200,
      clientHeight: 400,
      wasPinnedToBottom: false,
      updatedAt: Date.now(),
    });

    const state = readSessionScrollMemory(key);
    expect(state?.scrollTop).toBe(480);
    expect(state?.wasPinnedToBottom).toBe(false);
    expect(resolveSessionScrollTop(state!, { scrollHeight: 700, clientHeight: 400 })).toBe(300);
    expect(resolveSessionScrollTop(state!, { scrollHeight: 2_000, clientHeight: 400 })).toBe(480);
  });

  it("使用 LRU 上限，且读取会刷新条目的存活顺序", () => {
    const prefix = `scroll-memory-lru-${Date.now()}-${Math.random()}`;
    for (let index = 0; index < SESSION_SCROLL_MEMORY_MAX_ENTRIES; index += 1) {
      saveSessionScrollMemory(`${prefix}-${index}`, {
        scrollTop: index,
        scrollHeight: 2_000,
        clientHeight: 500,
        wasPinnedToBottom: false,
        updatedAt: index,
      });
    }

    expect(readSessionScrollMemory(`${prefix}-0`)).not.toBeNull();
    saveSessionScrollMemory(`${prefix}-${SESSION_SCROLL_MEMORY_MAX_ENTRIES}`, {
      scrollTop: 200,
      scrollHeight: 2_000,
      clientHeight: 500,
      wasPinnedToBottom: false,
      updatedAt: 200,
    });

    expect(readSessionScrollMemory(`${prefix}-0`)).not.toBeNull();
    expect(readSessionScrollMemory(`${prefix}-1`)).toBeNull();
  });

  it("拒绝空 key，并把异常数值归一化为可用快照", () => {
    saveSessionScrollMemory("", {
      scrollTop: 1,
      scrollHeight: 2,
      clientHeight: 3,
      wasPinnedToBottom: true,
      updatedAt: 4,
    });
    const key = `scroll-memory-normalize-${Date.now()}-${Math.random()}`;
    saveSessionScrollMemory(key, {
      scrollTop: Number.NaN,
      scrollHeight: -1,
      clientHeight: Number.POSITIVE_INFINITY,
      wasPinnedToBottom: true,
      updatedAt: Number.NaN,
    });

    expect(readSessionScrollMemory("")).toBeNull();
    expect(readSessionScrollMemory(key)).toMatchObject({
      scrollTop: 0,
      scrollHeight: 0,
      clientHeight: 0,
      wasPinnedToBottom: true,
    });
  });
});
