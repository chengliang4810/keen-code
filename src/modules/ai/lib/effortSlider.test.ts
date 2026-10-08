import {
  effortPointerPosition,
  effortStopPosition,
} from "@/modules/ai/lib/effortSlider";
import { describe, expect, it } from "vitest";

describe("effort slider geometry", () => {
  it.each([2, 4, 7, 16])(
    "keeps all %i model levels evenly spaced inside the knob bounds",
    (count) => {
      const width = 288;
      expect(effortStopPosition(0, count, width)).toBe(18);
      expect(effortStopPosition(count - 1, count, width)).toBe(width - 18);
      for (let index = 1; index < count; index++)
        expect(
          effortStopPosition(index, count, width) -
            effortStopPosition(index - 1, count, width),
        ).toBeCloseTo((width - 36) / (count - 1));
    },
  );

  it("uses layout coordinates while the popover is scaled", () => {
    const layoutWidth = 288;
    const renderedWidth = layoutWidth * 0.95;
    expect(
      effortPointerPosition(
        100 + renderedWidth,
        100,
        renderedWidth,
        layoutWidth,
      ),
    ).toBe(270);
    expect(
      effortPointerPosition(
        100 + renderedWidth / 2,
        100,
        renderedWidth,
        layoutWidth,
      ),
    ).toBe(144);
  });

  it("clamps outside drags and handles unselected or collapsed tracks", () => {
    expect(effortPointerPosition(-100, 0, 288, 288)).toBe(18);
    expect(effortPointerPosition(1000, 0, 288, 288)).toBe(270);
    expect(effortPointerPosition(1000, 0, 0, 0)).toBe(0);
    expect(effortStopPosition(-1, 4, 288)).toBe(18);
    expect(effortStopPosition(0, 1, 288)).toBe(18);
    expect(effortStopPosition(0, 1, 12)).toBe(6);
  });
});
