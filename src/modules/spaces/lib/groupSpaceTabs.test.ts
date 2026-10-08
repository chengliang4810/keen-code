import { expect, it } from "vitest";
import { groupSpaceTabs } from "./groupSpaceTabs";
import type { Tab } from "@/modules/tabs/lib/useTabs";

it("includes an explicit empty list for projects whose last tool was closed", () => {
  const tab: Tab = {
    id: 1,
    kind: "editor",
    spaceId: "b",
    path: "/file",
    title: "file",
    dirty: false,
    preview: false,
  };
  const groups = groupSpaceTabs([tab], ["a", "b"]);
  expect(groups.get("a")).toEqual([]);
  expect(groups.get("b")).toEqual([tab]);
});

it("retains all empty projects when every native tool has been closed", () => {
  expect([...groupSpaceTabs([], ["a", "b"])]).toEqual([
    ["a", []],
    ["b", []],
  ]);
});
