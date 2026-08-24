import { describe, expect, it } from "vitest";
import {
  autoArchiveExpiredSessions,
  SESSION_PREFERENCES_KEY,
  loadSessionPreferences,
  removeSessionPreference,
  updateSessionPreference,
} from "./sessionPreferences";

/** 创建测试使用的内存 Storage。 */
function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key) {
      return values.get(key) ?? null;
    },
    key(index) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key) {
      values.delete(key);
    },
    setItem(key, value) {
      values.set(key, value);
    },
  };
}

describe("sessionPreferences", () => {
  it("永久删除后移除对应展示偏好且保留其他会话", () => {
    const storage = memoryStorage();
    updateSessionPreference("deleted", { archived: true }, storage);
    updateSessionPreference("kept", { pinned: true }, storage);

    expect(removeSessionPreference("deleted", storage)).toEqual({
      kept: { pinned: true, archived: false },
    });
  });

  it("仅存储键缺失时使用空文档", () => {
    const storage = memoryStorage();
    expect(loadSessionPreferences(storage)).toEqual({});

    for (const raw of ["", "   ", "null", "[]", "{"]) {
      storage.setItem(SESSION_PREFERENCES_KEY, raw);
      expect(() => loadSessionPreferences(storage)).toThrow();
    }
  });

  it("保存当前 Session 偏好结构", () => {
    const storage = memoryStorage();
    updateSessionPreference("session-1", { pinned: true }, storage);
    updateSessionPreference("session-1", { archived: true }, storage);

    expect(loadSessionPreferences(storage)["session-1"]).toEqual({
      pinned: true,
      archived: true,
    });
  });

  it("拒绝缺字段、错误类型和未知字段", () => {
    const storage = memoryStorage();
    for (const value of [
      { invalid: { pinned: true } },
      { invalid: { pinned: "yes", archived: false } },
      { invalid: { pinned: true, archived: false, oldField: true } },
      { invalid: { pinned: true, archived: false, goal: { active: true } } },
      {
        invalid: {
          pinned: true,
          archived: false,
          title: " 会话标题 ",
          titleSource: "manual",
        },
      },
      {
        invalid: {
          pinned: true,
          archived: false,
          title: "会话标题",
        },
      },
    ]) {
      storage.setItem(SESSION_PREFERENCES_KEY, JSON.stringify(value));
      expect(() => loadSessionPreferences(storage)).toThrow();
    }
  });

  it("拒绝未知更新字段和不完整标题", () => {
    const storage = memoryStorage();
    expect(() =>
      updateSessionPreference(
        "session-1",
        { oldField: true } as never,
        storage,
      ),
    ).toThrow("包含未知字段");
    expect(() =>
      updateSessionPreference("session-1", { title: "无来源标题" }, storage),
    ).toThrow("标题与来源必须同时存在");
    expect(storage.getItem(SESSION_PREFERENCES_KEY)).toBeNull();
  });

  it("拒绝空白或含首尾空格的 Session 标识", () => {
    const storage = memoryStorage();
    expect(() => updateSessionPreference("", { pinned: true }, storage)).toThrow(
      "Session 标识",
    );
    expect(() =>
      updateSessionPreference(" session-1 ", { pinned: true }, storage),
    ).toThrow("Session 标识");
  });

  it("将特殊 Session 标识作为普通数据键处理", () => {
    const storage = memoryStorage();
    updateSessionPreference("__proto__", { pinned: true }, storage);
    expect(loadSessionPreferences(storage)["__proto__"]).toEqual({
      pinned: true,
      archived: false,
    });
  });

  it("自动标题不覆盖手动标题", () => {
    const storage = memoryStorage();
    updateSessionPreference(
      "manual",
      { title: "我的自定义标题", titleSource: "manual" },
      storage,
    );
    updateSessionPreference(
      "manual",
      { pinned: true, title: "自动标题", titleSource: "automatic" },
      storage,
    );

    expect(loadSessionPreferences(storage).manual).toEqual({
      pinned: true,
      archived: false,
      title: "我的自定义标题",
      titleSource: "manual",
    });
  });

  it("保存并读取消息前缀标题来源", () => {
    const storage = memoryStorage();
    updateSessionPreference(
      "prefix",
      { title: "帮我修复登录页的 bug", titleSource: "message-prefix" },
      storage,
    );

    expect(loadSessionPreferences(storage).prefix).toEqual({
      pinned: false,
      archived: false,
      title: "帮我修复登录页的 bug",
      titleSource: "message-prefix",
    });
  });

  it("自动短标题正常替换消息前缀标题", () => {
    const storage = memoryStorage();
    updateSessionPreference(
      "prefix",
      { title: "帮我修复登录页的 bug", titleSource: "message-prefix" },
      storage,
    );
    updateSessionPreference(
      "prefix",
      { title: "修复登录页", titleSource: "automatic" },
      storage,
    );

    expect(loadSessionPreferences(storage).prefix).toEqual({
      pinned: false,
      archived: false,
      title: "修复登录页",
      titleSource: "automatic",
    });
  });

  it("只自动归档超过保留期的非置顶对话", () => {
    const storage = memoryStorage();
    updateSessionPreference("pinned", { pinned: true }, storage);

    const preferences = autoArchiveExpiredSessions(
      [
        { id: "old", updatedAt: "2026-08-01T00:00:00.000Z" },
        { id: "recent", updatedAt: "2026-08-09T00:00:00.000Z" },
        { id: "pinned", updatedAt: "2026-08-01T00:00:00.000Z" },
      ],
      7,
      Date.parse("2026-08-10T00:00:00.000Z"),
      storage,
    );

    expect(preferences.old.archived).toBe(true);
    expect(preferences.recent?.archived).not.toBe(true);
    expect(preferences.pinned).toMatchObject({ pinned: true, archived: false });
  });
});
