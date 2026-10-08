import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  installModelCatalog,
  type ModelCatalogSnapshot,
} from "@/modules/ai/lib/modelCatalog";

const FRESH_MS = 60 * 60 * 1000;
export const useModelCatalogStore = create<{
  revision: number;
  loading: boolean;
  fetchedAt: number | null;
  error: string;
}>(() => ({ revision: 0, loading: false, fetchedAt: null, error: "" }));

let hydrated = false;
let pending: Promise<void> | undefined;
let lastAttempt = 0;

function publish(snapshot: ModelCatalogSnapshot): void {
  installModelCatalog(snapshot);
  useModelCatalogStore.setState((state) => ({
    revision: state.revision + 1,
    fetchedAt: snapshot.fetchedAt,
    error: "",
  }));
}

export function refreshModelCatalog(force = false): Promise<void> {
  if (pending) return pending;
  if (!force && hydrated && Date.now() - lastAttempt < 300_000)
    return Promise.resolve();
  useModelCatalogStore.setState({ loading: true });
  pending = (async () => {
    try {
      if (!hydrated) {
        try {
          const cached = await invoke<ModelCatalogSnapshot | null>(
            "model_catalog_load",
          );
          if (cached) publish(cached);
        } catch {
          // A corrupt cache can be replaced by a validated download.
        }
        hydrated = true;
      }
      const fetchedAt = useModelCatalogStore.getState().fetchedAt;
      if (!force && fetchedAt && Date.now() - fetchedAt < FRESH_MS) return;
      const snapshot = await invoke<ModelCatalogSnapshot>(
        "model_catalog_refresh",
        { force },
      );
      publish(snapshot);
    } catch (error) {
      useModelCatalogStore.setState({ error: String(error) });
    } finally {
      lastAttempt = Date.now();
      useModelCatalogStore.setState({ loading: false });
    }
  })().finally(() => {
    pending = undefined;
  });
  return pending;
}
