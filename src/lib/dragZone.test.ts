import { describe, expect, it } from "vitest";
import {
  dragPosNeedsScale,
  hitDragZoneFromRects,
  toClientDragPoint,
} from "./dragZone";

describe("dragPosNeedsScale", () => {
  it("mac keeps logical points (no scale)", () => {
    expect(dragPosNeedsScale("mac")).toBe(false);
  });

  it("win / other use physical → divide by scale", () => {
    expect(dragPosNeedsScale("win")).toBe(true);
    expect(dragPosNeedsScale("other")).toBe(true);
    expect(dragPosNeedsScale("linux")).toBe(true);
  });
});

describe("toClientDragPoint", () => {
  it("mac: use position as-is even when scaleFactor is 2", () => {
    expect(toClientDragPoint({ x: 400, y: 100 }, 2, "mac")).toEqual({
      x: 400,
      y: 100,
    });
  });

  it("win: divide by scaleFactor", () => {
    expect(toClientDragPoint({ x: 800, y: 200 }, 2, "win")).toEqual({
      x: 400,
      y: 100,
    });
  });
});

describe("hitDragZoneFromRects", () => {
  const projectDrop = {
    left: 300,
    right: 700,
    top: 220,
    bottom: 420,
    width: 400,
  };

  it("adds projects only inside the open panel drop control", () => {
    expect(hitDragZoneFromRects(300, 220, projectDrop, true)).toBe("project");
    expect(hitDragZoneFromRects(699, 420, projectDrop, true)).toBe("project");
    expect(hitDragZoneFromRects(700, 300, projectDrop, true)).toBeNull();
    expect(hitDragZoneFromRects(200, 300, projectDrop, true)).toBeNull();
  });

  it("treats drops as attachments when the panel is closed", () => {
    expect(hitDragZoneFromRects(350, 300, projectDrop, false)).toBe("main");
    expect(hitDragZoneFromRects(100, 100, null, false)).toBe("main");
  });

  it("ignores modal drops until the control is mounted", () => {
    expect(hitDragZoneFromRects(350, 300, null, true)).toBeNull();
    expect(hitDragZoneFromRects(350, 300, { ...projectDrop, width: 0 }, true)).toBeNull();
  });
});
