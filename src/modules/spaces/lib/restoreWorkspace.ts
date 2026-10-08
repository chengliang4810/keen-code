import { findActiveSpace } from "@/modules/spaces/lib/activeSpace";
import { hydrateTabs } from "@/modules/spaces/lib/serialize";
import type { LoadedSpaces } from "@/modules/spaces/lib/store";

export function restoreProjectWorkspace(
  loaded: LoadedSpaces,
  allocId: () => number,
) {
  const { spaces, states } = loaded;
  const activeId = findActiveSpace(spaces, loaded.activeId)?.id ?? null;
  const tabs = spaces
    .filter((space) => !space.removed)
    .flatMap((space) => {
      const state = states.get(space.id);
      return state ? hydrateTabs(state.tabs, space.id, allocId) : [];
    });
  const initialActiveIndex = Object.fromEntries(
    [...states].map(([id, state]) => [id, state.activeTabIndex]),
  );
  return { activeId, tabs, initialActiveIndex };
}
