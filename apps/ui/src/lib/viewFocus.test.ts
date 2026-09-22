import { describe, expect, it } from "vitest";
import {
  isSameView,
  isViewingSendTarget,
  shouldAdoptView,
  type ViewFocus,
} from "./viewFocus";

const at = (sessionId: string | null, epoch: number): ViewFocus => ({
  sessionId,
  epoch,
});

describe("viewFocus", () => {
  it("treats two different drafts as different views", () => {
    // Both are `sessionId: null`; only the epoch tells them apart.
    const draftA = at(null, 1);
    const draftB = at(null, 2);
    expect(isSameView(draftA, draftA)).toBe(true);
    expect(isSameView(draftA, draftB)).toBe(false);
  });

  it("does not yank the workbench back when the user opened a new draft", () => {
    // Send from draft A → materializes as chat "s1" while the user has already
    // hit "new chat" in another project (epoch bumped, still a draft).
    const origin = at(null, 1);
    const current = at(null, 2);
    expect(shouldAdoptView(origin, current, "s1")).toBe(false);
  });

  it("still adopts the view when the user never navigated", () => {
    const origin = at(null, 1);
    expect(shouldAdoptView(origin, at(null, 1), "s1")).toBe(true);
  });

  it("adopts when the user is already looking at that chat", () => {
    // e.g. user opened s1 manually while connect was in flight.
    const origin = at(null, 1);
    expect(shouldAdoptView(origin, at("s1", 5), "s1")).toBe(true);
  });

  it("does not adopt when the user switched to a different chat", () => {
    const origin = at("s1", 1);
    expect(shouldAdoptView(origin, at("s2", 2), "s1")).toBe(false);
  });

  it("scopes draft sends by epoch but real sends by id", () => {
    const origin = at(null, 1);
    // Draft send, user still on that draft → optimistic UI belongs here.
    expect(isViewingSendTarget(origin, at(null, 1), null)).toBe(true);
    // Draft send, user opened a *different* new draft → must not paint it.
    expect(isViewingSendTarget(origin, at(null, 2), null)).toBe(false);
    // Draft send, user navigated to a real chat.
    expect(isViewingSendTarget(origin, at("s9", 2), null)).toBe(false);

    // Real target: id comparison wins regardless of navigation count.
    const originS1 = at("s1", 1);
    expect(isViewingSendTarget(originS1, at("s1", 7), "s1")).toBe(true);
    expect(isViewingSendTarget(originS1, at("s2", 7), "s1")).toBe(false);
    expect(isViewingSendTarget(originS1, at(null, 7), "s1")).toBe(false);
  });
});
