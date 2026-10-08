import type { RefObject } from "react";
import type { Tab } from "@/modules/tabs/lib/useTabs";
import { afterEach, describe, expect, it, vi } from "vitest";

const ports = vi.hoisted(() => ({
  effects: [] as (() => void)[],
  invoke: vi.fn(async (_name: string, _args?: unknown) => true),
  listen: vi.fn(),
  window: { show: vi.fn(), setFocus: vi.fn() },
}));

vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: (effect: () => (() => void)) => ports.effects.push(effect()),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ports.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: ports.listen }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ports.window }));

import { useControlBridge } from "@/modules/control/useControlBridge";

afterEach(() => {
  for (const cleanup of ports.effects.splice(0)) cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("background CLI open", () => {
  it("waits for navigation and responds without a rendering frame", async () => {
    const raf = vi.fn();
    vi.stubGlobal("requestAnimationFrame", raf);
    ports.listen.mockResolvedValue(vi.fn());
    let completeOpen: (value: number) => void = () => {};
    const onOpen = vi.fn(() => new Promise<number>((resolve) => { completeOpen = resolve; }));
    useControlBridge({
      ready: true,
      tabsRef: { current: [] } as RefObject<Tab[]>,
      activeTabIdRef: { current: 0 },
      activeSpaceIdRef: { current: "project" },
      onOpen,
    });
    const callback = ports.listen.mock.calls[0][1];
    callback({ payload: { id: "hidden-open", method: "open", caller: {}, params: { path: "/repo/main.rs", focus: false } } });
    expect(onOpen).toHaveBeenCalledOnce();
    expect(ports.invoke.mock.calls.filter(([name]) => name === "control_respond")).toHaveLength(0);
    completeOpen(42);
    await vi.waitFor(() => expect(ports.invoke).toHaveBeenCalledWith("control_respond", {
      requestId: "hidden-open",
      response: {
        ok: true,
        result: { path: "/repo/main.rs", line: null, tab_id: 42, space_id: "project", focus_requested: false, focused: false },
      },
    }));
    expect(raf).not.toHaveBeenCalled();
    expect(ports.window.show).not.toHaveBeenCalled();
    expect(ports.window.setFocus).not.toHaveBeenCalled();
  });
});
