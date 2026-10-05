import { describe, expect, it } from "vitest";
import { shouldStartNativeWindowDrag } from "../src/nativeWindowDrag.js";

describe("native window drag hit testing", () => {
  it("only accepts an explicit drag region", () => {
    expect(
      shouldStartNativeWindowDrag({
        hasDragRegion: true,
        hasNoDragRegion: false,
        isInteractive: false,
      }),
    ).toBe(true);
    expect(
      shouldStartNativeWindowDrag({
        hasDragRegion: false,
        hasNoDragRegion: false,
        isInteractive: false,
      }),
    ).toBe(false);
  });

  it("never drags from a no-drag or interactive descendant", () => {
    expect(
      shouldStartNativeWindowDrag({
        hasDragRegion: true,
        hasNoDragRegion: true,
        isInteractive: false,
      }),
    ).toBe(false);
    expect(
      shouldStartNativeWindowDrag({
        hasDragRegion: true,
        hasNoDragRegion: false,
        isInteractive: true,
      }),
    ).toBe(false);
  });
});
