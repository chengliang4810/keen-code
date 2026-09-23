import { afterEach, describe, expect, it, vi } from "vitest";

import { randomUuid } from "./randomUuid";

const UUID_V4_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("randomUuid", () => {
  it("优先使用原生 crypto.randomUUID", () => {
    vi.stubGlobal("crypto", { randomUUID: () => "native-uuid" });
    expect(randomUuid()).toBe("native-uuid");
  });

  it("非安全上下文（无 randomUUID）时用 getRandomValues 生成合法 v4", () => {
    const nativeGetRandomValues = globalThis.crypto.getRandomValues.bind(globalThis.crypto);
    vi.stubGlobal("crypto", { getRandomValues: nativeGetRandomValues });
    expect(randomUuid()).toMatch(UUID_V4_PATTERN);
  });

  it("完全无 crypto 时回退 Math.random 并保持 v4 格式", () => {
    vi.stubGlobal("crypto", undefined);
    expect(randomUuid()).toMatch(UUID_V4_PATTERN);
  });

  it("连续生成不重复", () => {
    const seen = new Set(Array.from({ length: 200 }, () => randomUuid()));
    expect(seen.size).toBe(200);
  });
});
