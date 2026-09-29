import { describe, expect, it } from "vitest";
import {
  DEFAULT_EFFORT_COLOR,
  EFFORT_COLORS,
  EFFORT_COLOR_SWATCHES,
  loadEffortColor,
} from "./effortColor";

describe("effortColor", () => {
  it("预设列表与色板一一对应，默认为紫色", () => {
    expect(DEFAULT_EFFORT_COLOR).toBe("purple");
    expect(EFFORT_COLORS).toContain(DEFAULT_EFFORT_COLOR);
    for (const preset of EFFORT_COLORS) {
      expect(EFFORT_COLOR_SWATCHES[preset]).toMatch(/^#[0-9a-f]{6}$/i);
    }
  });

  it("读取持久化预设，无效值回落默认", () => {
    const store = (value: string | null) => ({ getItem: () => value });
    expect(loadEffortColor(store("terracotta"))).toBe("terracotta");
    expect(loadEffortColor(store("silver"))).toBe("silver");
    expect(loadEffortColor(store("nope"))).toBe(DEFAULT_EFFORT_COLOR);
    expect(loadEffortColor(store(null))).toBe(DEFAULT_EFFORT_COLOR);
  });
});
