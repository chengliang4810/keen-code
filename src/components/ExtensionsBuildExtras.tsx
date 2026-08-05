/** 设置 → 扩展：浏览和管理 KeenCode 本地插件市场。 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
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

/** 生成市场中的插件元信息。 */
function pluginMeta(plugin: AvailablePlugin): string {
  const parts: string[] = [];
  if (plugin.version?.trim()) parts.push(`v${plugin.version.replace(/^v/i, "")}`);
  if (plugin.skillCount > 0) parts.push(`${plugin.skillCount} Skills`);
  return parts.join(" · ");
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

  /** 从当前后端重新读取市场和可安装插件。 */
  const refresh = useCallback(async () => {
    if (!api.isTauri()) {
      setSources([]);
      setAvailable([]);
      setLoading(false);
      return;
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
      setMarketFilter((current) =>
        current === "__all__" ||
        nextSources.some((source) => source.name === current)
          ? current
          : "__all__",
      );
    } catch (cause) {
      setSources([]);
      setAvailable([]);
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

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
      await refresh();
    } catch (cause) {
      setError(String(cause));
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
      await refresh();
    } catch (cause) {
      setError(String(cause));
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
      await refresh();
    } catch (cause) {
      setError(String(cause));
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
      setError(String(cause));
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
        <button
          type="button"
          className="btn btn--ghost ext-bulk-btn"
          disabled={loading || busy !== null}
          onClick={() => void refresh()}
        >
          <IconRefresh size={14} />
          <span>{loading ? tr("ext.market.updating") : tr("ext.market.refreshCatalog")}</span>
        </button>
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
            <button
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
            </button>
            {sources.map((source) => (
              <button
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
              </button>
            ))}
          </div>
          <input
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
            {visible.map((plugin) => (
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
                {pluginMeta(plugin) ? (
                  <div className="ext-item__meta">{pluginMeta(plugin)}</div>
                ) : null}
                <div className="ext-item__actions">
                  <button
                    type="button"
                    className="btn btn--solid btn--sm"
                    disabled={busy !== null}
                    onClick={() => setInstallTarget(plugin)}
                  >
                    {busy === `install:${plugin.name}`
                      ? tr("ext.market.installing")
                      : tr("ext.market.install")}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}

        {filtered.length > visible.length ? (
          <div className="ext-folder-actions">
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              onClick={() => setPageLimit((current) => current + PAGE_SIZE)}
            >
              {tr("ext.market.showMore", { n: filtered.length - visible.length })}
            </button>
          </div>
        ) : null}

        <details
          className="ext-market-sources"
          open={sourcesOpen}
          onToggle={(event) =>
            setSourcesOpen((event.target as HTMLDetailsElement).open)
          }
        >
          <summary className="ext-market-sources__summary">
            {tr("ext.market.sourcesTitle")}
            {!loading ? <span className="ext-count">{sources.length}</span> : null}
          </summary>
          <div className="ext-plugin-install">
            <label
              className="ext-plugin-install__label"
              htmlFor="ext-market-source"
            >
              {tr("ext.market.addLabel")}
            </label>
            <div className="ext-plugin-install__row">
              <input
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
              <button
                type="button"
                className="btn btn--solid"
                disabled={busy !== null || !addSource.trim()}
                onClick={() => void addMarketplace()}
              >
                {busy === "add" ? tr("ext.market.adding") : tr("ext.market.add")}
              </button>
            </div>
          </div>
          <div className="ext-folder-actions">
            <button
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
            </button>
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
                    <button
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
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm ext-item__danger"
                      disabled={busy !== null}
                      onClick={() => setRemoveSource(source)}
                    >
                      <IconTrash size={13} />
                      <span>{tr("ext.market.remove")}</span>
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </details>
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
            <button
              type="button"
              className="btn btn--ghost"
              disabled={busy !== null}
              onClick={() => setRemoveSource(null)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--danger"
              disabled={busy !== null}
              onClick={() => void confirmRemoveSource()}
            >
              {tr("ext.market.remove")}
            </button>
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
            <button
              type="button"
              className="btn btn--ghost"
              disabled={busy !== null}
              onClick={() => setInstallTarget(null)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--solid"
              disabled={busy !== null}
              onClick={() => void confirmInstall()}
            >
              {busy?.startsWith("install:")
                ? tr("ext.market.installing")
                : tr("ext.market.install")}
            </button>
          </>
        }
      >
        <p className="app-dialog__msg">
          {tr("ext.market.installConfirm", { name: installTarget?.name ?? "" })}
        </p>
      </GlassModal>
    </>
  );
}
