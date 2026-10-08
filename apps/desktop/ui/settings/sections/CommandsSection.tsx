import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/localized/dialog";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowLeft01Icon,
  Delete02Icon,
  Edit02Icon,
  RefreshIcon,
} from "@hugeicons/core-free-icons";
import { useTranslation } from "@/modules/i18n";
import {
  commandApi,
  type CommandCatalog,
  type CommandEntry,
  type CommandFile,
} from "@/modules/ai/lib/commands";
import { CommandForm } from "@/settings/components/CommandForm";
import { CommandImportDialog } from "@/settings/components/CommandImportDialog";
import { SectionHeader } from "@/settings/components/SectionHeader";

export function CommandsSection() {
  const tr = useTranslation();
  const [catalog, setCatalog] = useState<CommandCatalog | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [editor, setEditor] = useState<{
    file: CommandFile | null;
  } | null>(null);
  const [deleting, setDeleting] = useState<CommandFile | null>(null);
  const [importing, setImporting] = useState(false);
  const generation = useRef(0);
  const reload = useCallback(async () => {
    const current = ++generation.current;
    setLoading(true);
    setError("");
    try {
      const next = await commandApi.list(null);
      if (current === generation.current) setCatalog(next);
    } catch (error) {
      if (current === generation.current) setError(String(error));
    } finally {
      if (current === generation.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void reload();
    return () => {
      generation.current += 1;
    };
  }, [reload]);
  const perform = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (error) {
      setError(String(error));
    } finally {
      setBusy(false);
    }
  };
  const changed = async () => {
    window.dispatchEvent(new Event("rcode:commands-changed"));
    await reload();
  };
  const open = (entry: CommandEntry, remove = false) =>
    perform(async () => {
      const file = await commandApi.read(entry.name);
      if (remove) setDeleting(file);
      else setEditor({ file });
    });
  const toggle = (entry: CommandEntry, enabled: boolean) =>
    perform(async () => {
      const file = await commandApi.read(entry.name);
      const { content, ...config } = file;
      await commandApi.save({ ...config, enabled }, content);
      await changed();
    });
  const matches = (entry: { name: string; description: string }) =>
    `${entry.name} ${entry.description}`
      .toLowerCase()
      .includes(query.trim().toLowerCase());
  const commands = catalog?.commands.filter(matches) ?? [];
  const groups = new Map<
    string,
    NonNullable<CommandCatalog>["pluginCommands"]
  >();
  for (const entry of catalog?.pluginCommands.filter(matches) ?? []) {
    const key = JSON.stringify([entry.pluginName, entry.marketplace]);
    const entries = groups.get(key) ?? [];
    entries.push(entry);
    groups.set(key, entries);
  }
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          {editor && (
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={busy}
              aria-label={tr("Back")}
              onClick={() => {
                setEditor(null);
                setError("");
              }}
            >
              <HugeiconsIcon icon={ArrowLeft01Icon} size={16} />
            </Button>
          )}
          <SectionHeader
            title={tr(
              editor
                ? editor.file
                  ? "Edit command"
                  : "New command"
                : "Commands",
            )}
          />
        </div>
        {!editor && (
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={busy || loading}
              aria-label={tr("Refresh")}
              onClick={() => void reload()}
            >
              <HugeiconsIcon icon={RefreshIcon} size={15} />
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={busy || loading || !catalog}
              onClick={() => setImporting(true)}
            >
              {tr("Import")}
            </Button>
            <Button
              size="sm"
              disabled={busy || loading || !catalog}
              onClick={() => {
                setEditor({ file: null });
                setError("");
              }}
            >
              <HugeiconsIcon icon={Add01Icon} size={14} />
              {tr("New command")}
            </Button>
          </div>
        )}
      </div>
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {tr(error)}
        </p>
      )}
      {editor ? (
        <CommandForm
          initial={editor.file}
          busy={busy}
          onCancel={() => {
            setEditor(null);
            setError("");
          }}
          onDelete={
            editor.file
              ? () => setDeleting(editor.file as CommandFile)
              : undefined
          }
          onSave={(config) =>
            perform(async () => {
              await commandApi.save(config, editor.file?.content ?? null);
              setEditor(null);
              await changed();
            })
          }
        />
      ) : (
        <>
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="my-command"
            aria-label={tr("Search commands")}
            className="h-8"
          />
          {loading && (
            <p role="status" className="text-ui-sm text-muted-foreground">
              {tr("Loading…")}
            </p>
          )}
          {(!query.trim() || commands.length > 0) && (
            <section className="space-y-2">
              <h3 className="text-ui-sm font-medium">
                {tr("Installed")}{" "}
                <span className="text-muted-foreground">{commands.length}</span>
              </h3>
              {!loading && commands.length === 0 && (
                <p className="py-5 text-center text-ui-sm text-muted-foreground">
                  {tr(
                    query.trim()
                      ? "No commands match your search"
                      : "No user commands",
                  )}
                </p>
              )}
              {!!commands.length && (
                <ul className="divide-y divide-border/60 rounded-lg border border-border/60">
                  {commands.map((entry) => (
                    <li
                      key={entry.path}
                      className="flex items-center gap-3 px-3 py-3"
                    >
                      <button
                        type="button"
                        className="min-w-0 flex-1 text-left"
                        disabled={busy}
                        onClick={() => void open(entry)}
                      >
                        <span className="block truncate font-mono text-ui-sm">
                          /{entry.name}{" "}
                          {entry.argumentHint && (
                            <span className="text-muted-foreground">
                              {entry.argumentHint}
                            </span>
                          )}
                        </span>
                        {entry.description && (
                          <span className="mt-0.5 block truncate text-ui-xs text-muted-foreground">
                            {entry.description}
                          </span>
                        )}
                      </button>
                      <Switch
                        checked={entry.enabled}
                        disabled={busy}
                        onCheckedChange={(enabled) =>
                          void toggle(entry, enabled)
                        }
                        aria-label={`${tr(entry.enabled ? "Disable" : "Enable")} /${entry.name}`}
                      />
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label={`${tr("Edit command")} /${entry.name}`}
                        onClick={() => void open(entry)}
                      >
                        <HugeiconsIcon icon={Edit02Icon} size={14} />
                      </Button>
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label={`${tr("Delete command")} /${entry.name}`}
                        onClick={() => void open(entry, true)}
                      >
                        <HugeiconsIcon icon={Delete02Icon} size={14} />
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}
          {!loading &&
            !!query.trim() &&
            commands.length === 0 &&
            groups.size === 0 && (
              <p className="py-5 text-center text-ui-sm text-muted-foreground">
                {tr("No commands match your search")}
              </p>
            )}
          {[...groups].map(([key, entries]) => (
            <section key={key} className="space-y-2">
              <h3 className="text-ui-sm font-medium">
                {entries[0].pluginName}{" "}
                {entries[0].marketplace && (
                  <span className="text-muted-foreground">
                    {entries[0].marketplace}
                  </span>
                )}{" "}
                <span className="text-muted-foreground">{entries.length}</span>
              </h3>
              <ul className="divide-y divide-border/60 rounded-lg border border-border/60">
                {entries.map((entry) => (
                  <li key={entry.name} className="px-3 py-3">
                    <span className="block break-all font-mono text-ui-sm">
                      /{entry.name}
                    </span>
                    {entry.description && (
                      <span className="mt-0.5 block truncate text-ui-xs text-muted-foreground">
                        {entry.description}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          ))}
          {catalog?.diagnostics.map((message) => (
            <p
              key={message}
              role="alert"
              className="text-ui-sm text-destructive"
            >
              {tr(message)}
            </p>
          ))}
        </>
      )}
      <Dialog
        open={!!deleting}
        onOpenChange={(open) => {
          if (!open && !busy) setDeleting(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {tr("Delete command")} /{deleting?.name}
            </DialogTitle>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-ui-sm text-destructive">
              {tr(error)}
            </p>
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setDeleting(null)}
            >
              {tr("Cancel")}
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  if (!deleting) return;
                  await commandApi.remove(deleting.name, deleting.content);
                  setDeleting(null);
                  setEditor(null);
                  await changed();
                })
              }
            >
              {tr("Delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {importing && (
        <CommandImportDialog
          existing={catalog?.commands ?? []}
          onClose={() => setImporting(false)}
          onImported={changed}
        />
      )}
    </div>
  );
}
