import { describe, expect, it, vi } from "vitest";
import type { AppSettings } from "./api";
import {
  persistLatestAppSetting,
  type AppSettingPersistenceMap,
} from "./appSettingPersistence";

/** 可由测试按任意顺序完成的 Promise。 */
interface Deferred<Value> {
  /** 等待测试显式完成的 Promise。 */
  promise: Promise<Value>;
  /** 以成功值完成 Promise。 */
  resolve: (value: Value) => void;
  /** 以失败原因完成 Promise。 */
  reject: (cause: unknown) => void;
}

/** 创建可由测试控制完成顺序的 Promise。 */
function deferred<Value>(): Deferred<Value> {
  let resolve!: (value: Value) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => {
    resolve = accept;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** 创建完整的当前 AppSettings 测试值。 */
function settings(overrides: Partial<AppSettings> = {}): AppSettings {
  return {
    interfaceLanguage: "zh",
    appUpdateDownloadSource: "auto",
    chromeHardwareAcceleration: true,
    sidebarCollapsedProjectIds: [],
    projectDirectory: "",
    taskNotifications: true,
    notificationSound: true,
    keepComputerAwake: true,
    backgroundAgentLimit: 10,
    terminalFontFamily: "monospace",
    terminalShell: "auto",
    localMemories: true,
    autoArchiveConversations: false,
    archiveRetentionDays: 7,
    ...overrides,
  };
}

describe("persistLatestAppSetting", () => {
  it("同字段写入严格按调用顺序落盘，旧失败不会回滚新状态", async () => {
    const first = deferred<AppSettings>();
    const second = deferred<AppSettings>();
    const pending = [first, second];
    const applied: boolean[] = [];
    const onError = vi.fn();
    const states: AppSettingPersistenceMap = new Map();
    const persist = vi.fn(() => pending.shift()!.promise);

    const oldRequest = persistLatestAppSetting(
      {
        key: "taskNotifications",
        value: false,
        optimistic: false,
        previous: true,
        apply: (value) => applied.push(value),
      },
      { states, persist, onError },
    );
    const newRequest = persistLatestAppSetting(
      {
        key: "taskNotifications",
        value: true,
        optimistic: true,
        previous: false,
        apply: (value) => applied.push(value),
      },
      { states, persist, onError },
    );

    await Promise.resolve();
    expect(persist).toHaveBeenCalledTimes(1);
    first.reject(new Error("旧请求失败"));
    await oldRequest;
    expect(persist).toHaveBeenCalledTimes(2);
    second.resolve(settings({ taskNotifications: true }));
    await newRequest;

    expect(applied).toEqual([false, true]);
    expect(onError).not.toHaveBeenCalled();
  });

  it("旧请求先成功时仅更新确认值，最终应用最新的后端规范化结果", async () => {
    const first = deferred<AppSettings>();
    const second = deferred<AppSettings>();
    const pending = [first, second];
    const applied: number[] = [];
    const states: AppSettingPersistenceMap = new Map();
    const persist = vi.fn(() => pending.shift()!.promise);
    const normalizeSaved = (saved: AppSettings) => saved.backgroundAgentLimit;

    const oldRequest = persistLatestAppSetting(
      {
        key: "backgroundAgentLimit",
        value: 40,
        optimistic: 40,
        previous: 10,
        apply: (value) => applied.push(value),
        normalizeSaved,
      },
      { states, persist, onError: vi.fn() },
    );
    const newRequest = persistLatestAppSetting(
      {
        key: "backgroundAgentLimit",
        value: 6,
        optimistic: 6,
        previous: 40,
        apply: (value) => applied.push(value),
        normalizeSaved,
      },
      { states, persist, onError: vi.fn() },
    );

    first.resolve(settings({ backgroundAgentLimit: 20 }));
    await oldRequest;
    second.resolve(settings({ backgroundAgentLimit: 5 }));
    await newRequest;

    expect(applied).toEqual([40, 6, 5]);
  });

  it("最新请求失败时回滚并报告一次错误", async () => {
    const request = deferred<AppSettings>();
    const applied: string[] = [];
    const onError = vi.fn();
    const operation = persistLatestAppSetting(
      {
        key: "terminalFontFamily",
        value: "JetBrains Mono",
        optimistic: "JetBrains Mono",
        previous: "monospace",
        apply: (value) => applied.push(value),
      },
      {
        states: new Map(),
        persist: () => request.promise,
        onError,
      },
    );

    request.reject(new Error("保存失败"));
    await operation;

    expect(applied).toEqual(["JetBrains Mono", "monospace"]);
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("连续请求都失败时回滚到后端最后确认值", async () => {
    const first = deferred<AppSettings>();
    const second = deferred<AppSettings>();
    const pending = [first, second];
    const applied: boolean[] = [];
    const onError = vi.fn();
    const states: AppSettingPersistenceMap = new Map();
    const persist = vi.fn(() => pending.shift()!.promise);

    const oldRequest = persistLatestAppSetting(
      {
        key: "taskNotifications",
        value: false,
        optimistic: false,
        previous: true,
        apply: (value) => applied.push(value),
      },
      { states, persist, onError },
    );
    const newRequest = persistLatestAppSetting(
      {
        key: "taskNotifications",
        value: true,
        optimistic: true,
        previous: false,
        apply: (value) => applied.push(value),
      },
      { states, persist, onError },
    );

    first.reject(new Error("第一次保存失败"));
    await oldRequest;
    second.reject(new Error("第二次保存失败"));
    await newRequest;

    expect(applied).toEqual([false, true, true]);
    expect(onError).toHaveBeenCalledTimes(1);
    expect(states.get("taskNotifications")?.confirmed).toBe(true);
  });

  it("不同字段各自维护独立修订号", async () => {
    const localeRequest = deferred<AppSettings>();
    const limitRequest = deferred<AppSettings>();
    const pending = [localeRequest, limitRequest];
    const states: AppSettingPersistenceMap = new Map();
    const persist = vi.fn(() => pending.shift()!.promise);
    const localeApplied: string[] = [];
    const limitApplied: number[] = [];

    const localeOperation = persistLatestAppSetting(
      {
        key: "interfaceLanguage",
        value: "en",
        optimistic: "en",
        previous: "zh",
        apply: (value) => localeApplied.push(value),
      },
      { states, persist, onError: vi.fn() },
    );
    const limitOperation = persistLatestAppSetting(
      {
        key: "backgroundAgentLimit",
        value: 99,
        optimistic: 99,
        previous: 10,
        apply: (value) => limitApplied.push(value),
        normalizeSaved: (saved) => saved.backgroundAgentLimit,
      },
      { states, persist, onError: vi.fn() },
    );

    limitRequest.resolve(settings({ backgroundAgentLimit: 12 }));
    localeRequest.resolve(settings({ interfaceLanguage: "en" }));
    await Promise.all([localeOperation, limitOperation]);

    expect(states.get("interfaceLanguage")?.revision).toBe(1);
    expect(states.get("backgroundAgentLimit")?.revision).toBe(1);
    expect(localeApplied).toEqual(["en"]);
    expect(limitApplied).toEqual([99, 12]);
  });
});
