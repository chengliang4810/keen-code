import { describe, expect, it } from "vitest";
import { parseHostMode, resolveHostMode } from "./hostMode";

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
