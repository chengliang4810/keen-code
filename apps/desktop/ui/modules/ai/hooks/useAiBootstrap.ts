import { useEffect, useState } from "react";
import { firePendingReviewForSession } from "@/modules/agents/lib/review";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { onKeysChanged } from "@/modules/settings/store";
import { getAllCustomEndpointKeys, getAllKeys } from "../lib/keyring";
import {
  configuredCustomModelIds,
  NO_MODEL_ID,
  selectConfiguredCustomModel,
} from "@/modules/ai/lib/modelSelection";
import { useAgentsStore } from "../store/agentsStore";
import { useChatStore } from "../store/chatStore";

/**
 * Startup wiring for the AI subsystem: loads provider keys (and keeps them in
 * sync), hydrates the preference store and mirrors the default model, hydrates
 * chat/agents stores, and fires any pending review for the active
 * session. Returns the two derived flags the shell needs.
 */
export function useAiBootstrap(): {
  hasComposer: boolean;
  keysLoaded: boolean;
} {
  const setApiKeys = useChatStore((s) => s.setApiKeys);
  const setCustomEndpointKeys = useChatStore((s) => s.setCustomEndpointKeys);
  const setSelectedModelId = useChatStore((s) => s.setSelectedModelId);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const hydrateSessions = useChatStore((s) => s.hydrateSessions);

  useEffect(() => {
    if (activeSessionId) firePendingReviewForSession(activeSessionId);
  }, [activeSessionId]);

  const customEndpoints = usePreferencesStore((s) => s.customEndpoints);
  const hasComposer = configuredCustomModelIds(customEndpoints).length > 0;

  const prefsHydrated = usePreferencesStore((s) => s.hydrated);
  useEffect(() => {
    void import("@/modules/ai/lib/modelCatalogState").then(
      ({ refreshModelCatalog }) => refreshModelCatalog(),
    );
  }, []);
  const [keysLoaded, setKeysLoaded] = useState(false);
  useEffect(() => {
    let alive = true;
    const reload = () => {
      void getAllKeys().then((keys) => {
        if (!alive) return;
        setApiKeys(keys);
        setKeysLoaded(true);
      });
      if (!prefsHydrated) return;
      void getAllCustomEndpointKeys(
        usePreferencesStore.getState().customEndpoints,
      ).then((epKeys) => {
        if (!alive) return;
        setCustomEndpointKeys(epKeys);
      });
    };
    reload();
    const unlistenP = onKeysChanged(reload);
    return () => {
      alive = false;
      void unlistenP.then((fn) => fn());
    };
  }, [setApiKeys, setCustomEndpointKeys, prefsHydrated]);

  // Hydrate the cross-window preference store and mirror the default model
  // into chatStore so the dropdown reflects what the user picked in Settings.
  const initPrefs = usePreferencesStore((s) => s.init);
  const prefDefaultModel = usePreferencesStore((s) => s.defaultModelId);
  useEffect(() => {
    void initPrefs();
  }, [initPrefs]);
  useEffect(() => {
    if (!prefsHydrated) return;
    const current = useChatStore.getState().selectedModelId;
    setSelectedModelId(
      selectConfiguredCustomModel(
        current === NO_MODEL_ID ? prefDefaultModel : current,
        customEndpoints,
      ),
    );
  }, [prefsHydrated, prefDefaultModel, customEndpoints, setSelectedModelId]);

  useEffect(() => {
    void hydrateSessions();
    void useAgentsStore.getState().hydrate();
  }, [hydrateSessions]);

  return { hasComposer, keysLoaded };
}
