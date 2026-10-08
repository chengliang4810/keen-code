import { useEffect, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { Add01Icon, CheckmarkCircle02Icon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { endpointModels, type CustomEndpoint } from "@/modules/ai/config";
import {
  clearCustomEndpointKey,
  getAllCustomEndpointKeys,
  setCustomEndpointKey,
  type CustomEndpointKeys,
} from "@/modules/ai/lib/keyring";
import {
  isConfiguredCustomModel,
  selectConfiguredCustomModel,
} from "@/modules/ai/lib/modelSelection";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { useTranslation } from "@/modules/i18n";
import { usePreferencesStore } from "@/modules/settings/preferences";
import {
  emitKeysChanged,
  setCustomEndpoints,
  setFavoriteModelIds,
  setRecentModelIds,
} from "@/modules/settings/store";
import { CustomProviderDetail } from "@/settings/components/CustomProviderDetail";
import { ProviderIcon } from "@/settings/components/ProviderIcon";
import { SectionHeader } from "@/settings/components/SectionHeader";
import {
  refreshModelCatalog,
  useModelCatalogStore,
} from "@/modules/ai/lib/modelCatalogState";

export function ModelsSection() {
  const tr = useTranslation();
  const catalogLoading = useModelCatalogStore((s) => s.loading);
  const catalogError = useModelCatalogStore((s) => s.error);
  const customEndpoints = usePreferencesStore((s) => s.customEndpoints);
  const [keys, setKeys] = useState<CustomEndpointKeys>({});
  const [selection, setSelection] = useState<string | null>(null);
  const [error, setError] = useState("");
  const saveQueue = useRef(Promise.resolve());
  const keyIds = customEndpoints.map((endpoint) => endpoint.id).join("\0");

  useEffect(() => {
    let disposed = false;
    void getAllCustomEndpointKeys(
      keyIds
        .split("\0")
        .filter(Boolean)
        .map((id) => ({ id })),
    )
      .then((next) => {
        if (!disposed) setKeys(next);
      })
      .catch((e) => {
        if (!disposed) setError(String(e));
      });
    return () => {
      disposed = true;
    };
  }, [keyIds]);

  const persistEndpoints = (
    change: (current: CustomEndpoint[]) => CustomEndpoint[],
  ) => {
    const operation = saveQueue.current
      .catch(() => {})
      .then(async () => {
        setError("");
        const next = change(usePreferencesStore.getState().customEndpoints);
        await setCustomEndpoints(next);
        const favorites = usePreferencesStore.getState().favoriteModelIds;
        const filteredFavorites = favorites.filter((id) =>
          isConfiguredCustomModel(id, next),
        );
        if (filteredFavorites.length !== favorites.length)
          await setFavoriteModelIds(filteredFavorites);
        const recents = usePreferencesStore.getState().recentModelIds;
        const filteredRecents = recents.filter((id) =>
          isConfiguredCustomModel(id, next),
        );
        if (filteredRecents.length !== recents.length)
          await setRecentModelIds(filteredRecents);
        const selected = useChatStore.getState().selectedModelId;
        const nextSelection = selectConfiguredCustomModel(selected, next);
        if (nextSelection !== selected)
          useChatStore.getState().setSelectedModelId(nextSelection);
      });
    saveQueue.current = operation;
    void operation.catch((e) => setError(String(e)));
    return operation;
  };

  const addEndpoint = async () => {
    const endpoint: CustomEndpoint = {
      id: crypto.randomUUID().slice(0, 8),
      name: "",
      baseURL: "",
      modelId: "",
      contextLimit: 128_000,
      models: [],
    };
    await persistEndpoints((current) => [...current, endpoint]);
    setSelection(endpoint.id);
  };
  const removeEndpoint = async (id: string) => {
    await persistEndpoints((current) =>
      current.filter((entry) => entry.id !== id),
    );
    await clearCustomEndpointKey(id);
    setKeys((current) => {
      const next = { ...current };
      delete next[id];
      return next;
    });
    await emitKeysChanged();
  };
  const saveKey = async (id: string, value: string) => {
    await setCustomEndpointKey(id, value);
    setKeys((current) => ({ ...current, [id]: value }));
    await emitKeysChanged();
  };
  const clearKey = async (id: string) => {
    await clearCustomEndpointKey(id);
    setKeys((current) => ({ ...current, [id]: null }));
    await emitKeysChanged();
  };
  const active =
    customEndpoints.find((endpoint) => endpoint.id === selection) ??
    customEndpoints[0];

  return (
    <div className="flex flex-col gap-7">
      <div className="flex flex-wrap items-end justify-between gap-4 max-[900px]:items-start">
        <div className="min-w-0 flex-1 max-[900px]:basis-full">
          <SectionHeader
            title={tr("Models")}
            description={tr(
              "Manage your custom providers and configure the models used in conversations.",
            )}
          />
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            size="sm"
            variant="ghost"
            disabled={catalogLoading}
            onClick={() => void refreshModelCatalog(true)}
          >
            {tr(
              catalogLoading
                ? "Updating model information…"
                : "Update model information",
            )}
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="gap-1.5"
            onClick={() => void addEndpoint().catch(() => {})}
          >
            <HugeiconsIcon icon={Add01Icon} size={14} />
            {tr("Add custom provider")}
          </Button>
        </div>
      </div>
      {catalogError && (
        <p role="alert" className="text-ui-sm text-destructive">
          {tr(
            "Could not update model information. Existing local information is still available: {value0}",
            { value0: catalogError },
          )}
        </p>
      )}
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
      {customEndpoints.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border/60 bg-card/40 px-4 py-8 text-center">
          <p className="text-ui-sm text-muted-foreground">
            {tr("No providers connected yet.")}
          </p>
          <p className="mt-1 text-ui-sm text-muted-foreground">
            {tr(
              'Click "Add custom provider" to configure your model endpoint.',
            )}
          </p>
        </div>
      ) : (
        <div className="grid min-h-140 grid-cols-[192px_minmax(0,1fr)] gap-7 max-[900px]:min-h-0 max-[900px]:grid-cols-1">
          <nav
            aria-label={tr("Providers")}
            className="min-w-0 rounded-lg bg-card/35 p-2 max-[900px]:border-b max-[900px]:border-border/60"
          >
            <p className="px-2 pt-2 pb-2 text-ui-base text-muted-foreground">
              {tr("Custom providers")}
            </p>
            <div className="flex flex-col gap-1">
              {customEndpoints.map((endpoint) => {
                const configured =
                  !!endpoint.baseURL.trim() &&
                  endpointModels(endpoint).some(
                    (model) => model.enabled !== false && model.id.trim(),
                  );
                return (
                  <button
                    key={endpoint.id}
                    type="button"
                    aria-current={
                      active?.id === endpoint.id ? "true" : undefined
                    }
                    onClick={() => setSelection(endpoint.id)}
                    className={cn(
                      "flex min-w-0 items-center gap-2 rounded-md border border-transparent px-2 py-2 text-left text-ui-base transition-colors hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      active?.id === endpoint.id &&
                        "border-border/60 bg-accent/50",
                    )}
                  >
                    <ProviderIcon
                      provider="openai-compatible"
                      size={14}
                      className="shrink-0"
                    />
                    <span
                      className="min-w-0 flex-1 truncate"
                      title={endpoint.name || tr("New provider")}
                    >
                      {endpoint.name || tr("New provider")}
                    </span>
                    <HugeiconsIcon
                      icon={CheckmarkCircle02Icon}
                      size={12}
                      className={
                        configured ? "text-primary" : "text-muted-foreground/35"
                      }
                      aria-label={tr(
                        configured ? "Configured" : "Not configured",
                      )}
                    />
                  </button>
                );
              })}
            </div>
          </nav>
          {active && (
            <div className="min-w-0 py-2">
              <CustomProviderDetail
                key={active.id}
                endpoint={active}
                endpointKey={keys[active.id] ?? null}
                onSaveKey={(value) => saveKey(active.id, value)}
                onClearKey={() => clearKey(active.id)}
                onUpdate={(patch) =>
                  persistEndpoints((current) =>
                    current.map((entry) =>
                      entry.id === active.id ? { ...entry, ...patch } : entry,
                    ),
                  )
                }
                onRemove={() => removeEndpoint(active.id)}
              />
            </div>
          )}
        </div>
      )}
    </div>
  );
}
