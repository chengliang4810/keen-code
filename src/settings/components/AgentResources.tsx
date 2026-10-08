import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  extensionApi,
  type AgentResource,
  type AgentResources,
} from "@/modules/ai/lib/extensions";
import { useTranslation } from "@/modules/i18n";

export function useAgentResources() {
  const [data, setData] = useState<AgentResources | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const reload = useCallback(async () => {
    const current = ++generation.current;
    setError("");
    setBusy(true);
    try {
      const data = await extensionApi.userResources();
      if (generation.current === current) setData(data);
    } catch (e) {
      if (generation.current === current) setError(String(e));
    } finally {
      if (generation.current === current) setBusy(false);
    }
  }, []);
  useEffect(() => {
    void reload();
    return () => {
      generation.current += 1;
    };
  }, [reload]);
  return {
    data,
    busy,
    error,
    reload,
  };
}

export function ResourceList({
  entries,
  empty,
  inactive = false,
}: {
  entries: AgentResource[];
  empty: string;
  inactive?: boolean;
}) {
  const tr = useTranslation();
  const [query, setQuery] = useState("");
  const filtered = entries.filter((entry) =>
    `${entry.name} ${entry.description}`
      .toLowerCase()
      .includes(query.trim().toLowerCase()),
  );
  return (
    <div className="flex flex-col gap-4">
      {entries.length > 0 && (
        <Input
          aria-label={tr("Search resources")}
          placeholder={tr("Search resources")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="h-8 max-w-80"
        />
      )}
      {filtered.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border/60 px-4 py-8 text-center text-ui-sm text-muted-foreground">
          {tr(entries.length ? "No matching resources." : empty)}
        </div>
      ) : (
        <ul className="divide-y divide-border/60 rounded-lg border border-border/60">
          {filtered.map((entry) => (
            <li
              key={`${entry.source}:${entry.name}`}
              className="flex items-start gap-3 px-4 py-4"
            >
              <div className="min-w-0 flex-1 space-y-1">
                <p className="break-all text-ui-base font-medium">
                  {entry.name}
                </p>
                {entry.description && (
                  <p className="break-words text-ui-sm text-muted-foreground">
                    {entry.description}
                  </p>
                )}
                {entry.path && (
                  <p className="break-all font-mono text-ui-xs text-muted-foreground">
                    {entry.path}
                  </p>
                )}
              </div>
              <div className="flex shrink-0 flex-col items-end gap-1.5">
                <Badge variant="secondary">
                  {tr(
                    entry.source === "project"
                      ? "Project"
                      : entry.source === "user"
                        ? "User"
                        : "Plugin",
                  )}
                </Badge>
                <span className="text-ui-xs text-muted-foreground">
                  {tr(
                    inactive
                      ? "Declared only"
                      : entry.enabled
                        ? "Enabled"
                        : "Disabled",
                  )}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function ResourceFeedback({
  resources,
}: {
  resources: ReturnType<typeof useAgentResources>;
}) {
  const tr = useTranslation();
  return (
    <>
      {resources.busy && (
        <p role="status" className="text-ui-sm text-muted-foreground">
          {tr("Loading…")}
        </p>
      )}
      {resources.error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {resources.error}
        </p>
      )}
      {resources.data?.truncated && (
        <p className="text-ui-sm text-muted-foreground">
          {tr("Showing the first 500 resources in each category.")}
        </p>
      )}
      {resources.data?.diagnostics.map((message) => (
        <p key={message} className="text-ui-sm text-muted-foreground">
          {message}
        </p>
      ))}
    </>
  );
}

export function ResourceRefresh({
  resources,
}: {
  resources: ReturnType<typeof useAgentResources>;
}) {
  const tr = useTranslation();
  return (
    <Button
      size="sm"
      variant="outline"
      className="h-7"
      disabled={resources.busy}
      onClick={() => void resources.reload()}
    >
      {tr("Refresh")}
    </Button>
  );
}

export function PluginCommandResources() {
  const tr = useTranslation();
  const resources = useAgentResources();
  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h3 className="text-ui-base font-medium">{tr("Plugin commands")}</h3>
        <ResourceRefresh resources={resources} />
      </div>
      <ResourceFeedback resources={resources} />
      {resources.data && (
        <ResourceList
          entries={resources.data.commands}
          empty="No matching resources."
        />
      )}
    </section>
  );
}
