import type { Ref } from "react";
import { describe, expect, it, vi } from "vitest";
import { assignRef, mergeRefs } from "./reactRefs";

type RefValue = { id: string };

describe("assignRef", () => {
  it("同时支持回调 Ref 和可变对象 Ref", () => {
    const value: RefValue = { id: "node" };
    const callback = vi.fn<(node: RefValue | null) => void>();
    const mutable: { current: RefValue | null } = { current: null };

    assignRef(callback, value);
    assignRef(mutable, value);

    expect(callback).toHaveBeenCalledWith(value);
    expect(mutable.current).toBe(value);
  });

  it("忽略未提供的 Ref", () => {
    expect(() => assignRef<RefValue>(undefined, null)).not.toThrow();
  });
});

describe("mergeRefs", () => {
  it("按顺序同步节点并在卸载时写入 null", () => {
    const calls: string[] = [];
    const first: Ref<RefValue> = (value) => {
      calls.push(`first:${value?.id ?? "null"}`);
    };
    const second: Ref<RefValue> = (value) => {
      calls.push(`second:${value?.id ?? "null"}`);
    };
    const mutable: { current: RefValue | null } = { current: null };
    const merged = mergeRefs(first, undefined, mutable, second);
    const value: RefValue = { id: "node" };

    merged(value);
    expect(mutable.current).toBe(value);
    merged(null);

    expect(mutable.current).toBeNull();
    expect(calls).toEqual([
      "first:node",
      "second:node",
      "first:null",
      "second:null",
    ]);
  });
});
