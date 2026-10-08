import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createProjectDragGesture,
  PROJECT_DRAG_DISTANCE,
} from "@/modules/ai/lib/projectDragGesture";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());
const callbacks = () => ({ activate: vi.fn(), move: vi.fn(), finish: vi.fn() });

describe("project distance drag gesture", () => {
  it("keeps pressing without movement as a click regardless of duration", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(1000);
    expect(gesture.end(true)).toBe(false);
    vi.runAllTimers();
    expect(cb.activate).not.toHaveBeenCalled();
    expect(cb.finish).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });
  it("activates immediately past the threshold and forwards the same movement", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    gesture.move({ x: 12, y: 22 });
    expect(cb.move).not.toHaveBeenCalled();
    const point = { x: 10, y: 20 + PROJECT_DRAG_DISTANCE + 1 };
    gesture.move(point);
    expect(cb.activate).toHaveBeenCalledTimes(1);
    expect(cb.move).toHaveBeenCalledExactlyOnceWith(point);
    gesture.move({ x: 40, y: 90 });
    expect(cb.activate).toHaveBeenCalledTimes(1);
    expect(cb.move).toHaveBeenCalledWith({ x: 40, y: 90 });
    expect(gesture.end(true)).toBe(true);
    expect(cb.finish).toHaveBeenCalledWith(true);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("keeps small movements including the exact threshold as a click", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    gesture.move({ x: 12, y: 22 });
    gesture.move({ x: 10 + PROJECT_DRAG_DISTANCE, y: 20 });
    expect(gesture.end(true)).toBe(false);
    expect(cb.activate).not.toHaveBeenCalled();
    expect(cb.move).not.toHaveBeenCalled();
    expect(cb.finish).not.toHaveBeenCalled();
  });
  it("cancels an activated drag without committing and finishes only once", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    gesture.move({ x: 40, y: 20 });
    expect(gesture.end(false)).toBe(true);
    gesture.end(true);
    const moves = cb.move.mock.calls.length;
    gesture.move({ x: 99, y: 99 });
    expect(cb.finish).toHaveBeenCalledExactlyOnceWith(false);
    expect(cb.move).toHaveBeenCalledTimes(moves);
  });
  it("does not activate after cancellation before the threshold", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    expect(gesture.end(false)).toBe(false);
    gesture.move({ x: 99, y: 99 });
    vi.runAllTimers();
    expect(cb.activate).not.toHaveBeenCalled();
    expect(cb.finish).not.toHaveBeenCalled();
  });
  it("uses total distance for diagonal movement", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    gesture.move({ x: 16, y: 26 });
    expect(cb.activate).toHaveBeenCalledTimes(1);
    expect(gesture.end(true)).toBe(true);
  });
  it("suppresses release clicks even when a drag returns to its origin", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    gesture.move({ x: 30, y: 20 });
    gesture.move({ x: 10, y: 20 });
    expect(gesture.end(true)).toBe(true);
  });
  it("does not forward movement if activation itself cancels the gesture", () => {
    const cb = callbacks();
    const gesture = createProjectDragGesture({ x: 10, y: 20 }, cb);
    cb.activate.mockImplementation(() => gesture.end(false));
    gesture.move({ x: 40, y: 20 });
    expect(cb.finish).toHaveBeenCalledExactlyOnceWith(false);
    expect(cb.move).not.toHaveBeenCalled();
    expect(gesture.end(true)).toBe(true);
  });
});
