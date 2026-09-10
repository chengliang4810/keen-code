import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { createAcpWorkspaceState, emptySession } from "@/lib/acp/store";
import { IDLE_SNAPSHOT, type ChatMessage } from "@/lib/session";
import { useAcpRuntimeProjection, type AcpRuntimeProjectionOptions } from "./projection";

it("目标投影只保留目标会话的未提交气泡，不继承全局上一帧", () => {
  let visible: ChatMessage[] = [{ id: "u-cart", role: "user", content: "cart failed" }];
  const cache = new Map<string, ChatMessage[]>([["cart", visible], ["dashboard", [{ id: "u-dash", role: "user", content: "dash failed" }]]]);
  const workspace = createAcpWorkspaceState();
  workspace.sessions.dashboard = emptySession("dashboard");
  workspace.sessions.empty = emptySession("empty");
  const options: AcpRuntimeProjectionOptions = {
    locale: "zh", acpWorkspaceRef: { current: workspace }, activeTurnIdBySessionRef: { current: new Map() },
    contextUsageBySessionRef: { current: new Map() }, sessionTitleOverridesRef: { current: new Map() },
    sessionsRef: { current: [] }, sendInFlightRef: { current: false }, liveHostRef: { current: IDLE_SNAPSHOT },
    messagesBySessionRef: { current: cache }, setMessages: (value) => { visible = typeof value === "function" ? value(visible) : value; },
    setContextUsage: vi.fn(), setSession: vi.fn(), setLiveHost: vi.fn(), setLiveMap: vi.fn(), setRetryStatus: vi.fn(), setEffort: vi.fn(),
  };
  let result!: ReturnType<typeof useAcpRuntimeProjection>;
  function Harness() { result = useAcpRuntimeProjection(options); return null; }
  renderToString(createElement(Harness));
  result.applyViewProjection("dashboard");
  expect(visible.map((message) => message.content)).toEqual(["dash failed"]);
  result.applyViewProjection("empty");
  expect(visible).toEqual([]);
  expect(cache.get("cart")?.[0].content).toBe("cart failed");
});

it("恢复窗口内的投影不清空当前消息与本地缓存，恢复完成后才整体替换", () => {
  let visible: ChatMessage[] = [{ id: "u-cart", role: "user", content: "cached" }];
  const cached: ChatMessage[] = [{ id: "u-cart", role: "user", content: "cached" }];
  const cache = new Map<string, ChatMessage[]>([["cart", cached]]);
  const workspace = createAcpWorkspaceState();
  const view = emptySession("cart");
  view.replay.restoring = true;
  workspace.sessions.cart = view;
  const options: AcpRuntimeProjectionOptions = {
    locale: "zh", acpWorkspaceRef: { current: workspace }, activeTurnIdBySessionRef: { current: new Map() },
    contextUsageBySessionRef: { current: new Map() }, sessionTitleOverridesRef: { current: new Map() },
    sessionsRef: { current: [] }, sendInFlightRef: { current: false }, liveHostRef: { current: IDLE_SNAPSHOT },
    messagesBySessionRef: { current: cache }, setMessages: (value) => { visible = typeof value === "function" ? value(visible) : value; },
    setContextUsage: vi.fn(), setSession: vi.fn(), setLiveHost: vi.fn(), setLiveMap: vi.fn(), setRetryStatus: vi.fn(), setEffort: vi.fn(),
  };
  let result!: ReturnType<typeof useAcpRuntimeProjection>;
  function Harness() { result = useAcpRuntimeProjection(options); return null; }
  renderToString(createElement(Harness));
  result.applyViewProjection("cart");
  expect(visible.map((message) => message.content)).toEqual(["cached"]);
  expect(cache.get("cart")).toBe(cached);
  view.replay.restoring = false;
  view.history.push({ role: "user", content: "loaded" });
  result.applyViewProjection("cart");
  // 恢复完成后按既有语义整体替换：历史 + 未持久化的乐观用户消息。
  expect(visible.map((message) => message.content)).toEqual(["loaded", "cached"]);
});
