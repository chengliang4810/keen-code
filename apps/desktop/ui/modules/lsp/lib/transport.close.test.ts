import { TauriLspTransport } from "@/modules/lsp/lib/transport";
import { beforeEach, expect, it, vi } from "vitest";

const ports = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: ports.invoke,
  Channel: class {
    onmessage: unknown;
  },
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
}));

beforeEach(() => {
  ports.invoke.mockReset();
});

it("coalesces native kill and resolves close only after IPC completes", async () => {
  let finishKill: (() => void) | undefined;
  ports.invoke.mockImplementation((command: string) =>
    command === "lsp_spawn"
      ? Promise.resolve(17)
      : new Promise<void>((resolve) => {
          finishKill = resolve;
        }),
  );
  const transport = new TauriLspTransport();
  await transport.start({ command: "server", args: [], root: "/test" });
  const closing = transport.close();
  expect(transport.close()).toBe(closing);
  let complete = false;
  void closing.then(() => {
    complete = true;
  });
  await Promise.resolve();
  expect(complete).toBe(false);
  expect(
    ports.invoke.mock.calls.filter(([command]) => command === "lsp_kill"),
  ).toEqual([["lsp_kill", { id: 17 }]]);
  finishKill?.();
  await closing;
  expect(complete).toBe(true);
});

it("reports a native kill failure without an unhandled rejection", async () => {
  ports.invoke.mockImplementation((command: string) =>
    command === "lsp_spawn"
      ? Promise.resolve(18)
      : Promise.reject(new Error("synthetic native kill error")),
  );
  const transport = new TauriLspTransport();
  const onError = vi.fn();
  transport.onError(onError);
  await transport.start({ command: "server", args: [], root: "/test" });
  await transport.close();
  expect(onError).toHaveBeenCalledWith(
    expect.objectContaining({ message: "Error: synthetic native kill error" }),
  );
  expect(
    ports.invoke.mock.calls.filter(([command]) => command === "lsp_kill"),
  ).toHaveLength(1);
});
