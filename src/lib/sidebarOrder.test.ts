import { describe, expect, it } from "vitest";
import { moveId, orderedByIds } from "./sidebarOrder";

describe("sidebar order", () => {
  it("keeps new items first and moves an existing item before its target", () => {
    const items = [{ id: "new" }, { id: "a" }, { id: "b" }];
    expect(orderedByIds(items, ["b", "a"]).map(({ id }) => id)).toEqual(["new", "b", "a"]);
    expect(moveId(["new", "b", "a"], "a", "b")).toEqual(["new", "a", "b"]);
    expect(moveId(["a", "b", "c"], "a", "c", true)).toEqual(["b", "c", "a"]);
  });
});
