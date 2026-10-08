import { afterEach, describe, expect, it } from "vitest";
import { useSidebarWidth } from "./sidebarWidth";

const initialWidth = useSidebarWidth.getState().width;

afterEach(() => useSidebarWidth.setState({ width: initialWidth }));

describe("shared sidebar width", () => {
  it("retains the expanded width when the conversation sidebar collapses", () => {
    useSidebarWidth.getState().reportWidth(360);
    useSidebarWidth.getState().reportWidth(0);
    expect(useSidebarWidth.getState().width).toBe(360);
  });

  it("uses actual layout sizes without persisting invalid measurements", () => {
    useSidebarWidth.getState().reportWidth(310);
    for (const width of [-1, NaN, Infinity]) {
      useSidebarWidth.getState().reportWidth(width);
    }
    expect(useSidebarWidth.getState().width).toBe(310);
    useSidebarWidth.getState().reportWidth(270);
    expect(useSidebarWidth.getState().width).toBe(270);
  });
});
