import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  extensionApi,
  type PluginConfigField,
  type PluginDetails,
} from "@/modules/ai/lib/extensions";
import { pluginConfigValues } from "@/modules/ai/lib/pluginUi";
import { useTranslation } from "@/modules/i18n";

export function PluginActionDialog({
  title,
  target,
  busy,
  error,
  onClose,
  onConfirm,
}: {
  title: string;
  target: string;
  busy: boolean;
  error: string;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const tr = useTranslation();
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent aria-describedby={undefined} showCloseButton={false}>
        <DialogTitle>{tr(title)}</DialogTitle>
        <p className="break-all text-ui-sm text-muted-foreground">{target}</p>
        {error && (
          <p role="alert" className="text-ui-sm text-destructive">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="outline" disabled={busy} onClick={onClose}>
            {tr("Cancel")}
          </Button>
          <Button disabled={busy} onClick={onConfirm}>
            {tr(busy ? "Working…" : "Confirm")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function ConfigInput({
  name,
  field,
  value,
  disabled,
  onChange,
  onError,
}: {
  name: string;
  field: PluginConfigField;
  value: unknown;
  disabled: boolean;
  onChange: (value: unknown) => void;
  onError: (error: string) => void;
}) {
  const tr = useTranslation();
  if (field.type === "boolean")
    return (
      <Switch
        aria-label={name}
        checked={value === true}
        disabled={disabled}
        onCheckedChange={onChange}
      />
    );
  if (field.type === "select") {
    const index = field.enum.findIndex(
      (option) => JSON.stringify(option) === JSON.stringify(value),
    );
    return (
      <Select
        value={index < 0 ? "unset" : String(index)}
        disabled={disabled}
        onValueChange={(next) => {
          if (next !== "unset") onChange(field.enum[Number(next)]);
        }}
      >
        <SelectTrigger aria-label={name} className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="unset" disabled>
            {tr("Not set")}
          </SelectItem>
          {field.enum.map((option, i) => (
            <SelectItem key={JSON.stringify(option)} value={String(i)}>
              {String(option)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    );
  }
  return (
    <div className="flex flex-1 gap-2">
      <Input
        aria-label={name}
        type={field.sensitive ? "password" : "text"}
        inputMode={field.type === "number" ? "decimal" : undefined}
        autoComplete="off"
        spellCheck={false}
        value={value == null ? "" : String(value)}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      {(field.type === "directory" || field.type === "file") && (
        <Button
          type="button"
          variant="outline"
          disabled={disabled}
          onClick={() => {
            void extensionApi
              .pickPath(field.type === "directory", name)
              .then((path) => {
                if (path) onChange(path);
              })
              .catch((error) => onError(String(error)));
          }}
        >
          {tr("Browse")}
        </Button>
      )}
    </div>
  );
}

export function PluginDetailsDialog({
  pluginId,
  onClose,
  onSaved,
}: {
  pluginId: string;
  onClose: () => void;
  onSaved: () => Promise<unknown>;
}) {
  const tr = useTranslation();
  const [details, setDetails] = useState<PluginDetails | null>(null);
  const [draft, setDraft] = useState<Record<string, unknown>>({});
  const [rowIds, setRowIds] = useState<Record<string, number[]>>({});
  const nextRowId = useRef(0);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const initialize = useCallback((next: PluginDetails) => {
    setDetails(next);
    setDraft(
      Object.fromEntries(
        Object.entries(next.fields)
          .filter(([, field]) => !field.sensitive)
          .flatMap(([name, field]) => {
            const value =
              next.plugin.publicUserConfig[name] ??
              field.default ??
              (field.type === "boolean" && !field.multiple ? false : undefined);
            return value === undefined || value === null ? [] : [[name, value]];
          }),
      ),
    );
    setRowIds(
      Object.fromEntries(
        Object.entries(next.fields).map(([name, field]) => {
          const value = field.sensitive
            ? undefined
            : (next.plugin.publicUserConfig[name] ?? field.default);
          return [
            name,
            field.multiple && Array.isArray(value)
              ? value.map(() => nextRowId.current++)
              : [],
          ];
        }),
      ),
    );
  }, []);
  useEffect(() => {
    let active = true;
    void extensionApi
      .details(pluginId)
      .then((next) => {
        if (active) initialize(next);
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [pluginId, initialize]);
  const save = async () => {
    if (!details || busy) return;
    setBusy(true);
    setError("");
    try {
      await extensionApi.configure(
        pluginId,
        pluginConfigValues(details.fields, draft),
      );
      setDraft({});
      initialize(await extensionApi.details(pluginId));
      await onSaved();
    } catch (cause) {
      setError(tr(cause instanceof Error ? cause.message : String(cause)));
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
      <DialogContent
        aria-describedby={undefined}
        showCloseButton={false}
        className="max-h-[85vh] overflow-y-auto sm:max-w-lg"
      >
        <DialogTitle className="break-all">{pluginId}</DialogTitle>
        {loading && (
          <p role="status" className="text-ui-sm text-muted-foreground">
            {tr("Loading details…")}
          </p>
        )}
        {error && (
          <p role="alert" className="text-ui-sm text-destructive">
            {error}
          </p>
        )}
        {details && (
          <>
            {details.description && (
              <p className="text-ui-sm text-muted-foreground">
                {details.description}
              </p>
            )}
            <dl className="grid grid-cols-3 gap-3 text-ui-sm">
              {Object.entries(details.components).map(([name, count]) => (
                <div key={name}>
                  <dt className="text-muted-foreground">{tr(name)}</dt>
                  <dd>{count}</dd>
                </div>
              ))}
            </dl>
            <p className="break-all font-mono text-ui-xs text-muted-foreground">
              {details.plugin.installPath}
            </p>
            <form
              className="flex flex-col gap-4"
              onSubmit={(event) => {
                event.preventDefault();
                void save();
              }}
            >
              {Object.entries(details.fields).map(([name, field]) => {
                const title = field.title ?? name;
                const values = field.multiple
                  ? Array.isArray(draft[name])
                    ? (draft[name] as unknown[])
                    : []
                  : [draft[name]];
                const update = (value: unknown) =>
                  setDraft((previous) => ({ ...previous, [name]: value }));
                return (
                  <fieldset
                    key={name}
                    disabled={busy}
                    className="flex flex-col gap-2"
                  >
                    <legend className="mb-2 flex items-center gap-2 text-ui-sm">
                      {title}
                      {field.required && (
                        <>
                          <span aria-hidden="true">*</span>
                          <span className="sr-only">{tr("Required")}</span>
                        </>
                      )}
                      {field.sensitive && (
                        <span className="text-ui-xs text-muted-foreground">
                          {tr("Sensitive")}
                          {details.plugin.sensitiveUserConfigKeys.includes(name)
                            ? ` · ${tr("Configured")}`
                            : ""}
                        </span>
                      )}
                    </legend>
                    {values.map((value, index) => (
                      <div
                        key={field.multiple ? rowIds[name]?.[index] : name}
                        className="flex items-center gap-2"
                      >
                        <ConfigInput
                          name={
                            field.multiple ? `${title} ${index + 1}` : title
                          }
                          field={field}
                          value={value}
                          disabled={busy}
                          onError={setError}
                          onChange={(next) =>
                            update(
                              field.multiple
                                ? values.map((entry, i) =>
                                    i === index ? next : entry,
                                  )
                                : next,
                            )
                          }
                        />
                        {field.multiple && (
                          <Button
                            type="button"
                            size="sm"
                            variant="ghost"
                            aria-label={tr("Remove value")}
                            onClick={() => {
                              setRowIds((previous) => ({
                                ...previous,
                                [name]: previous[name].filter(
                                  (_, i) => i !== index,
                                ),
                              }));
                              update(values.filter((_, i) => i !== index));
                            }}
                          >
                            {tr("Remove")}
                          </Button>
                        )}
                      </div>
                    ))}
                    {field.multiple && (
                      <Button
                        type="button"
                        className="self-start"
                        size="sm"
                        variant="outline"
                        onClick={() => {
                          const rowId = nextRowId.current++;
                          setRowIds((previous) => ({
                            ...previous,
                            [name]: [...(previous[name] ?? []), rowId],
                          }));
                          update([
                            ...values,
                            field.type === "boolean" ? false : "",
                          ]);
                        }}
                      >
                        {tr("Add value")}
                      </Button>
                    )}
                  </fieldset>
                );
              })}
              {Object.keys(details.fields).length === 0 && (
                <p className="text-ui-sm text-muted-foreground">
                  {tr("This plugin has no configurable fields.")}
                </p>
              )}
              <div className="flex justify-end gap-2">
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={onClose}
                >
                  {tr("Close")}
                </Button>
                {Object.keys(details.fields).length > 0 && (
                  <Button type="submit" disabled={busy}>
                    {tr(busy ? "Saving…" : "Save configuration")}
                  </Button>
                )}
              </div>
            </form>
          </>
        )}
        {!details && !loading && (
          <Button variant="outline" onClick={onClose}>
            {tr("Close")}
          </Button>
        )}
      </DialogContent>
    </Dialog>
  );
}
