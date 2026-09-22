import { describe, expect, it } from "vitest";
import { buildGoalDraft } from "./goalDraft";

describe("Goal 标题与正文", () => {
  it("长中文和 emoji 正文保持完整，标题满足字节上限且不切断字符", () => {
    const objective = "修复🚀".repeat(100);
    const draft = buildGoalDraft(objective);
    expect(draft.objective).toBe(objective);
    expect(new TextEncoder().encode(draft.title).length).toBeLessThanOrEqual(512);
    expect(draft.title.endsWith("…")).toBe(true);
    expect(draft.title).not.toMatch(/[\uD800-\uDBFF]…$/);
    expect(objective.startsWith(draft.title.slice(0, -1))).toBe(true);
  });
  it("首行作标题，完整多行需求作正文", () => {
    expect(buildGoalDraft("  修复登录\n保留中文边界\n验证重启  ")).toEqual({
      title: "修复登录", objective: "修复登录\n保留中文边界\n验证重启",
    });
  });
  it("按 UTF-8 字节校验正文边界", () => {
    expect(buildGoalDraft("a".repeat(65536)).objective.length).toBe(65536);
    expect(() => buildGoalDraft("a".repeat(65537))).toThrow("65536");
    expect(() => buildGoalDraft("中".repeat(21846))).toThrow("65536");
    expect(() => buildGoalDraft(" \n ")).toThrow("不能为空");
    expect(buildGoalDraft("a".repeat(512)).title).toBe("a".repeat(512));
  });
});
