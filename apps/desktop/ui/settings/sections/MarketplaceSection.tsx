import { useEffect, useRef, useState } from "react";
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
  extensionApi,
  type AvailablePlugin,
  type MarketplaceCatalog,
} from "@/modules/ai/lib/extensions";
import {
  filterMarketplace,
  marketplaceSourceDirectory,
} from "@/modules/ai/lib/pluginUi";
import { native } from "@/modules/ai/lib/native";
import { LOCAL_WORKSPACE } from "@/modules/workspace";
import { useSettingsOverlay } from "@/modules/settings/settingsOverlay";
import { useTranslation } from "@/modules/i18n";
import { SectionHeader } from "@/settings/components/SectionHeader";
import {
  PluginActionDialog,
  PluginDetailsDialog,
} from "@/settings/components/PluginDialogs";

const PAGE_SIZE = 20;
export function MarketplaceSection() {
  const tr = useTranslation();
  const [catalog, setCatalog] = useState<MarketplaceCatalog>({
    initialized: false,
    sources: [],
    plugins: [],
  });
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [market, setMarket] = useState("");
  const [page, setPage] = useState(0);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [source, setSource] = useState("");
  const [remove, setRemove] = useState<string | null>(null);
  const [install, setInstall] = useState<AvailablePlugin | null>(null);
  const [configure, setConfigure] = useState<string | null>(null);
  const pending = useRef(false);
  const load = async () => {
    const next = await extensionApi.marketplace();
    setCatalog(next);
    setMarket((current) =>
      next.sources.some((source) => source.name === current) ? current : "",
    );
  };
  useEffect(() => {
    let active = true;
    void (async () => {
      let next = await extensionApi.marketplace();
      if (active) setCatalog(next);
      if (!next.initialized) {
        const errors = await extensionApi.initializeMarketplace();
        next = await extensionApi.marketplace();
        if (active) setError(errors.join("\n"));
      }
      if (active) setCatalog(next);
    })()
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
      await load();
    } catch (cause) {
      setError(String(cause));
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  const refresh = (name: string | null = null, restoreDefault = false) =>
    run(async () =>
      setError(
        (await extensionApi.refreshMarketplace(name, restoreDefault)).join(
          "\n",
        ),
      ),
    );
  const filtered = filterMarketplace(catalog.plugins, query, market);
  const pages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const currentPage = Math.min(page, pages - 1);
  const visible = filtered.slice(
    currentPage * PAGE_SIZE,
    (currentPage + 1) * PAGE_SIZE,
  );
  return (
    <div className="flex flex-col gap-5" aria-busy={busy}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <SectionHeader title={tr("Plugin marketplace")} />
        <div className="flex gap-2">
          <Button
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void refresh(null, true)}
          >
            {tr(busy ? "Refreshing…" : "Refresh catalog")}
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => setSourcesOpen(!sourcesOpen)}
            aria-expanded={sourcesOpen}
          >
            {tr("Marketplace sources")}
          </Button>
        </div>
      </div>
      {error && (
        <p
          role="alert"
          className="whitespace-pre-line text-ui-sm text-destructive"
        >
          {error}
        </p>
      )}
      {sourcesOpen && (
        <section
          aria-label={tr("Marketplace sources")}
          className="rounded-lg border border-border/60 p-4"
        >
          <form
            className="flex flex-wrap gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void run(async () => {
                const directory = marketplaceSourceDirectory(source.trim());
                if (directory)
                  await native.workspaceAuthorize(directory, LOCAL_WORKSPACE);
                await extensionApi.addMarketplace(source.trim());
                setSource("");
              });
            }}
          >
            <Input
              aria-label={tr("Source path or URL")}
              placeholder="github:anthropics/claude-plugins-official"
              className="min-w-48 flex-1"
              value={source}
              disabled={busy}
              onChange={(event) => setSource(event.target.value)}
              spellCheck={false}
              autoComplete="off"
            />
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() =>
                void native
                  .pickProjectDirectory(tr("Source path or URL"))
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
            <Button type="submit" disabled={busy || !source.trim()}>
              {tr("Add source")}
            </Button>
          </form>
          {!catalog.sources.length && (
            <p className="pt-4 text-ui-sm text-muted-foreground">
              {tr("No marketplace sources configured")}
            </p>
          )}
          <ul className="mt-3 divide-y divide-border/60">
            {catalog.sources.map((entry) => (
              <li key={entry.name} className="flex items-center gap-2 py-3">
                <div className="min-w-0 flex-1">
                  <p className="text-ui-sm font-medium">{entry.name}</p>
                  <p
                    className="truncate text-ui-xs text-muted-foreground"
                    title={entry.source}
                  >
                    {entry.source}
                  </p>
                </div>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => void refresh(entry.name)}
                >
                  {tr("Refresh")}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setRemove(entry.name)}
                >
                  {tr("Remove")}
                </Button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <div className="flex items-center gap-3">
        <Input
          type="search"
          aria-label={tr("Search plugins…")}
          placeholder="code-review"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setPage(0);
          }}
        />
        <Select
          value={market || "all"}
          onValueChange={(value) => {
            setMarket(value === "all" ? "" : value);
            setPage(0);
          }}
        >
          <SelectTrigger
            className="w-48 shrink-0"
            aria-label={tr("Filter by source")}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{tr("All sources")}</SelectItem>
            {catalog.sources.map((entry) => (
              <SelectItem key={entry.name} value={entry.name}>
                {entry.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {busy && !catalog.plugins.length && (
        <p
          role="status"
          className="py-8 text-center text-ui-sm text-muted-foreground"
        >
          {tr("Loading available plugins…")}
        </p>
      )}
      {!busy && !visible.length && (
        <p className="py-10 text-center text-ui-sm text-muted-foreground">
          {tr("No plugins match this filter.")}
        </p>
      )}
      <ul className="divide-y divide-border/60">
        {visible.map((plugin) => (
          <li
            key={`${plugin.name}@${plugin.marketplace}`}
            className="flex items-start gap-4 py-4"
          >
            <div className="min-w-0 flex-1">
              <p className="text-ui-base font-medium">
                {plugin.name}{" "}
                <span className="font-normal text-muted-foreground">
                  {plugin.version}
                </span>
              </p>
              <p className="mt-1 text-ui-xs text-muted-foreground">
                {plugin.marketplace}
                {plugin.installed ? ` · ${tr("Installed")}` : ""}
              </p>
              {plugin.description && (
                <p className="mt-2 line-clamp-2 text-ui-sm text-muted-foreground">
                  {plugin.description}
                </p>
              )}
            </div>
            <Button
              size="sm"
              variant={plugin.installed ? "outline" : "default"}
              disabled={busy}
              onClick={() =>
                plugin.installed
                  ? useSettingsOverlay.getState().select("plugins")
                  : setInstall(plugin)
              }
            >
              {tr(plugin.installed ? "Manage" : "Install")}
            </Button>
          </li>
        ))}
      </ul>
      {pages > 1 && (
        <nav
          aria-label={tr("Plugin marketplace pages")}
          className="flex items-center justify-end gap-3"
        >
          <Button
            size="sm"
            variant="ghost"
            disabled={currentPage === 0}
            onClick={() => setPage(currentPage - 1)}
          >
            {tr("Previous")}
          </Button>
          <span className="text-ui-xs text-muted-foreground">
            {currentPage + 1} / {pages}
          </span>
          <Button
            size="sm"
            variant="ghost"
            disabled={currentPage >= pages - 1}
            onClick={() => setPage(currentPage + 1)}
          >
            {tr("Next")}
          </Button>
        </nav>
      )}
      {remove && (
        <PluginActionDialog
          title="Remove marketplace source"
          target={remove}
          busy={busy}
          error={error}
          onClose={() => setRemove(null)}
          onConfirm={() =>
            void run(async () => {
              await extensionApi.removeMarketplace(remove);
              setRemove(null);
            })
          }
        />
      )}
      {install && (
        <PluginActionDialog
          title="Install plugin"
          target={`${install.name}@${install.marketplace}`}
          busy={busy}
          error={error}
          onClose={() => setInstall(null)}
          onConfirm={() =>
            void run(async () => {
              const id = `${install.name}@${install.marketplace}`;
              await extensionApi.installMarketplacePlugin(id);
              setInstall(null);
              setConfigure(id);
            })
          }
        />
      )}
      {configure && (
        <PluginDetailsDialog
          key={configure}
          pluginId={configure}
          onClose={() => setConfigure(null)}
          onSaved={load}
        />
      )}
    </div>
  );
}
