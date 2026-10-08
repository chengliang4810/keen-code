import { beforeEach, describe, expect, it, vi } from "vitest";
import { EMPTY_SUBAGENT_CONFIG } from "@/modules/ai/agents/config";
const storage = vi.hoisted(() => ({
  get: vi.fn(),
  reload: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@/modules/ai/lib/agentStateStorage", () => ({
  agentStateStorage: storage,
  serializeAgentState: (() => {
    let queue = Promise.resolve();
    return (action: () => Promise<void>) => {
      const next = queue.then(action);
      queue = next.catch(() => {});
      return next;
    };
  })(),
}));
import { useSubagentsStore } from "@/modules/ai/store/subagentsStore";
beforeEach(() => {
  vi.resetAllMocks();
  useSubagentsStore.setState({
    config: EMPTY_SUBAGENT_CONFIG,
    hydrated: false,
  });
});
describe("persisted subagents", () => {
  it("does not publish a failed save and restores the shared store projection", async () => {
    await useSubagentsStore.getState().hydrate();
    storage.save.mockRejectedValueOnce(new Error("disk full"));
    await expect(
      useSubagentsStore
        .getState()
        .update((current) => ({
          ...current,
          overrides: [{ id: "explore", modelId: "new" }],
        })),
    ).rejects.toThrow("disk full");
    expect(useSubagentsStore.getState().config).toEqual(EMPTY_SUBAGENT_CONFIG);
    expect(storage.set).toHaveBeenLastCalledWith(
      "subagents",
      EMPTY_SUBAGENT_CONFIG,
    );
  });
  it("coalesces hydration and serializes edits without dropping earlier changes", async () => {
    await Promise.all([
      useSubagentsStore.getState().hydrate(),
      useSubagentsStore.getState().hydrate(),
    ]);
    expect(storage.get).toHaveBeenCalledTimes(1);
    await Promise.all([
      useSubagentsStore.getState().update((current) => ({
        ...current,
        overrides: [{ id: "explore", modelId: "model-a" }],
      })),
      useSubagentsStore.getState().update((current) => ({
        ...current,
        overrides: [
          ...current.overrides,
          { id: "general", modelId: "model-b" },
        ],
      })),
    ]);
    expect(useSubagentsStore.getState().config.overrides).toHaveLength(2);
    expect(storage.set).toHaveBeenLastCalledWith(
      "subagents",
      useSubagentsStore.getState().config,
    );
    expect(storage.save).toHaveBeenCalledTimes(2);
  });
  it("blocks writes on corrupt data and permits a later reload", async () => {
    storage.get.mockResolvedValueOnce({ version: 99 });
    await expect(useSubagentsStore.getState().hydrate()).rejects.toThrow();
    await expect(
      useSubagentsStore.getState().update((current) => current),
    ).rejects.toThrow();
    expect(storage.set).not.toHaveBeenCalled();
    await useSubagentsStore.getState().reload();
    expect(useSubagentsStore.getState().hydrated).toBe(true);
  });
});
