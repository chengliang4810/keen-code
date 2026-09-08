import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { GlassModal } from "@/components/GlassModal";
import { AgentModelSelect } from "@/components/AgentsPanel";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";

const ALIASES = ["sonnet", "opus", "haiku"] as const;

/** 只有打开配置时读取模型目录，不增加市场的后台轮询。 */
export function PluginCompatibilitySettings({ locale }: { locale: Locale }) {
  const [open, setOpen] = useState(false);
  const tr = createT(locale);
  return <>
    <Button className="btn btn--ghost btn--sm" onClick={() => setOpen(true)}>
      {tr("ext.compatibility.title")}
    </Button>
    {open ? <CompatibilityDialog locale={locale} onClose={() => setOpen(false)} /> : null}
  </>;
}

function CompatibilityDialog({ locale, onClose }: { locale: Locale; onClose: () => void }) {
  const tr = createT(locale);
  const [config, setConfig] = useState<api.PluginModelAliases>({ sonnet: null, opus: null, haiku: null });
  const [providers, setProviders] = useState<api.CustomProvider[]>([]);
  const [loading, setLoading] = useState(true);
  const [ready, setReady] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    Promise.all([api.pluginModelAliasesGet(), api.providersList()]).then(([aliases, catalog]) => {
      if (active) { setConfig(aliases); setProviders(catalog.providers); setReady(true); setLoading(false); }
    }).catch(() => {
      if (active) { setError(tr("ext.compatibility.loadFailed")); setLoading(false); }
    });
    return () => { active = false; };
  }, [locale]);
  const groups = providers.map(provider => ({ providerId: provider.id, providerLabel: provider.name || provider.id, models: provider.models }));
  const save = async () => {
    setSaving(true);
    setError(null);
    try { await api.pluginModelAliasesSet(config); onClose(); }
    catch { setError(tr("ext.compatibility.saveFailed")); }
    finally { setSaving(false); }
  };
  return <GlassModal open onClose={() => { if (!saving) onClose(); }} title={tr("ext.compatibility.title")}
    closeLabel={tr("common.close")} footer={<>
      <Button className="btn btn--ghost" disabled={saving} onClick={onClose}>{tr("common.cancel")}</Button>
      <Button className="btn btn--primary" disabled={!ready || saving} onClick={() => void save()}>{tr("common.save")}</Button>
    </>}>
    <p>{tr("ext.compatibility.description")}</p>
    {error ? <p role="alert">{error}</p> : null}
    <div className="flex flex-col gap-4" aria-busy={loading || saving}>
      {ALIASES.map(alias => <div key={alias} className="flex items-center justify-between gap-4">
        <span>{alias}</span>
        <AgentModelSelect locale={locale} label={alias} value={config[alias]} providerGroups={groups}
          disabled={!ready || saving} onSelect={value => setConfig(current => ({ ...current, [alias]: value || null }))} />
      </div>)}
    </div>
  </GlassModal>;
}
