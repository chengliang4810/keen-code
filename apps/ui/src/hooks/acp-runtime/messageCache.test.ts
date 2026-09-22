import { describe, expect, it } from "vitest";
import type { ChatMessage } from "@/lib/session";
import {
  pruneInactiveSessionMessageCache,
  pruneUnprotectedSessionMessageCache,
} from "./messageCache";

function messages(id: string): ChatMessage[] {
  return [{ id, role: "assistant", content: id }];
}

describe("pruneInactiveSessionMessageCache", () => {
  it("只淘汰最早的非活动缓存并保留当前、运行和待回答会话", () => {
    const cache = new Map<string, ChatMessage[]>([
      ["old-1", messages("old-1")],
      ["running", messages("running")],
      ["old-2", messages("old-2")],
      ["asking", messages("asking")],
      ["current", messages("current")],
      ["recent", messages("recent")],
    ]);

    expect(pruneInactiveSessionMessageCache(
      cache,
      new Set(["running", "asking", "current"]),
      1,
    )).toEqual(["old-1", "old-2"]);
    expect([...cache.keys()]).toEqual(["running", "asking", "current", "recent"]);
  });

  it("草稿不计入非活动上限", () => {
    const cache = new Map<string, ChatMessage[]>([
      ["__draft__", messages("draft")],
      ["old", messages("old")],
    ]);
    expect(pruneInactiveSessionMessageCache(cache, new Set(), 0)).toEqual(["old"]);
    expect(cache.has("__draft__")).toBe(true);
  });

  it("统一保护活动、排队、运行中和待回答会话", () => {
    const protectedIds = ["active", "queued", "running", "asking"];
    const cache = new Map<string, ChatMessage[]>([
      ...Array.from({ length: 7 }, (_, index) => {
        const id = `old-${index}`;
        return [id, messages(id)] as const;
      }),
      ...protectedIds.map((id) => [id, messages(id)] as const),
    ]);

    expect(pruneUnprotectedSessionMessageCache(
      cache,
      ["running"],
      ["queued"],
      "active",
      ["asking"],
    )).toEqual(["old-0"]);
    expect(protectedIds.every((id) => cache.has(id))).toBe(true);
  });
});
