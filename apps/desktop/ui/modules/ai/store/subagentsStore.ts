import { create } from "zustand";
import {
  agentStateStorage as storage,
  serializeAgentState as enqueue,
} from "@/modules/ai/lib/agentStateStorage";
import {
  EMPTY_SUBAGENT_CONFIG,
  parseSubagentConfig,
  type SubagentConfig,
} from "@/modules/ai/agents/config";

let hydration: Promise<void> | undefined;
type State = {
  config: SubagentConfig;
  hydrated: boolean;
  hydrate: () => Promise<void>;
  reload: () => Promise<void>;
  update: (edit: (current: SubagentConfig) => SubagentConfig) => Promise<void>;
};
export const useSubagentsStore = create<State>((set, get) => ({
  config: EMPTY_SUBAGENT_CONFIG,
  hydrated: false,
  reload: () =>
    enqueue(async () => {
      set({ hydrated: false });
      await storage.reload();
      const config = parseSubagentConfig(await storage.get("subagents"));
      set({ config, hydrated: true });
    }),
  hydrate: () => {
    if (get().hydrated) return Promise.resolve();
    if (!hydration)
      hydration = get()
        .reload()
        .finally(() => {
          hydration = undefined;
        });
    return hydration;
  },
  update: (edit) =>
    enqueue(async () => {
      if (!get().hydrated) throw new Error("Reload agents before saving.");
      const config = parseSubagentConfig(edit(get().config));
      try {
        await storage.set("subagents", config);
        await storage.save();
      } catch (error) {
        try {
          await storage.set("subagents", get().config);
        } catch {
          set({ hydrated: false });
        }
        throw error;
      }
      set({ config });
    }),
}));
