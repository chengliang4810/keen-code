import { describe, expect, it } from "vitest";
import {
  drawEffortTrackFrame,
  edgeFade,
  noise,
  parseColor,
  resolveEffortTrackKind,
  trackFrameIntervalMs,
  withAlpha,
  type EffortTrackFrame,
} from "./effortTrack";

describe("resolveEffortTrackKind", () => {
  it("四态由最高档与快速两个布尔量组合", () => {
    expect(resolveEffortTrackKind(false, false)).toBe("plain");
    expect(resolveEffortTrackKind(true, false)).toBe("supercharged");
    expect(resolveEffortTrackKind(false, true)).toBe("fast");
    expect(resolveEffortTrackKind(true, true)).toBe("fusion");
  });
});

describe("trackFrameIntervalMs", () => {
  it("纯最高档 30fps，其余 60fps", () => {
    expect(trackFrameIntervalMs("supercharged")).toBeCloseTo(1000 / 30);
    expect(trackFrameIntervalMs("fast")).toBeCloseTo(1000 / 60);
    expect(trackFrameIntervalMs("fusion")).toBeCloseTo(1000 / 60);
  });
});

describe("noise", () => {
  it("同一序号与属性给出稳定值，且落在 [0, 1)", () => {
    for (let index = 0; index < 200; index += 7) {
      for (let trait = 1; trait <= 6; trait += 1) {
        const value = noise(index, trait);
        expect(value).toBe(value % 1);
        expect(value).toBeGreaterThanOrEqual(0);
        expect(value).toBeLessThan(1);
        expect(noise(index, trait)).toBe(value);
      }
    }
  });

  it("不同属性给出不同值", () => {
    expect(noise(3, 1)).not.toBe(noise(3, 2));
  });
});

describe("edgeFade", () => {
  it("两端 14 点内淡出，中部为 1", () => {
    expect(edgeFade(0, 200)).toBe(0);
    expect(edgeFade(7, 200)).toBeCloseTo(0.5);
    expect(edgeFade(100, 200)).toBe(1);
    expect(edgeFade(200, 200)).toBe(0);
  });
});

describe("parseColor / withAlpha", () => {
  it("解析 hex 与 rgb 字面量", () => {
    expect(parseColor("#cc6b47")).toEqual({ r: 204, g: 107, b: 71 });
    expect(parseColor("#fff")).toEqual({ r: 255, g: 255, b: 255 });
    expect(parseColor("rgb(12, 34, 56)")).toEqual({ r: 12, g: 34, b: 56 });
  });

  it("CSS 变量退化值返回 null", () => {
    expect(parseColor("var(--missing)")).toBeNull();
    expect(parseColor("")).toBeNull();
  });

  it("withAlpha 生成 rgba 字面量", () => {
    expect(withAlpha({ r: 204, g: 107, b: 71 }, 0.5)).toBe(
      "rgba(204, 107, 71, 0.5)",
    );
  });
});

/** 记录桩：只统计操作与填充样式，不依赖 DOM。 */
function recordingContext() {
  const fills: string[] = [];
  const strokes: string[] = [];
  const ops: string[] = [];
  let composite: GlobalCompositeOperation = "source-over";
  const gradient = {
    addColorStop: (offset: number, color: string) => {
      ops.push(`stop:${offset}:${color}`);
    },
  };
  const ctx = {
    get fillStyle() {
      return fills.at(-1) ?? "";
    },
    set fillStyle(value: string) {
      fills.push(value);
    },
    get strokeStyle() {
      return strokes.at(-1) ?? "";
    },
    set strokeStyle(value: string) {
      strokes.push(value);
    },
    get globalCompositeOperation() {
      return composite;
    },
    set globalCompositeOperation(value: GlobalCompositeOperation) {
      composite = value;
      ops.push(`composite:${value}`);
    },
    lineWidth: 1,
    lineCap: "butt" as CanvasLineCap,
    lineJoin: "miter" as CanvasLineJoin,
    shadowBlur: 0,
    shadowColor: "",
    beginPath: () => ops.push("beginPath"),
    moveTo: () => ops.push("moveTo"),
    lineTo: () => ops.push("lineTo"),
    closePath: () => ops.push("closePath"),
    arc: () => ops.push("arc"),
    fill: () => ops.push("fill"),
    stroke: () => ops.push("stroke"),
    save: () => ops.push("save"),
    restore: () => ops.push("restore"),
    clip: () => ops.push("clip"),
    createLinearGradient: () => gradient,
    createRadialGradient: () => gradient,
  };
  return { fills, strokes, ops, ctx };
}

const PALETTE = { brand: "#cc6b47", fast: "#ffd65c", spark: "#ffffff" };

function frame(kind: EffortTrackFrame["kind"], width = 240): EffortTrackFrame {
  return { kind, width, height: 34, time: 1.37, palette: PALETTE };
}

describe("drawEffortTrackFrame", () => {
  it("plain 不产生任何绘制", () => {
    const { ctx, ops } = recordingContext();
    drawEffortTrackFrame(ctx, frame("plain"));
    expect(ops).toEqual([]);
  });

  it("supercharged 只画粒子", () => {
    const { ctx, ops } = recordingContext();
    drawEffortTrackFrame(ctx, frame("supercharged"));
    expect(ops.filter((op) => op === "arc").length).toBeGreaterThanOrEqual(10);
    expect(ops).not.toContain("composite:lighter");
  });

  it("fast 画速度线与闪电，无粒子", () => {
    const { ctx, ops } = recordingContext();
    // time=0.1 落在 slot 0 的闪电窗口内（周期 1.969s、相位 0.1 < 0.26）。
    drawEffortTrackFrame(ctx, { ...frame("fast"), time: 0.1 });
    // 每条速度线一个 fill；闪烁中的闪电辉光 + 白芯各一次 stroke。
    expect(ops.filter((op) => op === "fill").length).toBeGreaterThanOrEqual(5);
    expect(ops.filter((op) => op === "stroke").length).toBe(2);
  });

  it("闪电只在自己的窗口内出现", () => {
    const { ctx, ops } = recordingContext();
    // time=1.37 时两个槽都不在 0.26s 的闪烁窗口内。
    drawEffortTrackFrame(ctx, { ...frame("fast"), time: 1.37 });
    expect(ops.filter((op) => op === "stroke")).toEqual([]);
  });

  it("fusion 画融合渐变、等离子、火花与接缝核心", () => {
    const { ctx, ops } = recordingContext();
    drawEffortTrackFrame(ctx, frame("fusion"));
    expect(ops).toContain("composite:lighter");
    expect(ops.filter((op) => op === "clip").length).toBe(6);
    // 火花是 arc；核心的辉光与亮线加上渐变底共有多次 fill。
    expect(ops.filter((op) => op === "arc").length).toBeGreaterThanOrEqual(10);
  });

  it("太窄的画布直接跳过", () => {
    const { ctx, ops } = recordingContext();
    drawEffortTrackFrame(ctx, { ...frame("fusion"), width: 4 });
    expect(ops).toEqual([]);
  });
});
