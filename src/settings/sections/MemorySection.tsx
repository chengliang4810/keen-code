import { useSpaces } from "@/modules/spaces/lib/useSpaces";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useTranslation, useLocale } from "@/modules/i18n";
import {
  memoryApi,
  memoryWorkspaceLabel,
  type MemoryWorkspace,
} from "@/modules/ai/lib/memory";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { setMemoryEnabled } from "@/modules/settings/store";
import { currentWorkspaceEnv } from "@/modules/workspace";
import { SectionHeader } from "@/settings/components/SectionHeader";

export function MemorySection() {
  const tr = useTranslation();
  const locale = useLocale();
  const projects = useSpaces((s) => s.spaces);
  const enabled = usePreferencesStore((s) => s.memoryEnabled);
  const local = currentWorkspaceEnv().kind === "local";
  const [catalog, setCatalog] = useState<MemoryWorkspace[]>([]);
  const [scope, setScope] = useState("");
  const [query, setQuery] = useState("");
  const [file, setFile] = useState<string | null>(null);
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const request = useRef(0);
  const previewRequest = useRef(0);
  const refresh = useCallback(async () => {
    const id = ++request.current;
    setLoading(true);
    setError("");
    try {
      const data = await memoryApi.catalog();
      if (id !== request.current) return;
      setCatalog(data);
      setScope((current) =>
        data.some((w) => w.id === current) ? current : (data[0]?.id ?? ""),
      );
      setFile(null);
      setContent(null);
      previewRequest.current++;
    } catch (e) {
      if (id === request.current) setError(String(e));
    } finally {
      if (id === request.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    if (enabled && local) void refresh();
    return () => {
      request.current++;
      previewRequest.current++;
    };
  }, [enabled, local, refresh]);
  const selected = catalog.find((w) => w.id === scope);
  const visible =
    selected?.files.filter((f) =>
      f.name.toLowerCase().includes(query.trim().toLowerCase()),
    ) ?? [];
  const read = async (name: string) => {
    const id = ++previewRequest.current;
    setFile(name);
    setContent(null);
    setError("");
    try {
      const result = await memoryApi.read(scope, name);
      if (id === previewRequest.current) setContent(result.content ?? "");
    } catch (e) {
      if (id === previewRequest.current) setError(String(e));
    }
  };
  return (
    <div className="flex flex-col gap-5">
      <SectionHeader title={tr("Memory")} />
      <div className="flex items-center justify-between gap-3">
        <label htmlFor="workspace-memory" className="text-ui-base font-medium">
          {tr("Workspace Memory")}
        </label>
        <Switch
          id="workspace-memory"
          checked={enabled}
          disabled={saving || !local}
          onCheckedChange={(value) => {
            setSaving(true);
            setError("");
            void setMemoryEnabled(value)
              .catch((e) => setError(String(e)))
              .finally(() => setSaving(false));
          }}
        />
      </div>
      {!local && (
        <p className="text-ui-sm text-muted-foreground">
          {tr(
            "Memory details are available only in the local desktop app. Open Memory settings there to view them.",
          )}
        </p>
      )}
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
      {enabled && local && (
        <>
          <div className="flex flex-wrap items-center gap-2">
            {catalog.length > 0 && (
              <Select
                value={scope}
                onValueChange={(value) => {
                  setScope(value);
                  setFile(null);
                  setContent(null);
                  setError("");
                  previewRequest.current++;
                }}
              >
                <SelectTrigger aria-label={tr("Workspaces")} className="w-48">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {catalog.map((w) => (
                    <SelectItem key={w.id} value={w.id} title={w.root}>
                      {memoryWorkspaceLabel(w, projects)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            <Input
              aria-label={tr("Search memory files…")}
              placeholder="MEMORY.md"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="min-w-32 flex-1"
            />
            <Button
              variant="outline"
              size="sm"
              disabled={loading}
              onClick={() => void refresh()}
            >
              {tr("Refresh")}
            </Button>
          </div>
          {loading && (
            <p role="status" className="text-ui-sm text-muted-foreground">
              {tr("Loading memories…")}
            </p>
          )}
          {!loading && !selected && (
            <p className="text-ui-sm text-muted-foreground">
              {tr("No saved workspace memories")}
            </p>
          )}
          {selected && (
            <section className="space-y-3">
              <div className="flex items-center justify-between text-ui-base">
                <h3 className="font-medium">{tr("Files")}</h3>
                <span className="text-muted-foreground">
                  {selected.files.length}
                </span>
              </div>
              <div className="max-h-72 overflow-y-auto rounded-lg border divide-y">
                {visible.map((f) => (
                  <button
                    type="button"
                    key={f.name}
                    aria-pressed={file === f.name}
                    onClick={() => void read(f.name)}
                    className="flex w-full items-center justify-between gap-3 px-3 py-2.5 text-left text-ui-sm hover:bg-accent aria-pressed:bg-accent"
                  >
                    <span className="truncate font-mono">{f.name}</span>
                    <time
                      className="shrink-0 text-ui-xs text-muted-foreground"
                      dateTime={new Date(f.updatedAt).toISOString()}
                    >
                      {new Date(f.updatedAt).toLocaleString(locale)}
                    </time>
                  </button>
                ))}
              </div>
              {!visible.length && (
                <p className="text-ui-sm text-muted-foreground">
                  {tr("No memory files found.")}
                </p>
              )}
              {file && (
                <Textarea
                  aria-label={file}
                  readOnly
                  value={content ?? ""}
                  className="min-h-56 max-h-96 font-mono text-ui-sm"
                />
              )}
            </section>
          )}
        </>
      )}
    </div>
  );
}
