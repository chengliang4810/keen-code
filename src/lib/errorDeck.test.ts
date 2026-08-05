import { describe, expect, it } from "vitest";
import {
  buildErrorDeck,
  deckCodeFromAgent,
  isReconnectAction,
} from "./errorDeck";

describe("buildErrorDeck", () => {
  it("returns problem/cause/actions for the four product classes (en)", () => {
    const runtime = buildErrorDeck("RUNTIME_UNAVAILABLE", "en");
    expect(runtime.problem.toLowerCase()).toMatch(/runtime/);
    expect(runtime.cause.length).toBeGreaterThan(8);
    expect(runtime.primary.id).toBe("reconnect");
    expect(runtime.secondary?.id).toBe("dismiss");

    const auth = buildErrorDeck("AUTH_FAILED", "en");
    expect(auth.problem.toLowerCase()).toMatch(/auth|login|key/);
    expect(auth.primary.id).toBe("open_account");

    const net = buildErrorDeck("NETWORK_PROVIDER", "en");
    expect(net.problem.toLowerCase()).toMatch(/network|provider|model/);
    expect(isReconnectAction(net.primary.id)).toBe(true);

    const crash = buildErrorDeck("AGENT_CRASHED", "en");
    expect(crash.problem.toLowerCase()).toMatch(/agent|crash|process/);
    expect(crash.primary.id).toBe("reconnect");
    expect(crash.secondary?.id).toBe("dismiss");

    const connect = buildErrorDeck("CONNECT_FAILED", "en");
    expect(connect.primary.id).toBe("reconnect");
    expect(connect.secondary?.id).toBe("open_providers");

    const limit = buildErrorDeck("PROCESS_LIMIT", "en");
    expect(limit.primary.id).toBe("reconnect");
    expect(limit.secondary?.id).toBe("dismiss");
  });

  it("returns Chinese copy for zh", () => {
    const runtime = buildErrorDeck("RUNTIME_UNAVAILABLE", "zh");
    expect(runtime.problem).toMatch(/运行时|不可用/i);
    expect(runtime.cause).toMatch(/重连|重试/);
    expect(runtime.primary.label.length).toBeGreaterThan(1);
  });

  it("maps timeout / disconnect specials", () => {
    expect(deckCodeFromAgent("NETWORK_PROVIDER", { timeout: true })).toBe(
      "TURN_TIMEOUT",
    );
    expect(deckCodeFromAgent(null, { disconnected: true })).toBe(
      "AGENT_DISCONNECTED",
    );
    expect(deckCodeFromAgent("AUTH_FAILED")).toBe("AUTH_FAILED");
  });

  it("STREAM_STALL uses keep_waiting / cancel_turn (not dual dismiss)", () => {
    const stall = buildErrorDeck("STREAM_STALL", "en");
    expect(stall.code).toBe("STREAM_STALL");
    expect(stall.problem.toLowerCase()).toMatch(/stuck|stream/);
    expect(stall.primary.id).toBe("keep_waiting");
    expect(stall.secondary?.id).toBe("cancel_turn");
    expect(stall.primary.label.toLowerCase()).toMatch(/wait/);
    expect(stall.secondary?.label.toLowerCase()).toMatch(/cancel/);
  });
});
