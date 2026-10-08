import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useTranslation } from "@/modules/i18n";
import {
  extensionApi,
  parseExtensionObject,
  type ExtensionConfig,
  type McpServerEntry,
} from "@/modules/ai/lib/extensions";
import { SectionHeader } from "@/settings/components/SectionHeader";

export function ExtensionsSection() {
  const tr = useTranslation();
  const [config, setConfig] = useState<ExtensionConfig>({ mcpServers: [] });
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [adding, setAdding] = useState(false);
  const pending = useRef(false);
  useEffect(() => {
    let active = true;
    void extensionApi
      .get()
      .then((next) => {
        if (active) setConfig(next);
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
    if (pending.current) throw new Error(tr("Working…"));
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      await operation();
      setConfig(await extensionApi.get());
    } catch (cause) {
      setError(String(cause));
      throw cause;
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  const saveServer = (entry: McpServerEntry, oldId?: string) =>
    run(() =>
      extensionApi.save({
        mcpServers: oldId
          ? config.mcpServers.map((server) =>
              server.id === oldId ? entry : server,
            )
          : [...config.mcpServers, entry],
      }),
    );
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-start justify-between gap-3">
        <SectionHeader title={tr("MCP servers")} />
        <Button
          size="sm"
          variant="outline"
          disabled={busy}
          onClick={() => setAdding(!adding)}
        >
          {tr(adding ? "Cancel" : "Add server")}
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
      <section className="flex flex-col gap-4" aria-label={tr("MCP servers")}>
        {!busy && !config.mcpServers.length && !adding && (
          <p className="rounded-lg border border-dashed border-border/60 px-4 py-8 text-center text-ui-sm text-muted-foreground">
            {tr(
              "No MCP servers configured. Add a server to connect external tools.",
            )}
          </p>
        )}
        {config.mcpServers.map((server) => (
          <McpEditor
            key={server.id}
            server={server}
            busy={busy}
            onSave={(entry) => saveServer(entry, server.id)}
            onCredentials={(token, environment) =>
              run(() => extensionApi.credentials(server.id, token, environment))
            }
            onRemove={() =>
              run(() =>
                extensionApi.save({
                  mcpServers: config.mcpServers.filter(
                    (entry) => entry.id !== server.id,
                  ),
                }),
              )
            }
          />
        ))}
        {adding && (
          <McpEditor
            key="new-mcp"
            busy={busy}
            onSave={(entry) => saveServer(entry).then(() => setAdding(false))}
          />
        )}
      </section>
    </div>
  );
}
function McpEditor({
  server,
  busy,
  onSave,
  onCredentials,
  onRemove,
}: {
  server?: McpServerEntry;
  busy: boolean;
  onSave: (entry: McpServerEntry) => Promise<unknown>;
  onCredentials?: (
    token: string | null,
    environment: Record<string, string> | null,
  ) => Promise<unknown>;
  onRemove?: () => Promise<unknown>;
}) {
  const tr = useTranslation();
  const [id, setId] = useState(server?.id ?? "");
  const [transport, setTransport] = useState<"stdio" | "http">(
    server?.transport ?? "stdio",
  );
  const [command, setCommand] = useState(server?.command ?? "");
  const [args, setArgs] = useState(JSON.stringify(server?.args ?? []));
  const [url, setUrl] = useState(server?.url ?? "");
  const [enabled, setEnabled] = useState(server?.enabled ?? true);
  const [token, setToken] = useState("");
  const [environment, setEnvironment] = useState("");
  const [tokenEdited, setTokenEdited] = useState(false);
  const [error, setError] = useState("");
  const save = async () => {
    setError("");
    try {
      const parsed: unknown = JSON.parse(args);
      if (!Array.isArray(parsed) || !parsed.every((v) => typeof v === "string"))
        throw new Error(tr("Arguments must be a JSON array of strings"));
      await onSave({
        id: id.trim(),
        enabled,
        transport,
        command: command.trim(),
        args: parsed as string[],
        url: url.trim(),
      });
      if (!server) {
        setId("");
        setCommand("");
        setArgs("[]");
        setUrl("");
      }
    } catch (e) {
      setError(String(e));
    }
  };
  const saveCredentials = async () => {
    setError("");
    try {
      const env = environment.trim() ? parseExtensionObject(environment) : null;
      if (
        env &&
        !Object.values(env).every((value) => typeof value === "string")
      )
        throw new Error(tr("Environment values must be strings"));
      await onCredentials?.(
        tokenEdited ? token : null,
        env as Record<string, string> | null,
      );
      setToken("");
      setTokenEdited(false);
      setEnvironment("");
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border/60 p-4">
      <div className="flex items-center gap-3">
        <Input
          aria-label={tr("Server ID")}
          value={id}
          readOnly={!!server}
          placeholder={tr("Server ID")}
          onChange={(e) => setId(e.target.value)}
          spellCheck={false}
        />
        <Switch
          checked={enabled}
          onCheckedChange={setEnabled}
          disabled={busy}
          aria-label={tr("Enable server")}
        />
      </div>
      <Select
        value={transport}
        onValueChange={(v) => setTransport(v as "stdio" | "http")}
      >
        <SelectTrigger aria-label={tr("Transport")}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="stdio">stdio</SelectItem>
          <SelectItem value="http">Streamable HTTP</SelectItem>
        </SelectContent>
      </Select>
      {transport === "stdio" ? (
        <>
          <Input
            aria-label={tr("Executable")}
            placeholder={tr("Executable")}
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            spellCheck={false}
          />
          <Input
            aria-label={tr("Arguments (JSON array)")}
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            className="font-mono"
            spellCheck={false}
          />
        </>
      ) : (
        <Input
          aria-label={tr("Server URL")}
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://example.com/mcp"
          spellCheck={false}
        />
      )}
      <div className="flex gap-2">
        <Button disabled={busy || !id.trim()} onClick={() => void save()}>
          {tr(server ? "Save server" : "Add server")}
        </Button>
        {onRemove && (
          <Button
            variant="outline"
            disabled={busy}
            onClick={() => void onRemove().catch(() => {})}
          >
            {tr("Remove")}
          </Button>
        )}
      </div>
      {server && (
        <details>
          <summary className="cursor-pointer text-ui-sm text-muted-foreground">
            {tr("Credentials and environment")}
          </summary>
          <div className="mt-3 flex flex-col gap-3">
            <Input
              type="password"
              autoComplete="new-password"
              aria-label={tr("Bearer token")}
              placeholder={tr("Leave untouched to keep the stored token")}
              value={token}
              onChange={(e) => {
                setToken(e.target.value);
                setTokenEdited(true);
              }}
            />
            <Textarea
              aria-label={tr("Environment (JSON)")}
              placeholder='{"KEY":"value"}'
              value={environment}
              onChange={(e) => setEnvironment(e.target.value)}
              className="min-h-16 font-mono text-ui-sm"
              spellCheck={false}
            />
            <p className="text-ui-xs text-muted-foreground">
              {tr(
                "Secrets use the key store. Blank environment keeps existing values; {} clears them.",
              )}
            </p>
            <Button
              variant="outline"
              className="self-start"
              disabled={busy || (!tokenEdited && !environment.trim())}
              onClick={() => void saveCredentials()}
            >
              {tr("Save credentials")}
            </Button>
          </div>
        </details>
      )}
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
