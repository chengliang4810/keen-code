import { describe, expect, it } from "vitest";
import {
  parseHostMode,
  readUrlAuthToken,
  resolveHostMode,
  stripUrlAuthToken,
} from "./hostMode";

describe("host mode resolution", () => {
  it("只接受固定宿主模式值", () => {
    expect(parseHostMode("desktop")).toBe("desktop");
    expect(parseHostMode("web")).toBe("web");
    expect(parseHostMode("mobile-remote")).toBe("mobile-remote");
    expect(parseHostMode("mobile")).toBeNull();
    expect(parseHostMode(null)).toBeNull();
  });

  it("从 query 解析宿主模式，缺失时保持 Desktop 默认值", () => {
    expect(resolveHostMode("?hostMode=web")).toBe("web");
    expect(resolveHostMode("?host=mobile-remote")).toBe("mobile-remote");
    expect(resolveHostMode("?hostMode=unknown")).toBe("desktop");
    expect(resolveHostMode("")).toBe("desktop");
  });
});

describe("pairing URL token", () => {
  it("只把 token 参数当作凭据，其余 query 忽略", () => {
    expect(readUrlAuthToken("?host=mobile-remote&token=secret")).toBe("secret");
    expect(readUrlAuthToken("?host=mobile-remote")).toBeNull();
    expect(readUrlAuthToken("?token=%20")).toBeNull();
    expect(readUrlAuthToken("")).toBeNull();
  });

  it("清理 token 参数时保留其余 query", () => {
    expect(stripUrlAuthToken("?host=mobile-remote&token=secret")).toBe("?host=mobile-remote");
    expect(stripUrlAuthToken("?token=secret")).toBe("");
    expect(stripUrlAuthToken("")).toBe("");
  });
});
