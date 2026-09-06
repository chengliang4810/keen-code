import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import {
  createMemoryFileAccessState,
  refreshMemoryFile,
  writeMemoryFile,
} from "./memoryFileAccess";

/** 用真实 Promise 控制读写回执顺序，不依赖墙钟或网络。 */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe("记忆文件按需刷新", () => {
  it("再次进入时读取后台生成的新正文", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const read = vi.fn().mockResolvedValueOnce("").mockResolvedValueOnce("新生成记忆");
    await refreshMemoryFile(state, read, apply);
    await refreshMemoryFile(state, read, apply);
    expect(read).toHaveBeenCalledTimes(2);
    expect(apply.mock.calls).toEqual([[""], ["新生成记忆"]]);
  });

  it("较旧读取后返回也不能覆盖新读取", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const old = deferred<string>();
    const first = refreshMemoryFile(state, () => old.promise, apply);
    await Promise.resolve();
    await refreshMemoryFile(state, async () => "最新", apply);
    old.resolve("旧正文");
    await first;
    expect(apply.mock.calls).toEqual([["最新"]]);
  });

  it.each(["保存正文", ""])("旧读取不覆盖保存或重置结果 %j", async (saved) => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const old = deferred<string>();
    const reading = refreshMemoryFile(state, () => old.promise, apply);
    await Promise.resolve();
    await writeMemoryFile(state, async () => saved, apply);
    old.resolve("旧正文");
    await reading;
    expect(apply.mock.calls).toEqual([[saved]]);
  });

  it("写入期间的刷新等待写入，不读取落盘前的旧值", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const saved = deferred<string>();
    const writing = writeMemoryFile(state, () => saved.promise, apply);
    const read = vi.fn().mockResolvedValue("不应读取的旧值");
    const refreshing = refreshMemoryFile(state, read, apply);
    await Promise.resolve();
    expect(read).not.toHaveBeenCalled();
    saved.resolve("新正文");
    await Promise.all([writing, refreshing]);
    expect(read).not.toHaveBeenCalled();
    expect(apply.mock.calls).toEqual([["新正文"]]);
  });

  it("读取失败不把已确认正文清空", async () => {
    const apply = vi.fn();
    await expect(refreshMemoryFile(createMemoryFileAccessState(), async () => {
      throw new Error("读失败");
    }, apply)).rejects.toThrow("读失败");
    expect(apply).not.toHaveBeenCalled();
  });

  it("保存失败保留错误语义，后续重置仍可串行成功", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const first = deferred<string>();
    const writing = writeMemoryFile(state, () => first.promise, apply);
    const rejection = expect(writing).rejects.toThrow("写失败");
    const reset = vi.fn().mockResolvedValue("");
    const resetting = writeMemoryFile(state, reset, apply);
    await Promise.resolve();
    expect(reset).not.toHaveBeenCalled();
    first.reject(new Error("写失败"));
    await rejection;
    await resetting;
    expect(reset).toHaveBeenCalledTimes(1);
    expect(apply.mock.calls).toEqual([[""]]);
  });

  it("保存与重置依调用顺序落盘，迟到保存不会恢复被重置的正文", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const saved = deferred<string>();
    const writing = writeMemoryFile(state, () => saved.promise, apply);
    const reset = vi.fn().mockResolvedValue("");
    const resetting = writeMemoryFile(state, reset, apply);
    await Promise.resolve();
    expect(reset).not.toHaveBeenCalled();
    saved.resolve("保存完成");
    await Promise.all([writing, resetting]);
    expect(apply.mock.calls).toEqual([["保存完成"], [""]]);
  });

  it("卸载失效后的读取不能应用", async () => {
    const state = createMemoryFileAccessState();
    const apply = vi.fn();
    const read = deferred<string>();
    const refreshing = refreshMemoryFile(state, () => read.promise, apply);
    await Promise.resolve();
    ++state.revision;
    read.resolve("迟到正文");
    await refreshing;
    expect(apply).not.toHaveBeenCalled();
  });

  it("路由只为个性化触发稳定刷新，面板仍保护未保存草稿", () => {
    // 源码装配检查不冒充 React/原生交互测试；运行行为由定向桌面验收补齐。
    const route = readFileSync(new URL("../features/app/SettingsRoute.tsx", import.meta.url), "utf8");
    const panel = readFileSync(new URL("../components/PersonalizationSettingsPanel.tsx", import.meta.url), "utf8");
    const hook = readFileSync(new URL("../hooks/useAppSettings.ts", import.meta.url), "utf8");
    expect(route).toContain('if (section === "personalization") void onMemoryFileRefresh();');
    expect(route).toContain("[section, onMemoryFileRefresh]");
    expect(panel).toContain("current === previousValue ? memoryFile : current");
    expect(hook).toContain("const onMemoryFileRefresh = useCallback(async () => {");
    expect(route).not.toContain("setInterval");
    expect(hook).not.toContain("setInterval");
  });
});
