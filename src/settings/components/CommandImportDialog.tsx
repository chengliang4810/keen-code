import { useEffect, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { ArrowDown01Icon, ArrowRight01Icon } from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/localized/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import { useTranslation } from "@/modules/i18n";
import {
  commandApi,
  importKey,
  type CommandEntry,
  type ExternalCommand,
  type ImportCatalog,
  type ImportResult,
} from "@/modules/ai/lib/commands";

export function CommandImportDialog({
  existing,
  onClose,
  onImported,
}: {
  existing: readonly CommandEntry[];
  onClose: () => void;
  onImported: () => Promise<void>;
}) {
  const tr = useTranslation();
  const [catalog, setCatalog] = useState<ImportCatalog | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const [results, setResults] = useState<ImportResult[] | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // biome-ignore lint/correctness/useExhaustiveDependencies: Refresh rescans files on disk.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError("");
    setSelected(new Set());
    void commandApi
      .discover()
      .then((catalog) => {
        if (!cancelled) setCatalog(catalog);
      })
      .catch((error) => {
        if (!cancelled) setError(String(error));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [revision]);
  const duplicate = (command: ExternalCommand) =>
    existing.some(
      (entry) => entry.name.toLowerCase() === command.name.toLowerCase(),
    );
  const commands = catalog?.commands ?? [];
  const available = commands.filter((command) => !duplicate(command));
  const selections =
    catalog?.commands.filter(
      (command) => selected.has(importKey(command)) && !duplicate(command),
    ) ?? [];
  const all =
    available.length > 0 &&
    available.every((command) => selected.has(importKey(command)));
  const groups = new Map<string, ExternalCommand[]>();
  for (const command of commands) {
    const group = groups.get(command.agentLabel) ?? [];
    group.push(command);
    groups.set(command.agentLabel, group);
  }
  const select = (key: string, checked: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(key);
      else next.delete(key);
      return next;
    });
  const importSelected = async () => {
    if (busy || !selections.length) return;
    setBusy(true);
    setError("");
    try {
      const results = await commandApi.import(
        selections.map((command) => command.selection),
      );
      setResults(results);
      await onImported();
    } catch (error) {
      setError(String(error));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent className="flex max-h-[85vh] max-w-xl flex-col gap-4">
        <DialogHeader>
          <DialogTitle>{tr("Import external agent commands")}</DialogTitle>
        </DialogHeader>
        {error && (
          <p role="alert" className="text-ui-sm text-destructive">
            {tr(error)}
          </p>
        )}
        {results ? (
          <ScrollArea className="min-h-0 flex-1 pr-3">
            <ul className="divide-y divide-border/60">
              {results.map((result) => (
                <li
                  key={result.id}
                  className="flex flex-wrap items-center justify-between gap-2 py-2 text-ui-sm"
                >
                  <span className="font-mono">/{result.name}</span>
                  <span
                    className={
                      result.status === "failed"
                        ? "text-destructive"
                        : "text-muted-foreground"
                    }
                  >
                    {tr(
                      result.status === "imported"
                        ? "Imported"
                        : result.status === "skipped"
                          ? "Skipped"
                          : "Failed",
                    )}
                  </span>
                  {result.error && (
                    <p className="w-full text-ui-xs text-destructive">
                      {tr(result.error)}
                    </p>
                  )}
                </li>
              ))}
            </ul>
          </ScrollArea>
        ) : (
          <>
            <div className="flex justify-end">
              <Button
                size="sm"
                variant="ghost"
                disabled={loading || busy}
                onClick={() => setRevision((value) => value + 1)}
              >
                {tr("Refresh")}
              </Button>
            </div>
            {loading && (
              <p role="status" className="text-ui-sm text-muted-foreground">
                {tr("Loading…")}
              </p>
            )}
            {!loading && (
              <div className="flex items-center justify-between text-ui-sm">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy || !available.length}
                  onClick={() =>
                    setSelected((current) => {
                      const next = new Set(current);
                      for (const command of available) {
                        if (all) next.delete(importKey(command));
                        else next.add(importKey(command));
                      }
                      return next;
                    })
                  }
                >
                  {tr(all ? "Clear all" : "Select all")}
                </Button>
                <span className="text-muted-foreground">
                  {selections.length} / {catalog?.commands.length ?? 0}
                </span>
              </div>
            )}
            <ScrollArea className="min-h-0 max-h-72 flex-1 pr-3">
              {!loading && !commands.length && (
                <p className="py-6 text-center text-ui-sm text-muted-foreground">
                  {tr("No user commands")}
                </p>
              )}
              {[...groups].map(([label, commands]) => {
                const available = commands.filter(
                  (command) => !duplicate(command),
                );
                const selectedCount = available.filter((command) =>
                  selected.has(importKey(command)),
                ).length;
                return (
                  <section key={label} className="mb-3 space-y-2">
                    <div className="flex items-center gap-3">
                      <Checkbox
                        checked={
                          selectedCount === 0
                            ? false
                            : selectedCount === available.length
                              ? true
                              : "indeterminate"
                        }
                        disabled={busy || available.length === 0}
                        aria-label={`${tr("Select all commands from this source")} · ${label}`}
                        onCheckedChange={(checked) =>
                          setSelected((current) => {
                            const next = new Set(current);
                            for (const command of available) {
                              if (checked) next.add(importKey(command));
                              else next.delete(importKey(command));
                            }
                            return next;
                          })
                        }
                      />
                      <h3 className="min-w-0 flex-1 text-ui-sm font-medium">
                        <button
                          type="button"
                          className="flex w-full items-center gap-2 text-left"
                          aria-expanded={!collapsed.has(label)}
                          onClick={() =>
                            setCollapsed((current) => {
                              const next = new Set(current);
                              if (next.has(label)) next.delete(label);
                              else next.add(label);
                              return next;
                            })
                          }
                        >
                          {label}{" "}
                          <span className="text-muted-foreground">
                            {commands.length}
                          </span>
                          <HugeiconsIcon
                            icon={
                              collapsed.has(label)
                                ? ArrowRight01Icon
                                : ArrowDown01Icon
                            }
                            size={12}
                            className="ml-auto text-muted-foreground"
                          />
                        </button>
                      </h3>
                    </div>
                    {!collapsed.has(label) && (
                      <ul className="divide-y divide-border/60 rounded-lg border border-border/60">
                        {commands.map((command) => (
                          <li
                            key={importKey(command)}
                            className="flex items-center gap-3 p-3"
                          >
                            <Checkbox
                              checked={
                                selected.has(importKey(command)) &&
                                !duplicate(command)
                              }
                              disabled={busy || duplicate(command)}
                              onCheckedChange={(checked) =>
                                select(importKey(command), checked === true)
                              }
                              aria-label={`/${command.name}`}
                            />
                            <div
                              className="min-w-0 flex-1"
                              title={command.path}
                            >
                              <span className="block truncate font-mono text-ui-sm">
                                /{command.name}
                              </span>
                              {command.description && (
                                <span className="block truncate text-ui-xs text-muted-foreground">
                                  {command.description}
                                </span>
                              )}
                            </div>
                            {duplicate(command) && (
                              <span className="text-ui-xs text-muted-foreground">
                                {tr("Name exists")}
                              </span>
                            )}
                          </li>
                        ))}
                      </ul>
                    )}
                  </section>
                );
              })}
              {catalog?.diagnostics.map((message) => (
                <p
                  key={message}
                  className="break-words text-ui-xs text-destructive"
                >
                  {tr(message)}
                </p>
              ))}
            </ScrollArea>
          </>
        )}
        <DialogFooter>
          <Button variant="ghost" disabled={busy} onClick={onClose}>
            {tr(results ? "Done" : "Cancel")}
          </Button>
          {!results && (
            <Button
              disabled={loading || busy || !selections.length}
              onClick={() => void importSelected()}
            >
              {tr("Import selected commands")}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
