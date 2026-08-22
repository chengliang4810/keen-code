import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
/** 设置 → 扩展：浏览和管理 KeenCode 本地插件市场。 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import { GlassModal } from "@/components/GlassModal";
import {
  marketplacePluginInstallConfirmKey,
  marketplacePluginMeta,
  marketplacePluginRequiresRestart,
} from "@/lib/extensionsUi";
import {
  IconPuzzle,
  IconRefresh,
  IconTrash,
} from "@/components/icons";

export type ExtensionsBuildExtrasProps = {
  /** 当前界面语言。 */
  locale: Locale;
  /** 插件安装完成后通知父组件刷新列表。 */
  onPluginsChanged?: () => void;
};

type MarketplaceSource = api.MarketplaceSourceDto;
type AvailablePlugin = api.AvailablePluginDto;

/** 单次渲染的插件数量，避免大市场列表一次全部挂载。 */
const PAGE_SIZE = 40;
/** 默认官方市场后台取得期间的轮询间隔；空闲或失败后立即停止。 */
const MARKETPLACE_POLL_INTERVAL_MS = 750;

export type MarketplaceRefresh = () => Promise<boolean>;

/** 可取消的市场后台取得轮询；仅在 refresh 返回 loading=true 时继续安排下一次。 */
export function createMarketplacePoller(
  refresh: MarketplaceRefresh,
  intervalMs = MARKETPLACE_POLL_INTERVAL_MS,
) {
  let active = true;
  let started = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const run = async () => {
    if (!active) return;
    let pending = false;
    try {
      pending = await refresh();
    } catch {
      return;
    }
    if (active && pending) {
      timer = setTimeout(() => {
        void run();
      }, intervalMs);
    }
  };

  return {
    start() {
      if (started || !active) return;
      started = true;
      void run();
    },
    cancel() {
      active = false;
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    },
  };
}

/** 管理本地插件市场与可安装插件。 */
export function ExtensionsBuildExtras({
  locale,
  onPluginsChanged,
}: ExtensionsBuildExtrasProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [sources, setSources] = useState<MarketplaceSource[]>([]);
  const [available, setAvailable] = useState<AvailablePlugin[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [addSource, setAddSource] = useState("");
  const [query, setQuery] = useState("");
  const [marketFilter, setMarketFilter] = useState("__all__");
  const [pageLimit, setPageLimit] = useState(PAGE_SIZE);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [removeSource, setRemoveSource] = useState<MarketplaceSource | null>(null);
  const [installTarget, setInstallTarget] = useState<AvailablePlugin | null>(null);
  const pollerRef = useRef<ReturnType<typeof createMarketplacePoller> | null>(null);

  /** 从当前后端重新读取市场和可安装插件。 */
  const refresh = useCallback(async (): Promise<boolean> => {
    if (!api.isTauri()) {
      setSources([]);
      setAvailable([]);
      setLoading(false);
      setError(null);
      return false;
    }
    setLoading(true);
    setError(null);
    try {
      const [sourcesResult, availableResult] = await Promise.all([
        api.marketplaceList(),
        api.marketplaceAvailable(),
      ]);
      const nextSources = [...sourcesResult].sort((left, right) =>
        left.name.localeCompare(right.name),
      );
      const nextAvailable = availableResult.plugins
        .sort((left, right) => left.name.localeCompare(right.name));
      setSources(nextSources);
      setAvailable(nextAvailable);
      setError(localizeUiError(availableResult.error, locale));
      const nextLoading = availableResult.loading;
      setLoading(nextLoading);
      setMarketFilter((current) =>
        current === "__all__" ||
        nextSources.some((source) => source.name === current)
          ? current
          : "__all__",
      );
      return nextLoading;
    } catch (cause) {
      setSources([]);
      setAvailable([]);
      setError(localizeUiError(cause, locale));
      setLoading(false);
      return false;
    }
  }, []);

  const startMarketplaceRefresh = useCallback(() => {
    pollerRef.current?.cancel();
    const poller = createMarketplacePoller(refresh);
    pollerRef.current = poller;
    poller.start();
  }, [refresh]);

  /** 用户显式刷新时绕过失败退避，并在默认源尚未登记时重新取得官方市场。 */
  const refreshCatalog = async () => {
    if (!api.isTauri()) {
      startMarketplaceRefresh();
      return;
    }
    setBusy("refresh:catalog");
    setError(null);
    try {
      await api.marketplaceUpdate(null, true);
      startMarketplaceRefresh();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    startMarketplaceRefresh();
    return () => {
      pollerRef.current?.cancel();
      pollerRef.current = null;
    };
  }, [startMarketplaceRefresh]);

  useEffect(() => {
    setPageLimit(PAGE_SIZE);
  }, [marketFilter, query]);

  const filtered = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return available.filter((plugin) => {
      if (
        marketFilter !== "__all__" &&
        plugin.marketplace !== marketFilter
      ) {
        return false;
      }
      if (!normalizedQuery) return true;
      return [plugin.name, plugin.description ?? "", plugin.marketplace]
        .join(" ")
        .toLowerCase()
        .includes(normalizedQuery);
    });
  }, [available, marketFilter, query]);
  const visible = filtered.slice(0, pageLimit);

  /** 添加一个本地市场目录或 `keencode-marketplace.json`。 */
  const addMarketplace = async () => {
    const source = addSource.trim();
    if (!source) {
      setError(tr("ext.market.addEmpty"));
      return;
    }
    setBusy("add");
    setError(null);
    try {
      await api.marketplaceAdd(source);
      setAddSource("");
      startMarketplaceRefresh();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  };

  /** 移除当前选中的市场登记，不删除原目录。 */
  const confirmRemoveSource = async () => {
    if (!removeSource) return;
    setBusy(`remove:${removeSource.name}`);
    setError(null);
    try {
      await api.marketplaceRemove(removeSource.name);
      setRemoveSource(null);
      startMarketplaceRefresh();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  };

  /** 重新校验一个或全部本地市场清单。 */
  const refreshSources = async (name: string | null) => {
    setBusy(name ? `refresh:${name}` : "refresh:all");
    setError(null);
    try {
      await api.marketplaceUpdate(name);
      startMarketplaceRefresh();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  };

  /** 从选中的本地市场安装插件。 */
  const confirmInstall = async () => {
    if (!installTarget) return;
    const source = `${installTarget.name}@${installTarget.marketplace}`;
    setBusy(`install:${installTarget.name}`);
    setError(null);
    try {
      await api.pluginInstall(source);
      setAvailable((current) =>
        current.filter(
          (plugin) =>
            plugin.name !== installTarget.name ||
            plugin.marketplace !== installTarget.marketplace,
        ),
      );
      setInstallTarget(null);
      onPluginsChanged?.();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  };

  return (
    <>
      <h2 className="settings-page__h2" id="settings-anchor-ext-market">
        <IconPuzzle size={15} />
        {tr("ext.market.title")}
        {!loading ? <span className="ext-count">{filtered.length}</span> : null}
        <Button
          type="button"
          className="btn btn--ghost ext-bulk-btn"
          disabled={loading || busy !== null}
          onClick={() => void refreshCatalog()}
        >
          <IconRefresh size={14} />
          <span>{loading ? tr("ext.market.updating") : tr("ext.market.refreshCatalog")}</span>
        </Button>
      </h2>

      <div className="settings-card ext-card">
        {error ? (
          <div className="ext-alert ext-alert--error" role="alert">
            <div className="ext-alert__title">{tr("ext.market.error")}</div>
            <p className="ext-alert__body">{error}</p>
          </div>
        ) : null}

        <div className="ext-market-browse">
          <div
            className="ext-plugin-filters"
            role="tablist"
            aria-label={tr("ext.market.filterLabel")}
          >
            <Button
              type="button"
              role="tab"
              aria-selected={marketFilter === "__all__"}
              className={
                "ext-plugin-filter" +
                (marketFilter === "__all__" ? " is-active" : "")
              }
              onClick={() => setMarketFilter("__all__")}
            >
              {tr("ext.market.filterAll")}
            </Button>
            {sources.map((source) => (
              <Button
                key={source.name}
                type="button"
                role="tab"
                aria-selected={marketFilter === source.name}
                className={
                  "ext-plugin-filter" +
                  (marketFilter === source.name ? " is-active" : "")
                }
                onClick={() => setMarketFilter(source.name)}
              >
                {source.name}
              </Button>
            ))}
          </div>
          <Input
            type="search"
            className="settings-input ext-market-browse__search"
            value={query}
            placeholder={tr("ext.market.searchPlaceholder")}
            disabled={loading}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>

        {loading ? (
          <p className="ext-empty">{tr("ext.market.availableLoading")}</p>
        ) : visible.length === 0 ? (
          <p className="ext-empty">{tr("ext.market.availableEmpty")}</p>
        ) : (
          <ul className="ext-list ext-market-browse__list">
            {visible.map((plugin) => {
              const meta = marketplacePluginMeta(plugin);
              const hasLspServers = marketplacePluginRequiresRestart(plugin);
              return (
                <li
                  key={`${plugin.marketplace}:${plugin.name}`}
                  className="ext-item"
                >
                  <div className="ext-item__head">
                    <span className="ext-item__name">{plugin.name}</span>
                    <span className="ext-badge ext-badge--plugin">
                      {plugin.marketplace}
                    </span>
                  </div>
                  {plugin.description ? (
                    <div className="ext-item__desc">{plugin.description}</div>
                  ) : null}
                  {meta ? <div className="ext-item__meta">{meta}</div> : null}
                  {hasLspServers ? (
                    <div className="ext-item__desc">
                      {tr("ext.market.lspRestart", {
                        count: plugin.lspCount,
                      })}
                    </div>
                  ) : null}
                  <div className="ext-item__actions">
                    <Button
                      type="button"
                      className="btn btn--solid btn--sm"
                      disabled={busy !== null}
                      onClick={() => setInstallTarget(plugin)}
                    >
                      {busy === `install:${plugin.name}`
                        ? tr("ext.market.installing")
                        : tr("ext.market.install")}
                    </Button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}

        {filtered.length > visible.length ? (
          <div className="ext-folder-actions">
            <Button
              type="button"
              className="btn btn--ghost btn--sm"
              onClick={() => setPageLimit((current) => current + PAGE_SIZE)}
            >
              {tr("ext.market.showMore", { n: filtered.length - visible.length })}
            </Button>
          </div>
        ) : null}

        <Collapsible
          className="ext-market-sources"
          open={sourcesOpen}
          onOpenChange={setSourcesOpen}
        >
          <CollapsibleTrigger className="ext-market-sources__summary">
            {tr("ext.market.sourcesTitle")}
            {!loading ? <span className="ext-count">{sources.length}</span> : null}
          </CollapsibleTrigger>
          <CollapsibleContent>
          <div className="ext-plugin-install">
            <Label
              className="ext-plugin-install__label"
              htmlFor="ext-market-source"
            >
              {tr("ext.market.addLabel")}
            </Label>
            <div className="ext-plugin-install__row">
              <Input
                id="ext-market-source"
                type="text"
                className="settings-input ext-plugin-install__input"
                value={addSource}
                placeholder="/absolute/path/to/marketplace"
                disabled={busy !== null}
                autoComplete="off"
                spellCheck={false}
                onChange={(event) => setAddSource(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addMarketplace();
                  }
                }}
              />
              <Button
                type="button"
                className="btn btn--solid"
                disabled={busy !== null || !addSource.trim()}
                onClick={() => void addMarketplace()}
              >
                {busy === "add" ? tr("ext.market.adding") : tr("ext.market.add")}
              </Button>
            </div>
          </div>
          <div className="ext-folder-actions">
            <Button
              type="button"
              className="btn btn--ghost btn--sm"
              disabled={loading || busy !== null}
              onClick={() => void refreshSources(null)}
            >
              <IconRefresh size={13} />
              <span>
                {busy === "refresh:all"
                  ? tr("ext.market.updating")
                  : tr("ext.market.updateAll")}
              </span>
            </Button>
          </div>
          {loading ? (
            <p className="ext-field-hint">{tr("ext.market.loading")}</p>
          ) : sources.length === 0 ? (
            <p className="ext-field-hint">{tr("ext.market.empty")}</p>
          ) : (
            <ul className="ext-list">
              {sources.map((source) => (
                <li key={source.name} className="ext-item">
                  <div className="ext-item__head">
                    <span className="ext-item__name">{source.name}</span>
                  </div>
                  <div className="ext-item__meta">{source.path}</div>
                  <div className="ext-item__actions">
                    <Button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={busy !== null}
                      onClick={() => void refreshSources(source.name)}
                    >
                      <IconRefresh size={13} />
                      <span>
                        {busy === `refresh:${source.name}`
                          ? tr("ext.market.updating")
                          : tr("ext.market.update")}
                      </span>
                    </Button>
                    <Button
                      type="button"
                      className="btn btn--ghost btn--sm ext-item__danger"
                      disabled={busy !== null}
                      onClick={() => setRemoveSource(source)}
                    >
                      <IconTrash size={13} />
                      <span>{tr("ext.market.remove")}</span>
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
          </CollapsibleContent>
        </Collapsible>
      </div>

      <GlassModal
        open={removeSource !== null}
        onClose={() => {
          if (busy === null) setRemoveSource(null);
        }}
        title={tr("ext.market.removeTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={busy !== null}
              onClick={() => setRemoveSource(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--danger"
              disabled={busy !== null}
              onClick={() => void confirmRemoveSource()}
            >
              {tr("ext.market.remove")}
            </Button>
          </>
        }
      >
        <p className="app-dialog__msg">
          {tr("ext.market.removeConfirm", { name: removeSource?.name ?? "" })}
        </p>
      </GlassModal>

      <GlassModal
        open={installTarget !== null}
        onClose={() => {
          if (busy === null) setInstallTarget(null);
        }}
        title={tr("ext.market.installTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={busy !== null}
              onClick={() => setInstallTarget(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              disabled={busy !== null}
              onClick={() => void confirmInstall()}
            >
              {busy?.startsWith("install:")
                ? tr("ext.market.installing")
                : tr("ext.market.install")}
            </Button>
          </>
        }
      >
        <p className="app-dialog__msg">
          {tr(
            installTarget
              ? marketplacePluginInstallConfirmKey(installTarget)
              : "ext.market.installConfirm",
            {
              name: installTarget?.name ?? "",
              count: installTarget?.lspCount ?? 0,
            },
          )}
        </p>
      </GlassModal>
    </>
  );
}
