import { createElement, type EffectCallback } from "react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useUnreadTerminalResults } from "./useUnreadTerminalResults";

/** 仅替换外部订阅与 effect 调度；Hook 自身的去重逻辑保持真实。 */
const ports = vi.hoisted(() => ({
  effects: [] as EffectCallback[],
  isTauri: true,
  setBadge: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => {
    ports.effects.push(effect);
  },
}));

vi.mock("@/lib/api", () => ({
  isTauri: () => ports.isTauri,
  traySetBadge: (count: number) => ports.setBadge(count),
}));

/** node 测试环境没有 localStorage，用内存实现承载未读结果持久化。 */
function stubLocalStorage(): void {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
    clear: () => values.clear(),
  } satisfies Partial<Storage>);
}

/** 渲染一次并返回本次渲染收集到的最后一个 effect。 */
function render(appBooting: boolean): EffectCallback {
  function Harness() {
    useUnreadTerminalResults(appBooting);
    return null;
  }
  renderToString(createElement(Harness));
  return ports.effects.at(-1)!;
}

describe("Dock 未读角标投影", () => {
  beforeEach(() => {
    stubLocalStorage();
    ports.effects.length = 0;
    ports.isTauri = true;
    ports.setBadge.mockClear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("启动页结束前不推送，避免品牌启动阶段出现角标", () => {
    render(true)();
    expect(ports.setBadge).not.toHaveBeenCalled();
  });

  it("没有未读时推送 0，由后端移除角标", () => {
    render(false)();
    expect(ports.setBadge).toHaveBeenCalledWith(0);
  });

  it("浏览器预览下静默跳过后端命令", () => {
    ports.isTauri = false;
    render(false)();
    expect(ports.setBadge).not.toHaveBeenCalled();
  });
});
