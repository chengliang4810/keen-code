import { describe, expect, it, vi } from "vitest";
import {
  parseCssColor,
  readThemeBackgroundColor,
} from "./nativeTheme";

describe("native theme surfaces", () => {
  it("解析 Tauri 支持的 RGBA 颜色格式", () => {
    expect(parseCssColor("#f8f8f8")).toEqual([248, 248, 248, 255]);
    expect(parseCssColor("#1234")).toEqual([17, 34, 51, 68]);
    expect(parseCssColor("rgb(22, 22, 22)")).toEqual([22, 22, 22, 255]);
    expect(parseCssColor("rgb(10 20 30 / 50%)")).toEqual([10, 20, 30, 128]);
  });

  it("拒绝无法传给原生表面的 CSS 颜色", () => {
    expect(parseCssColor("color-mix(in srgb, black 50%, white)")).toBeNull();
    expect(parseCssColor("#gggggg")).toBeNull();
    expect(parseCssColor("rgb(1, 2)")).toBeNull();
  });

  it("从当前 --bg-app 读取主题底色", () => {
    const getComputedStyle = vi.fn(() => ({
      getPropertyValue: () => "#161616",
    }));
    vi.stubGlobal("getComputedStyle", getComputedStyle);
    expect(readThemeBackgroundColor({} as HTMLElement)).toEqual([22, 22, 22, 255]);
    vi.unstubAllGlobals();
  });
});
