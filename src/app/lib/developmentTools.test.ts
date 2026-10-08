import { describe, expect, it } from "vitest";
import {
  openUtilityTab,
  toolViewAfterClose,
  type UtilityTool,
} from "./developmentTools";

describe("development tool singletons", () => {
  it("repeated opens keep exactly one file and Git tab", () => {
    let tabs: UtilityTool[] = [];
    for (let i = 0; i < 3; i++) {
      tabs = openUtilityTab(tabs, "explorer");
      tabs = openUtilityTab(tabs, "source-control");
    }
    expect(tabs).toEqual(["explorer", "source-control"]);
  });

  it("closing an inactive singleton leaves the active terminal unchanged", () => {
    expect(
      toolViewAfterClose(
        ["explorer", "source-control"],
        "workspace",
        "explorer",
        true,
      ),
    ).toBe("workspace");
  });

  it("closing the active utility falls back to the remaining utility then native content", () => {
    expect(
      toolViewAfterClose(
        ["explorer", "source-control"],
        "explorer",
        "explorer",
        true,
      ),
    ).toBe("source-control");
    expect(toolViewAfterClose(["explorer"], "explorer", "explorer", true)).toBe(
      "workspace",
    );
  });

  it("closing all tools yields an empty panel that can reopen the same singleton", () => {
    expect(
      toolViewAfterClose(["explorer"], "explorer", "explorer", false),
    ).toBe("empty");
    expect(openUtilityTab([], "explorer")).toEqual(["explorer"]);
  });
});
