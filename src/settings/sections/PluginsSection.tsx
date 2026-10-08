import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  extensionApi,
  type InstalledPlugin,
} from "@/modules/ai/lib/extensions";
import { installedPluginId } from "@/modules/ai/lib/pluginUi";
import { native } from "@/modules/ai/lib/native";
import { LOCAL_WORKSPACE } from "@/modules/workspace";
import { useSettingsOverlay } from "@/modules/settings/settingsOverlay";
import { useTranslation } from "@/modules/i18n";
import { SectionHeader } from "@/settings/components/SectionHeader";
import {
  PluginActionDialog,
  PluginDetailsDialog,
} from "@/settings/components/PluginDialogs";

export function PluginsSection() {
  const tr = useTranslation();
  const [plugins, setPlugins] = useState<InstalledPlugin[]>([]);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [adding, setAdding] = useState(false);
  const [source, setSource] = useState("");
  const [pluginId, setPluginId] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [remove, setRemove] = useState<string | null>(null);
  const pending = useRef(false);
  const reload = async () => setPlugins((await extensionApi.plugins()).plugins);
  useEffect(() => {
    let active = true;
    void extensionApi
      .plugins()
      .then((next) => {
        if (active) setPlugins(next.plugins);
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      })
      .finally(() => {
        if (active) setBusy(false);
      });
    return () => {
      active = false;
    };
  }, []);
  const run = async (operation: () => Promise<unknown>) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      await operation();
      await reload();
    } catch (cause) {
      setError(String(cause));
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  const visible = plugins.filter(
    (plugin) =>
      (filter === "all" || plugin.enabled === (filter === "enabled")) &&
      installedPluginId(plugin)
        .toLowerCase()
        .includes(query.trim().toLowerCase()),
  );
  return (
    <div className="flex flex-col gap-5" aria-busy={busy}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <SectionHeader title={tr("Installed plugins")} />
        <div className="flex flex-wrap gap-2">
          <Button
            size="sm"
            variant="outline"
            disabled={busy || !plugins.some((plugin) => plugin.source)}
            onClick={() => void run(() => extensionApi.update())}
          >
            {tr("Update all")}
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => setAdding(!adding)}
          >
            {tr(adding ? "Cancel" : "Install plugin")}
          </Button>
          <Button
            size="sm"
            onClick={() => useSettingsOverlay.getState().select("market")}
          >
            {tr("Browse plugin marketplace")}
          </Button>
        </div>
      </div>
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
      {adding && (
        <form
          className="flex flex-col gap-3 rounded-lg border border-border/60 p-4"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              await extensionApi.install(source, pluginId.trim(), {});
              setAdding(false);
              setSelected(pluginId.trim());
              setPluginId("");
              setSource("");
            });
          }}
        >
          <div className="flex gap-2">
            <Input
              aria-label={tr("Plugin folder")}
              value={source}
              readOnly
              placeholder="D:/plugins/my-plugin"
            />
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() =>
                void native
                  .pickProjectDirectory(tr("Select plugin folder"))
                  .then(async (path) => {
                    if (path)
                      setSource(
                        await native.workspaceAuthorize(path, LOCAL_WORKSPACE),
                      );
                  })
                  .catch((cause) => setError(String(cause)))
              }
            >
              {tr("Browse")}
            </Button>
          </div>
          <Input
            aria-label={tr("Plugin ID")}
            placeholder="my-plugin@local"
            value={pluginId}
            onChange={(event) => setPluginId(event.target.value)}
            spellCheck={false}
            disabled={busy}
          />
          <Button
            type="submit"
            className="self-start"
            disabled={busy || !source || !pluginId.includes("@")}
          >
            {tr("Install plugin")}
          </Button>
        </form>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <fieldset className="flex gap-1" aria-label={tr("Filter plugins")}>
          {[
            ["all", "All"],
            ["enabled", "Enabled"],
            ["disabled", "Disabled"],
          ].map(([id, label]) => (
            <Button
              key={id}
              variant={filter === id ? "secondary" : "ghost"}
              size="sm"
              aria-pressed={filter === id}
              onClick={() => setFilter(id)}
            >
              {tr(label)}
            </Button>
          ))}
        </fieldset>
        <Input
          type="search"
          aria-label={tr("Search plugins…")}
          placeholder="code-review"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          className="ml-auto max-w-64"
        />
      </div>
      {busy && plugins.length === 0 && (
        <p role="status" className="text-ui-sm text-muted-foreground">
          {tr("Loading plugins…")}
        </p>
      )}
      {!busy && !visible.length && (
        <p className="py-10 text-center text-ui-sm text-muted-foreground">
          {tr(
            plugins.length
              ? "No plugins match this filter."
              : "No plugins installed yet",
          )}
        </p>
      )}
      <ul className="divide-y divide-border/60">
        {visible.map((plugin) => {
          const id = installedPluginId(plugin);
          return (
            <li key={id} className="flex flex-wrap items-center gap-3 py-4">
              <div className="min-w-40 flex-1">
                <p className="text-ui-base font-medium">
                  {plugin.id.plugin}{" "}
                  <span className="font-normal text-muted-foreground">
                    {plugin.version}
                  </span>
                </p>
                <p className="text-ui-xs text-muted-foreground">
                  {plugin.id.marketplace}
                </p>
              </div>
              <Switch
                aria-label={`${tr("Enable plugin")} ${id}`}
                checked={plugin.enabled}
                disabled={busy}
                onCheckedChange={(enabled) =>
                  void run(() => extensionApi.enable(id, enabled))
                }
              />
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                onClick={() => setSelected(id)}
              >
                {tr("Configure")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={busy || !plugin.source}
                onClick={() => void run(() => extensionApi.update(id))}
              >
                {tr("Update")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                onClick={() => setRemove(id)}
              >
                {tr("Uninstall")}
              </Button>
            </li>
          );
        })}
      </ul>
      {selected && (
        <PluginDetailsDialog
          key={selected}
          pluginId={selected}
          onClose={() => setSelected(null)}
          onSaved={reload}
        />
      )}
      {remove && (
        <PluginActionDialog
          title="Uninstall plugin"
          target={remove}
          busy={busy}
          error={error}
          onClose={() => setRemove(null)}
          onConfirm={() =>
            void run(async () => {
              await extensionApi.uninstall(remove);
              setRemove(null);
            })
          }
        />
      )}
    </div>
  );
}
