import { useEffect, useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  InformationCircleIcon,
  MoreHorizontalIcon,
} from "@hugeicons/core-free-icons";
import { useTranslation } from "@/modules/i18n";
import {
  effectiveCustomModel,
  endpointModels,
  getProvider,
  type CustomEndpoint,
  type CustomModel,
  type MessageProtocol,
} from "@/modules/ai/config";
import { ProviderIcon } from "@/settings/components/ProviderIcon";
import { ProviderKeyCard } from "@/settings/components/ProviderKeyCard";
import { CustomModelDialog } from "@/settings/components/CustomModelDialog";
import { useModelCatalogStore } from "@/modules/ai/lib/modelCatalogState";
import { ProviderModelList } from "@/settings/components/ProviderModelList";
import { testProviderModel } from "@/settings/lib/testProviderModel";

export function CustomProviderDetail({
  endpoint,
  endpointKey,
  onSaveKey,
  onClearKey,
  onUpdate,
  onRemove,
}: {
  endpoint: CustomEndpoint;
  endpointKey: string | null;
  onSaveKey: (value: string) => Promise<void>;
  onClearKey: () => Promise<void>;
  onUpdate: (patch: Partial<CustomEndpoint>) => Promise<void>;
  onRemove: () => Promise<void>;
}) {
  const tr = useTranslation();
  useModelCatalogStore((s) => s.revision);
  const urlId = useId();
  const [name, setName] = useState(endpoint.name);
  const [url, setUrl] = useState(endpoint.baseURL);
  const [renaming, setRenaming] = useState(false);
  const [editing, setEditing] = useState<{ model?: CustomModel } | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [testStatus, setTestStatus] = useState<
    "idle" | "testing" | "ok" | "fail"
  >("idle");
  const testGeneration = useRef(0);
  const models = endpointModels(endpoint);
  useEffect(() => {
    setName(endpoint.name);
  }, [endpoint.name]);
  useEffect(() => {
    setUrl(endpoint.baseURL);
  }, [endpoint.baseURL]);
  useEffect(
    () => () => {
      testGeneration.current += 1;
    },
    [],
  );
  const run = async (operation: () => Promise<void>) => {
    setError("");
    setSaving(true);
    try {
      await operation();
    } catch (e) {
      setError(e instanceof Error ? tr(e.message) : String(e));
      throw e;
    } finally {
      setSaving(false);
    }
  };
  const saveModels = (next: CustomModel[]) => {
    const primary =
      next.find((m) => m.id === endpoint.modelId) ??
      next.find((m) => m.enabled !== false) ??
      next[0];
    return run(() =>
      onUpdate({
        models: next,
        modelId: primary?.id ?? "",
        contextLimit: primary
          ? effectiveCustomModel(primary, endpoint.baseURL).contextLimit
          : 128_000,
      }),
    );
  };
  const test = async () => {
    const generation = ++testGeneration.current;
    setTestStatus("testing");
    try {
      const status = await invoke<number>("lm_ping", { baseUrl: url.trim() });
      if (generation === testGeneration.current)
        setTestStatus(status > 0 ? "ok" : "fail");
    } catch {
      if (generation === testGeneration.current) setTestStatus("fail");
    }
  };
  const saveUrl = async () => {
    const value = url.trim();
    if (value === endpoint.baseURL) return;
    if (value) {
      try {
        const parsed = new URL(value);
        if (
          !["http:", "https:"].includes(parsed.protocol) ||
          parsed.username ||
          parsed.password ||
          parsed.hash
        )
          throw new Error();
      } catch {
        setError(
          tr("Enter an HTTP or HTTPS URL without embedded credentials."),
        );
        return;
      }
    }
    await run(() => onUpdate({ baseURL: value })).catch(() => {});
  };
  return (
    <div className="flex min-w-0 flex-col gap-6">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <ProviderIcon provider="openai-compatible" size={20} />
          {renaming ? (
            <Input
              aria-label={tr("Provider name")}
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              onBlur={() => {
                void run(() => onUpdate({ name: name.trim() }))
                  .then(() => setRenaming(false))
                  .catch(() => {});
              }}
              className="h-8 min-w-0 text-ui-base font-semibold"
            />
          ) : (
            <h3 className="truncate text-ui-lg font-semibold">
              {endpoint.name || tr("New provider")}
            </h3>
          )}
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              size="icon"
              variant="ghost"
              className="size-7"
              disabled={saving}
              aria-label={tr("Provider actions")}
            >
              <HugeiconsIcon icon={MoreHorizontalIcon} size={16} />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={() => setRenaming(true)}>
              {tr("Rename provider")}
            </DropdownMenuItem>
            <DropdownMenuItem
              className="text-destructive"
              onSelect={() => void run(onRemove).catch(() => {})}
            >
              {tr("Remove provider")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <div className="flex flex-col gap-1.5">
        <label htmlFor={urlId} className="text-ui-base text-muted-foreground">
          {tr("Base URL")}
        </label>
        <div className="flex gap-2">
          <Input
            id={urlId}
            value={url}
            onChange={(e) => {
              setUrl(e.target.value);
              testGeneration.current += 1;
              setTestStatus("idle");
            }}
            onBlur={() => void saveUrl()}
            placeholder="https://api.example.com/v1"
            autoComplete="off"
            spellCheck={false}
            className="h-9 min-w-0 flex-1 font-mono"
          />
          <Button
            size="sm"
            variant="outline"
            disabled={!url.trim() || saving || testStatus === "testing"}
            onClick={() => void test()}
            className="h-9"
          >
            {tr(testStatus === "testing" ? "Testing…" : "Test")}
          </Button>
        </div>
        {testStatus === "ok" && (
          <p role="status" className="text-ui-xs text-muted-foreground">
            {tr("Server reachable")}
          </p>
        )}
        {testStatus === "fail" && (
          <p role="alert" className="text-ui-xs text-destructive">
            {tr("Could not reach the server")}
          </p>
        )}
      </div>
      <div className="flex flex-col gap-1.5">
        <span className="text-ui-base text-muted-foreground">
          {tr("API format")}
        </span>
        <Select
          value={endpoint.protocol ?? "chat_completions"}
          disabled={saving}
          onValueChange={(protocol) =>
            void run(() =>
              onUpdate({ protocol: protocol as MessageProtocol }),
            ).catch(() => {})
          }
        >
          <SelectTrigger aria-label={tr("API format")} className="h-9 w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="chat_completions">
              OpenAI Chat Completions
            </SelectItem>
            <SelectItem value="responses">OpenAI Responses</SelectItem>
            <SelectItem value="messages">Anthropic Messages</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <div className="space-y-1.5">
        <p className="text-ui-base text-muted-foreground">{tr("API key")}</p>
        <ProviderKeyCard
          provider={{
            ...getProvider("openai-compatible"),
            label: endpoint.name || tr("New provider"),
          }}
          currentKey={endpointKey}
          onSave={onSaveKey}
          onClear={onClearKey}
          embedded
          fieldsOnly
        />
      </div>
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
      <section>
        <div className="mb-1 flex flex-wrap items-center justify-between gap-3">
          <h3 className="text-ui-base font-normal text-muted-foreground">
            {tr("Model list")}
          </h3>
          <Button
            variant="secondary"
            className="rounded-lg"
            disabled={saving}
            onClick={() => setEditing({})}
          >
            <HugeiconsIcon icon={Add01Icon} className="size-4" />
            {tr("Add model")}
          </Button>
        </div>
        {models.length ? (
          <ProviderModelList
            models={models}
            baseURL={endpoint.baseURL}
            testScope={`${endpoint.baseURL}:${endpoint.protocol ?? "chat_completions"}`}
            disabled={saving}
            onEdit={(model) => setEditing({ model })}
            onChange={(next) => void saveModels(next).catch(() => {})}
            onTest={(model, signal) =>
              testProviderModel({
                modelId: model.id,
                baseURL: endpoint.baseURL,
                protocol: endpoint.protocol ?? "chat_completions",
                apiKey: endpointKey,
                signal,
              })
            }
          />
        ) : (
          <p className="mt-1 flex h-12 items-center gap-2 rounded-lg border border-dashed border-border px-4 text-left text-ui-base text-muted-foreground">
            <HugeiconsIcon
              icon={InformationCircleIcon}
              className="size-4 shrink-0"
            />
            {tr("No models yet. Add the model ID provided by your service.")}
          </p>
        )}
      </section>
      {editing && (
        <CustomModelDialog
          baseURL={endpoint.baseURL}
          model={editing.model}
          models={models}
          onClose={() => setEditing(null)}
          onSave={(next) =>
            saveModels(
              editing.model
                ? models.map((m) => (m.id === editing.model?.id ? next : m))
                : [...models, next],
            )
          }
        />
      )}
    </div>
  );
}
