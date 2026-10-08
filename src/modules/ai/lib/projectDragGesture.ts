export const PROJECT_DRAG_DISTANCE = 8;
type Point = { x: number; y: number };

/** 以位移区分点击与拖拽，不等待长按；阈值内的手抖仍保留普通点击。 */
export function createProjectDragGesture(
  start: Point,
  callbacks: {
    activate: () => void;
    move: (point: Point) => void;
    finish: (commit: boolean) => void;
  },
) {
  let phase: "pending" | "dragging" | "ended" = "pending";
  let suppressClick = false;
  return {
    move(point: Point) {
      if (
        phase === "pending" &&
        Math.hypot(point.x - start.x, point.y - start.y) > PROJECT_DRAG_DISTANCE
      ) {
        phase = "dragging";
        suppressClick = true;
        callbacks.activate();
      }
      if (phase === "dragging") callbacks.move(point);
    },
    end(commit: boolean): boolean {
      if (phase === "ended") return suppressClick;
      const active = phase === "dragging";
      phase = "ended";
      if (active) callbacks.finish(commit);
      return suppressClick;
    },
  };
}
