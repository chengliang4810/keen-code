import { emit, listen } from "@tauri-apps/api/event";
import { create } from "zustand";
import { toast } from "sonner";
import { t } from "@/modules/i18n/state";
import {
  BUILTIN_AGENTS,
  loadAgents,
  newAgentId,
  saveActiveAgentId,
  saveCustomAgents,
  type Agent,
} from "@/modules/ai/lib/agents";

const CHANGED_EVENT = "rcode://ai-agents-changed";

type AgentsState = {
  hydrated: boolean;
  customAgents: Agent[];
  activeId: string;
  /** All agents, builtin first. */
  all: () => Agent[];
  hydrate: () => Promise<void>;
  reload: () => Promise<void>;
  setActiveId: (id: string) => void;
  upsert: (agent: Agent) => Promise<void>;
  remove: (id: string) => Promise<void>;
};

let initialized = false;
let queue = Promise.resolve();

function enqueue(action: () => Promise<void>): Promise<void> {
  const next = queue.then(action);
  queue = next.catch(() => {});
  return next;
}

function report(error: unknown): void {
  toast.error(t("Could not save agents"), { description: String(error) });
}

function broadcast(): void {
  void emit(CHANGED_EVENT);
}

export const useAgentsStore = create<AgentsState>((set, get) => ({
  hydrated: false,
  customAgents: [],
  activeId: BUILTIN_AGENTS[0].id,
  all: () => [...BUILTIN_AGENTS, ...get().customAgents],
  reload: () =>
    enqueue(async () => {
      const { custom, activeId } = await loadAgents();
      set({ customAgents: custom, activeId, hydrated: true });
    }),
  hydrate: async () => {
    if (initialized) return;
    initialized = true;
    try {
      const { custom, activeId } = await loadAgents();
      set({ customAgents: custom, activeId, hydrated: true });
      await listen(CHANGED_EVENT, () => {
        void enqueue(async () => {
          const fresh = await loadAgents();
          set({ customAgents: fresh.custom, activeId: fresh.activeId });
        }).catch(report);
      });
    } catch (error) {
      initialized = false;
      toast.error(t("Could not load agents"), { description: String(error) });
    }
  },
  setActiveId: (id) => {
    set({ activeId: id });
    void saveActiveAgentId(id).then(broadcast).catch(report);
  },
  upsert: (agent) =>
    enqueue(async () => {
      if (agent.builtIn) return;
      if (!get().hydrated) throw new Error(t("Reload agents before saving."));
      const list = get().customAgents;
      const idx = list.findIndex((a) => a.id === agent.id);
      const next =
        idx === -1
          ? [...list, agent]
          : list.map((a) => (a.id === agent.id ? agent : a));
      await saveCustomAgents(next);
      set({ customAgents: next });
      broadcast();
    }),
  remove: (id) =>
    enqueue(async () => {
      if (!get().hydrated) throw new Error(t("Reload agents before saving."));
      const list = get().customAgents.filter((a) => a.id !== id);
      await saveCustomAgents(list);
      set({ customAgents: list });
      let active = get().activeId;
      if (active === id) {
        active = BUILTIN_AGENTS[0].id;
        set({ activeId: active });
        await saveActiveAgentId(active);
      }
      broadcast();
    }),
}));

export { newAgentId };
