import { describe, expect, it } from "vitest";
import {
  DEFAULT_LAYOUT,
  loadInitialLayout,
  loadLayout,
  parseLayout,
  saveLayout,
  shouldCollapsePane,
  clampAsideWidth,
  clampSidebarWidth,
  getSidebarWidthMax,
  ASIDE_WIDTH_MIN,
  ASIDE_WIDTH_MAX,
  MAIN_WIDTH_MIN,
  SIDEBAR_WIDTH_MIN,
  LAYOUT_STORAGE_KEY,
} from "./layout";

describe("layout prefs", () => {
  it("defaults right pane collapsed", () => {
    expect(DEFAULT_LAYOUT.asideCollapsed).toBe(true);
    expect(DEFAULT_LAYOUT.sidebarWidth).toBe(264);
    expect(SIDEBAR_WIDTH_MIN).toBe(264);
  });

  it("手机首次进入默认收起覆盖式侧栏，桌面保留持久化状态", () => {
    const storage = { getItem: () => null };
    expect(loadInitialLayout(storage, 390).sidebarCollapsed).toBe(true);
    expect(loadInitialLayout(storage, 1440).sidebarCollapsed).toBe(false);
  });

  it("持久化布局损坏或不再符合当前尺寸约束时回退默认值", () => {
    const invalidStorage = {
      getItem: () => JSON.stringify({
        sidebarWidth: 240,
        asideWidth: 360,
        sidebarCollapsed: false,
      }),
    };

    expect(loadInitialLayout(invalidStorage, 1440)).toEqual(DEFAULT_LAYOUT);
    expect(loadInitialLayout({ getItem: () => "{" }, 390)).toEqual({
      ...DEFAULT_LAYOUT,
      sidebarCollapsed: true,
    });
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
        sidebarWidth: 264,
        asideWidth: 360,
      }),
    ).toThrow("布局配置字段不完整或包含未知字段");
    expect(() =>
      parseLayout({
        sidebarWidth: 264,
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
      sidebarWidth: 264,
      asideWidth: 360,
      sidebarCollapsed: false,
    };
    expect(() => parseLayout({ ...valid, sidebarWidth: 0 })).toThrow(
      "侧栏宽度无效",
    );
    expect(() => parseLayout({ ...valid, sidebarWidth: 120 })).toThrow(
      "侧栏宽度无效",
    );
    expect(parseLayout({ ...valid, sidebarWidth: 800 }).sidebarWidth).toBe(800);
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
    expect(clampSidebarWidth(9999)).toBe(9999);
    expect(clampSidebarWidth(9999, 720)).toBe(720);
    expect(clampSidebarWidth(300)).toBe(300);
  });

  it("uses half of the available workspace for the sidebar maximum", () => {
    expect(getSidebarWidthMax(1440)).toBe(720);
    expect(getSidebarWidthMax(640)).toBe(320);
    expect(getSidebarWidthMax(400)).toBe(SIDEBAR_WIDTH_MIN);
    expect(getSidebarWidthMax()).toBe(Number.POSITIVE_INFINITY);
  });

  it("bounds a persisted width for the current viewport without rewriting storage", () => {
    const raw = JSON.stringify({
      sidebarWidth: 800,
      asideWidth: 360,
      sidebarCollapsed: false,
    });
    const storage = { getItem: () => raw };
    expect(loadInitialLayout(storage, 1000).sidebarWidth).toBe(500);
    expect(raw).toContain('"sidebarWidth":800');
  });

  it("collapses a pane only after dragging beyond its minimum width", () => {
    expect(shouldCollapsePane(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MIN)).toBe(false);
    expect(
      shouldCollapsePane(SIDEBAR_WIDTH_MIN - 39, SIDEBAR_WIDTH_MIN),
    ).toBe(false);
    expect(
      shouldCollapsePane(SIDEBAR_WIDTH_MIN - 40, SIDEBAR_WIDTH_MIN),
    ).toBe(true);
  });
});
