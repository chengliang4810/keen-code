import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationSelectionReference } from "../../../packages/ui/src/lib/conversationSelectionReference.js";
import {
  buildSelectionSideChatKey,
  registerSelectionSideChatOpener,
  requestSelectionSideChatOpen,
} from "../../../packages/ui/src/lib/selectionSideChatRuntime.js";

describe("selection side chat opener routing", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("only routes a launcher request to the exact workspace and parent", async () => {
    const exactKey = buildSelectionSideChatKey("workspace-a", "parent-a");
    const otherKey = buildSelectionSideChatKey("workspace-b", "parent-a");
    let exactCalls = 0;
    let otherCalls = 0;
    const disposeExact = registerSelectionSideChatOpener(
      exactKey,
      async () => {
        exactCalls += 1;
      },
      true,
    );
    const disposeOther = registerSelectionSideChatOpener(
      otherKey,
      async () => {
        otherCalls += 1;
      },
      true,
    );

    expect(requestSelectionSideChatOpen(exactKey)).toBe(true);
    expect(requestSelectionSideChatOpen(otherKey)).toBe(true);
    expect(exactCalls).toBe(1);
    expect(otherCalls).toBe(1);

    disposeOther();
    disposeExact();
  });

  it("replays an exact-key request after the SessionPane opener registers", async () => {
    const key = buildSelectionSideChatKey("workspace-a", "parent-a");
    const reference = {
      id: "reference-1",
      sourceSessionId: "parent-a",
      sourceRowId: 1,
      contentType: "user" as const,
      text: "selected text",
    };
    let receivedReference: ConversationSelectionReference | undefined;

    expect(requestSelectionSideChatOpen(key, reference)).toBe(true);
    const dispose = registerSelectionSideChatOpener(
      key,
      async (received: ConversationSelectionReference | undefined) => {
        receivedReference = received;
      },
      true,
    );

    await Promise.resolve();
    expect(receivedReference).toEqual(reference);
    dispose();
  });

  it("keeps a deferred request isolated from another opener", async () => {
    const requestedKey = buildSelectionSideChatKey("workspace-a", "parent-a");
    const unrelatedKey = buildSelectionSideChatKey("workspace-b", "parent-a");
    let unrelatedCalls = 0;
    let requestedCalls = 0;

    expect(requestSelectionSideChatOpen(requestedKey)).toBe(true);
    const disposeUnrelated = registerSelectionSideChatOpener(
      unrelatedKey,
      async () => {
        unrelatedCalls += 1;
      },
      true,
    );
    await Promise.resolve();
    expect(unrelatedCalls).toBe(0);

    const disposeRequested = registerSelectionSideChatOpener(
      requestedKey,
      async () => {
        requestedCalls += 1;
      },
      true,
    );
    await Promise.resolve();
    expect(unrelatedCalls).toBe(0);
    expect(requestedCalls).toBe(1);

    disposeRequested();
    disposeUnrelated();
  });
});
