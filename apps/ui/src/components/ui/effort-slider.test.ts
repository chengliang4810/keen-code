import { describe, expect, it } from "vitest";
import {
  effortIndexFromSliderValue,
  effortSliderStep,
} from "./effort-slider";

describe("EffortSlider 的 0-100 离散档位映射", () => {
  it.each([
    [2, 100],
    [3, 50],
    [4, 100 / 3],
    [5, 25],
    [6, 20],
  ])("%i 个思考强度的 step 为 %f", (count, expected) => {
    expect(effortSliderStep(count)).toBeCloseTo(expected);
  });

  it("0 始终映射最小档，100 始终映射最大档", () => {
    for (const count of [2, 3, 4, 5, 6]) {
      expect(effortIndexFromSliderValue(0, count)).toBe(0);
      expect(effortIndexFromSliderValue(100, count)).toBe(count - 1);
    }
  });

  it("拖动值会吸附到最近的模型思考强度", () => {
    expect(effortIndexFromSliderValue(10, 4)).toBe(0);
    expect(effortIndexFromSliderValue(20, 4)).toBe(1);
    expect(effortIndexFromSliderValue(49, 4)).toBe(1);
    expect(effortIndexFromSliderValue(51, 4)).toBe(2);
    expect(effortIndexFromSliderValue(90, 4)).toBe(3);
  });
});
