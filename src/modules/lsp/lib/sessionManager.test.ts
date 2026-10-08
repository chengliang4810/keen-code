import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ports = vi.hoisted(() => ({
  activation: "enabled",
  starts: [] as {
    root: string;
    resolve: () => void;
    reject: (error: Error) => void;
  }[],
  transports: [] as { closed: boolean }[],
  shutdowns: [] as (() => void)[],
  deferShutdown: false,
  closes: [] as (() => void)[],
  deferClose: false,
  preferenceListener: null as null | ((
    state: { lspActivation: Record<string, string> },
    previous: { lspActivation: Record<string, string> },
  ) => void),
}));

vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: {
    getState: () => ({
      lspActivation: { server: ports.activation },
      lspCustomServers: [],
    }),
    subscribe: vi.fn((listener) => {
      ports.preferenceListener = listener;
      return () => {};
    }),
  },
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
}));
vi.mock("@/modules/lsp/lib/detect", () => ({
  detectBinary: async () => "/bin/server",
}));
vi.mock("@/modules/lsp/lib/presets", () => ({
  serverForLanguage: () => ({
    id: "server",
    name: "Server",
    command: "server",
    args: [],
    rootMarkers: [".git"],
    languages: { ts: "typescript" },
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (name: string, args: { path: string }) =>
    name === "lsp_resolve_root" ? args.path.split("/file")[0] : 1,
}));
vi.mock("@/modules/lsp/lib/client", () => ({
  RCodeLspClient: class {
    static hostPid = 1;
    initializePromise = Promise.resolve();
    textDocumentDidClose() {}
    close() {}
    shutdownGracefully() {
      return ports.deferShutdown
        ? new Promise<void>((resolve) => ports.shutdowns.push(resolve))
        : Promise.resolve();
    }
  },
  lspInteractions: () => [],
  languageServerWithTransport: () => [],
  SynchronizationMethod: { Incremental: 2 },
}));
vi.mock("@/modules/lsp/lib/transport", () => ({
  TauriLspTransport: class {
    exitInfo = null;
    closed = false;
    constructor() {
      ports.transports.push(this);
    }
    start({ root }: { root: string }) {
      return new Promise<void>((resolve, reject) =>
        ports.starts.push({ root, resolve, reject }),
      );
    }
    close() {
      this.closed = true;
      return ports.deferClose
        ? new Promise<void>((resolve) => ports.closes.push(resolve))
        : Promise.resolve();
    }
  },
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

import {
  acquireDocExtension,
  restartPresetSessions,
  stopPresetSessions,
} from "@/modules/lsp/lib/sessionManager";
import { useLspRuntimeStore } from "@/modules/lsp/lib/runtimeStore";

beforeEach(() => {
  ports.activation = "enabled";
  ports.starts = [];
  ports.transports = [];
  ports.shutdowns = [];
  ports.deferShutdown = false;
  ports.closes = [];
  ports.deferClose = false;
});
afterEach(async () => {
  await stopPresetSessions("server");
});

describe("LSP session admission", () => {
  it("reserves four slots before concurrent different-root spawns finish", async () => {
    const calls = Array.from({ length: 8 }, (_, index) =>
      acquireDocExtension(`/parallel-${index}/file.ts`, "ts"),
    );
    await vi.waitFor(() => expect(ports.starts).toHaveLength(4));
    for (const start of ports.starts) start.resolve();
    const handles = await Promise.all(calls);
    expect(handles.filter(Boolean)).toHaveLength(4);
    expect(ports.starts).toHaveLength(4);
    for (const handle of handles) handle?.release();
  });

  it("coalesces same-root creation into one reserved slot", async () => {
    const calls = Array.from({ length: 8 }, () =>
      acquireDocExtension("/shared-root/file.ts", "ts"),
    );
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    ports.starts[0].resolve();
    const handles = await Promise.all(calls);
    expect(handles.filter(Boolean)).toHaveLength(8);
    for (const handle of handles) handle?.release();
  });

  it("releases failed reservations and rejects late installs after stop", async () => {
    const first = acquireDocExtension("/failed-root/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    ports.starts[0].reject(new Error("synthetic spawn failure"));
    expect(await first).toBeNull();
    const pending = acquireDocExtension("/stopped-root/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(2));
    let stopped = false;
    const stopping = stopPresetSessions("server").then(() => {
      stopped = true;
    });
    await Promise.resolve();
    expect(stopped).toBe(false);
    ports.starts[1].resolve();
    expect(await pending).toBeNull();
    await stopping;
    expect(ports.transports[1].closed).toBe(true);
    const retry = acquireDocExtension("/stopped-root/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(3));
    ports.starts[2].resolve();
    const handle = await retry;
    expect(handle).not.toBeNull();
    handle?.release();
  });

  it("keeps closing sessions reserved until native kill completes", async () => {
    const calls = Array.from({ length: 4 }, (_, index) =>
      acquireDocExtension(`/closing-${index}/file.ts`, "ts"),
    );
    await vi.waitFor(() => expect(ports.starts).toHaveLength(4));
    for (const start of ports.starts) start.resolve();
    const handles = await Promise.all(calls);
    ports.deferShutdown = true;
    ports.deferClose = true;
    const stopping = stopPresetSessions("server");
    await vi.waitFor(() => expect(ports.shutdowns).toHaveLength(4));
    expect(
      await acquireDocExtension("/after-closing/file.ts", "ts"),
    ).toBeNull();
    for (const shutdown of ports.shutdowns) shutdown();
    await vi.waitFor(() => expect(ports.closes).toHaveLength(4));
    expect(
      await acquireDocExtension("/after-closing/file.ts", "ts"),
    ).toBeNull();
    for (const close of ports.closes) close();
    await stopping;
    ports.deferShutdown = false;
    ports.deferClose = false;
    const next = acquireDocExtension("/after-closing/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(5));
    ports.starts[4].resolve();
    const handle = await next;
    expect(handle).not.toBeNull();
    for (const previous of handles) previous?.release();
    handle?.release();
  });

  it("does not install an in-flight spawn after its preset is disabled", async () => {
    const pending = acquireDocExtension("/disabled-root/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    ports.activation = "dismissed";
    ports.starts[0].resolve();
    expect(await pending).toBeNull();
    expect(ports.transports[0].closed).toBe(true);
    expect(
      await acquireDocExtension("/disabled-root/file.ts", "ts"),
    ).toBeNull();
    expect(ports.starts).toHaveLength(1);
  });

  it("restarts open documents after a cancelled spawn finishes closing", async () => {
    const pending = acquireDocExtension("/restart-pending/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    const generation = useLspRuntimeStore.getState().generations.server ?? 0;
    ports.deferClose = true;
    let restarted = false;
    const restarting = restartPresetSessions("server").then(() => {
      restarted = true;
    });
    await Promise.resolve();
    expect(restarted).toBe(false);
    expect(useLspRuntimeStore.getState().generations.server ?? 0).toBe(generation);
    ports.starts[0].resolve();
    await vi.waitFor(() => expect(ports.closes).toHaveLength(1));
    expect(restarted).toBe(false);
    ports.closes[0]();
    expect(await pending).toBeNull();
    await restarting;
    expect(useLspRuntimeStore.getState().generations.server).toBe(generation + 1);
    ports.deferClose = false;
    const retry = acquireDocExtension("/restart-pending/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(2));
    ports.starts[1].resolve();
    const handle = await retry;
    expect(handle).not.toBeNull();
    handle?.release();
  });

  it("reacquires when a preset is re-enabled before its old spawn settles", async () => {
    const pending = acquireDocExtension("/toggle-pending/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    ports.activation = "dismissed";
    ports.preferenceListener?.(
      { lspActivation: { server: "dismissed" } },
      { lspActivation: { server: "enabled" } },
    );
    ports.activation = "enabled";
    ports.preferenceListener?.(
      { lspActivation: { server: "enabled" } },
      { lspActivation: { server: "dismissed" } },
    );
    const retry = acquireDocExtension("/toggle-pending/file.ts", "ts");
    await Promise.resolve();
    expect(ports.starts).toHaveLength(1);
    ports.starts[0].resolve();
    expect(await pending).toBeNull();
    await vi.waitFor(() => expect(ports.starts).toHaveLength(2));
    expect(ports.transports[0].closed).toBe(true);
    ports.starts[1].resolve();
    const handle = await retry;
    expect(handle).not.toBeNull();
    handle?.release();
  });

  it("waits for an existing stop before a concurrent restart completes", async () => {
    const pending = acquireDocExtension("/restart-closing/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(1));
    ports.starts[0].resolve();
    const handle = await pending;
    ports.deferClose = true;
    const stopping = stopPresetSessions("server");
    await vi.waitFor(() => expect(ports.closes).toHaveLength(1));
    let restarted = false;
    const restarting = restartPresetSessions("server").then(() => {
      restarted = true;
    });
    await Promise.resolve();
    expect(restarted).toBe(false);
    expect(ports.closes).toHaveLength(1);
    ports.closes[0]();
    await Promise.all([stopping, restarting]);
    ports.deferClose = false;
    const retry = acquireDocExtension("/restart-closing/file.ts", "ts");
    await vi.waitFor(() => expect(ports.starts).toHaveLength(2));
    ports.starts[1].resolve();
    const nextHandle = await retry;
    expect(nextHandle).not.toBeNull();
    handle?.release();
    nextHandle?.release();
  });
});
