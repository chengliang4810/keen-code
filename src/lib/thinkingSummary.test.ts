import { describe, expect, it } from "vitest";
import { extractThinkingSummary } from "./thinkingSummary";

describe("extractThinkingSummary", () => {
  it("prefers bold", () => {
    expect(
      extractThinkingSummary("前言\n\n**定位主项目目录**\n\n细节…"),
    ).toBe("定位主项目目录");
  });

  it("falls back to heading", () => {
    expect(extractThinkingSummary("# 搜索相关项目\n\nmore")).toBe(
      "搜索相关项目",
    );
  });

  it("uses first plain line", () => {
    expect(
      extractThinkingSummary("我来在工作区里搜索奇妙森林相关内容。\n\n下一步"),
    ).toBe("我来在工作区里搜索奇妙森林相关内容。");
  });

  it("clips long lines", () => {
    const long = "字".repeat(80);
    const s = extractThinkingSummary(long);
    expect(s).toBeTruthy();
    expect(s!.endsWith("…")).toBe(true);
    expect(s!.length).toBeLessThanOrEqual(48);
  });

  it("returns null for empty", () => {
    expect(extractThinkingSummary("")).toBeNull();
    expect(extractThinkingSummary("   \n  ")).toBeNull();
  });
});
