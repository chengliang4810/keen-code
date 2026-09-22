import { describe, expect, it } from "vitest";
import {
  collapsedIdsFromExpandMap,
  expandMapFromCollapsedIds,
  sameCollapsedIdSet,
} from "./sidebarExpand";

describe("sidebarExpand", () => {
  it("defaults unknown projects to expanded", () => {
    expect(expandMapFromCollapsedIds(["a", "b"], ["b"])).toEqual({
      a: true,
      b: false,
    });
    expect(expandMapFromCollapsedIds(["a"], null)).toEqual({ a: true });
    expect(expandMapFromCollapsedIds(["a"], undefined)).toEqual({ a: true });
  });

  it("round-trips collapsed ids from the expand map", () => {
    const map = { a: true, b: false, c: false };
    expect(collapsedIdsFromExpandMap(map)).toEqual(["b", "c"]);
    expect(
      expandMapFromCollapsedIds(
        ["a", "b", "c"],
        collapsedIdsFromExpandMap(map),
      ),
    ).toEqual(map);
  });

  it("compares collapsed id sets order-insensitively", () => {
    expect(sameCollapsedIdSet(["b", "a"], ["a", "b"])).toBe(true);
    expect(sameCollapsedIdSet(["a"], ["a", "b"])).toBe(false);
  });
});
