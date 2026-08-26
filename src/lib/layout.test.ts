import { describe, expect, it } from "vitest";
import {
  DEFAULT_LAYOUT,
  loadLayout,
  parseLayout,
  saveLayout,
  clampAsideWidth,
  clampSidebarWidth,
  ASIDE_WIDTH_MIN,
  ASIDE_WIDTH_MAX,
  MAIN_WIDTH_MIN,
  SIDEBAR_WIDTH_MIN,
  SIDEBAR_WIDTH_MAX,
  LAYOUT_STORAGE_KEY,
} from "./layout";

describe("layout prefs", () => {
  it("defaults right pane collapsed", () => {
    expect(DEFAULT_LAYOUT.asideCollapsed).toBe(true);
  });

  it("round-trips widths; right pane always starts collapsed", () => {
    const data: Record<string, string> = {};
    const storage = {
      getItem: (k: string) => data[k] ?? null,
      setItem: (k: string, v: string) => {
        data[k] = v;
      },
    };
    saveLayout(storage, {
      sidebarWidth: 280,
      asideWidth: 320,
      asideCollapsed: false,
      sidebarCollapsed: true,
    });
    expect(data[LAYOUT_STORAGE_KEY]).toBeTruthy();
    const loaded = loadLayout(storage);
    // Open state is not restored across app launches.
    expect(loaded.asideCollapsed).toBe(true);
    expect(loaded.sidebarWidth).toBe(280);
    expect(loaded.asideWidth).toBe(320);
    expect(loaded.sidebarCollapsed).toBe(true);
  });

  it("严格拒绝缺失字段与未知字段", () => {
    expect(() => parseLayout(null)).toThrow("布局配置必须是对象");
    expect(() => parseLayout([])).toThrow("布局配置必须是对象");
    expect(() =>
      parseLayout({
        sidebarWidth: 260,
        asideWidth: 360,
      }),
    ).toThrow("布局配置字段不完整或包含未知字段");
    expect(() =>
      parseLayout({
        sidebarWidth: 260,
        asideWidth: 360,
        sidebarCollapsed: false,
        asideCollapsed: true,
      }),
    ).toThrow("布局配置字段不完整或包含未知字段");
  });

  it("仅存储键缺失时使用首次启动默认值", () => {
    const missingStorage = { getItem: () => null };
    expect(loadLayout(missingStorage)).toEqual(DEFAULT_LAYOUT);

    for (const raw of ["", "   ", "null", "{}", "{"]) {
      expect(() => loadLayout({ getItem: () => raw })).toThrow();
    }
  });

  it("拒绝无效宽度和折叠状态", () => {
    const valid = {
      sidebarWidth: 260,
      asideWidth: 360,
      sidebarCollapsed: false,
    };
    expect(() => parseLayout({ ...valid, sidebarWidth: 0 })).toThrow(
      "侧栏宽度无效",
    );
    expect(() => parseLayout({ ...valid, sidebarWidth: 120 })).toThrow(
      "侧栏宽度无效",
    );
    expect(() => parseLayout({ ...valid, sidebarWidth: 800 })).toThrow(
      "侧栏宽度无效",
    );
    expect(() => parseLayout({ ...valid, asideWidth: 100 })).toThrow(
      "资源栏宽度无效",
    );
    expect(() =>
      parseLayout({ ...valid, sidebarCollapsed: "false" }),
    ).toThrow("侧栏折叠状态无效");
  });

  it("写入前校验当前结构", () => {
    const data: Record<string, string> = {};
    const storage = {
      setItem: (key: string, value: string) => {
        data[key] = value;
      },
    };
    expect(() =>
      saveLayout(storage, {
        ...DEFAULT_LAYOUT,
        asideWidth: Number.NaN,
      }),
    ).toThrow("资源栏宽度无效");
    expect(data).toEqual({});
  });

  it("clamps aside width", () => {
    expect(clampAsideWidth(100)).toBe(ASIDE_WIDTH_MIN);
    expect(clampAsideWidth(9999)).toBe(ASIDE_WIDTH_MAX);
    expect(clampAsideWidth(400)).toBe(400);
    expect(clampAsideWidth(1600, 1200)).toBe(1200 - MAIN_WIDTH_MIN);
  });

  it("clamps sidebar width", () => {
    expect(clampSidebarWidth(100)).toBe(SIDEBAR_WIDTH_MIN);
    expect(clampSidebarWidth(9999)).toBe(SIDEBAR_WIDTH_MAX);
    expect(clampSidebarWidth(300)).toBe(300);
  });
});
