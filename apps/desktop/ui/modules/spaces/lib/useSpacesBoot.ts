import { native } from "@/modules/ai/lib/native";
import type { Tab } from "@/modules/tabs";
import { DEFAULT_SPACE_ID } from "@/modules/tabs/lib/useTabs";
import { isLeaf, type PaneNode } from "@/modules/terminal/lib/panes";
import type { WorkspaceEnv } from "@/modules/workspace";
import { useEffect, useRef } from "react";
import { activeSpaceEnv, freshTabCwd } from "./activeSpace";
import { freshTerminalTab } from "@/modules/spaces/lib/serialize";
import { loadAll } from "@/modules/spaces/lib/store";
import { restoreProjectWorkspace } from "@/modules/spaces/lib/restoreWorkspace";
import { useSpaces } from "./useSpaces";

type Params = {
  ready: boolean;
  launchCwd: string | null;
  home: string | null;
  allocId: () => number;
  replaceTabs: (tabs: Tab[], activeId: number) => void;
  markBooted: () => void;
  setActiveSpaceForNewTabs: (id: string) => void;
  adoptWorkspaceEnv: (env: WorkspaceEnv) => Promise<string | null>;
};

function uniqueCwds(tabs: Tab[]): string[] {
  const set = new Set<string>();
  const walk = (n: PaneNode) => {
    if (isLeaf(n)) {
      if (n.cwd) set.add(n.cwd);
      return;
    }
    for (const c of n.children) walk(c);
  };
  for (const t of tabs) if (t.kind === "terminal") walk(t.paneTree);
  return [...set];
}

export function useSpacesBoot({
  ready,
  launchCwd,
  home,
  allocId,
  replaceTabs,
  markBooted,
  setActiveSpaceForNewTabs,
  adoptWorkspaceEnv,
}: Params) {
  const done = useRef(false);
  const loadAttempt = useSpaces((state) => state.loadAttempt);
  const lastAttempt = useRef(-1);

  useEffect(() => {
    if (
      !ready ||
      useSpaces.getState().hydrated ||
      (done.current && lastAttempt.current === loadAttempt)
    )
      return;
    done.current = true;
    lastAttempt.current = loadAttempt;
    let cancelled = false;
    useSpaces.getState().setLoadState(true);

    void (async () => {
      try {
        const loaded = await loadAll(loadAttempt > 0);
        if (cancelled) return;
        const { spaces, states } = loaded;
        const {
          activeId: active,
          tabs: restored,
          initialActiveIndex,
        } = restoreProjectWorkspace(loaded, allocId);
        setActiveSpaceForNewTabs(active ?? DEFAULT_SPACE_ID);
        if (!active) {
          useSpaces.getState().hydrate(spaces, null, initialActiveIndex);
          replaceTabs(restored, -1);
          return;
        }

        // Apply the space's env+home before the fresh-tab fallback and spawns
        // below; env is set synchronously so cwd resolution picks WSL vs local.
        const env = activeSpaceEnv(spaces, active);
        const restoredHome = await adoptWorkspaceEnv(env);
        if (cancelled) return;

        // 仅没有存档的新项目补初始标签，已保存的空工具区保持为空。
        if (
          spaces.some((space) => space.id === active && !space.removed) &&
          !states.has(active) &&
          !restored.some((t) => t.spaceId === active)
        ) {
          const cwd = freshTabCwd(env, restoredHome, launchCwd, home);
          restored.push(freshTerminalTab(active, cwd, allocId));
        }

        await Promise.allSettled(
          uniqueCwds(restored).map((cwd) => native.workspaceAuthorize(cwd)),
        );
        if (cancelled) return;

        useSpaces
          .getState()
          .hydrate(
            spaces,
            spaces.some((space) => space.id === active && !space.removed)
              ? active
              : null,
            initialActiveIndex,
          );

        const inActive = restored.filter((t) => t.spaceId === active);
        const idx = states.get(active)?.activeTabIndex ?? 0;
        const activeTab = inActive[idx] ?? inActive[0];
        replaceTabs(restored, activeTab?.id ?? -1);
      } catch (e) {
        if (!cancelled)
          useSpaces
            .getState()
            .setLoadState(false, e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) markBooted();
      }
    })();
    return () => {
      cancelled = true;
      done.current = false;
    };
  }, [
    ready,
    launchCwd,
    home,
    allocId,
    replaceTabs,
    markBooted,
    setActiveSpaceForNewTabs,
    adoptWorkspaceEnv,
    loadAttempt,
  ]);
}
