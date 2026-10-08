import { LazyStore } from "@/lib/storage";

export const agentStateStorage = new LazyStore("agents", {
  defaults: {},
  autoSave: false,
});

let queue = Promise.resolve();
export function serializeAgentState(
  action: () => Promise<void>,
): Promise<void> {
  const next = queue.then(action);
  queue = next.catch(() => {});
  return next;
}
