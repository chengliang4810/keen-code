import { useEffect, useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Switch } from "@/components/ui/switch";
import { Checkbox } from "@/components/ui/checkbox";
import { SectionHeader } from "@/settings/components/SectionHeader";
import {
  SubagentModelFields,
  type SubagentModelSelection,
} from "@/settings/components/SubagentModelFields";
import {
  configuredSubagents,
  parseSubagentConfig,
  resolveSubagentModel,
  SUBAGENT_COLORS,
  type ConfiguredSubagent,
  type SubagentProfile,
} from "@/modules/ai/agents/config";
import { READ_ONLY_TOOLS } from "@/modules/ai/agents/registry";
import { useSubagentsStore } from "@/modules/ai/store/subagentsStore";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { useTranslation } from "@/modules/i18n";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  Delete02Icon,
  ArrowLeft01Icon,
} from "@hugeicons/core-free-icons";

const COLOR_CLASS = {
  blue: "bg-blue-500",
  green: "bg-green-500",
  yellow: "bg-yellow-500",
  red: "bg-red-500",
  purple: "bg-purple-500",
  orange: "bg-orange-500",
  pink: "bg-pink-500",
  gray: "bg-gray-500",
};

export function SubagentsSection() {
  const tr = useTranslation();
  const { config, hydrated, hydrate, reload, update } = useSubagentsStore();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<SubagentProfile | null>(null);
  useEffect(() => {
    void hydrate().catch((e) => setError(String(e)));
  }, [hydrate]);
  const perform = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const agents = configuredSubagents(config).filter((agent) =>
    `${agent.label} ${agent.description} ${agent.id} ${agent.builtIn ? tr(agent.label) : ""}`
      .toLowerCase()
      .includes(query.toLowerCase().trim()),
  );
  const modelChange = (
    agent: ConfiguredSubagent,
    selection: SubagentModelSelection,
  ) =>
    perform(() =>
      update((current) =>
        agent.builtIn
          ? {
              ...current,
              overrides: [
                ...current.overrides.filter((entry) => entry.id !== agent.id),
                { id: agent.id as SubagentConfigBuiltinId, ...selection },
              ],
            }
          : {
              ...current,
              custom: current.custom.map((entry) =>
                entry.id === agent.id ? { ...entry, ...selection } : entry,
              ),
            },
      ),
    );

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <SectionHeader
          title={
            draft
              ? tr(
                  config.custom.some((agent) => agent.id === draft.id)
                    ? "Edit subagent"
                    : "New subagent",
                )
              : tr("Subagents")
          }
        />
        {draft ? (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={() => {
              setDraft(null);
              setError("");
            }}
          >
            <HugeiconsIcon icon={ArrowLeft01Icon} size={14} />
            {tr("Back")}
          </Button>
        ) : (
          <Button
            size="sm"
            variant="outline"
            disabled={!hydrated || busy}
            onClick={() => {
              setError("");
              setDraft({
                id: `custom-${crypto.randomUUID()}`,
                label: "",
                description: "",
                systemPrompt: "",
                tools: [...READ_ONLY_TOOLS],
                enabled: true,
                color: "blue",
                injectAgentsMd: false,
              });
            }}
          >
            <HugeiconsIcon icon={Add01Icon} size={14} />
            {tr("New subagent")}
          </Button>
        )}
      </div>
      {error && (
        <div
          role="alert"
          className="flex items-center justify-between gap-2 text-ui-sm text-destructive"
        >
          <span className="min-w-0 break-words">{error}</span>
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={() => void perform(reload)}
          >
            {tr("Reload from disk")}
          </Button>
        </div>
      )}
      {!hydrated && !error && (
        <p role="status" className="text-ui-sm text-muted-foreground">
          {tr("Loading…")}
        </p>
      )}
      {draft ? (
        <SubagentForm
          key={draft.id}
          draft={draft}
          busy={busy}
          onChange={setDraft}
          onCancel={() => setDraft(null)}
          onSave={() =>
            void perform(async () => {
              await update((current) =>
                parseSubagentConfig({
                  ...current,
                  custom: [
                    ...current.custom.filter((entry) => entry.id !== draft.id),
                    draft,
                  ],
                }),
              );
              setDraft(null);
            })
          }
        />
      ) : (
        hydrated && (
          <>
            <Input
              aria-label={tr("Search subagents...")}
              placeholder={tr("Search subagents...")}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              className="h-8"
            />
            {[false, true].map((builtIn) => {
              const group = agents.filter((agent) => agent.builtIn === builtIn);
              if (!group.length) return null;
              return (
                <section key={String(builtIn)} className="space-y-2">
                  <h3 className="text-ui-base font-medium">
                    {tr(builtIn ? "Built-in subagents" : "Installed")}{" "}
                    <span className="ml-1 text-muted-foreground">
                      {group.length}
                    </span>
                  </h3>
                  <ul className="divide-y divide-border/60 rounded-lg border border-border/60">
                    {group.map((agent) => (
                      <li
                        key={agent.id}
                        className="flex flex-wrap items-center gap-3 px-3 py-3"
                      >
                        <span
                          aria-hidden="true"
                          className={`size-2 shrink-0 rounded-full ${COLOR_CLASS[agent.color]}`}
                        />
                        <div className="min-w-40 flex-1">
                          {agent.builtIn ? (
                            <span className="text-ui-base font-medium">
                              {tr(agent.label)}
                            </span>
                          ) : (
                            <button
                              type="button"
                              disabled={busy}
                              className="text-left text-ui-base font-medium hover:underline focus-visible:underline"
                              onClick={() => {
                                setError("");
                                const { builtIn: _, ...profile } = agent;
                                setDraft(profile);
                              }}
                            >
                              {agent.label}
                            </button>
                          )}
                          <p className="mt-1 line-clamp-2 text-ui-sm text-muted-foreground">
                            {agent.builtIn
                              ? tr(agent.description)
                              : agent.description}
                          </p>
                        </div>
                        <SubagentModelFields
                          value={agent}
                          disabled={busy}
                          onChange={(selection) =>
                            void modelChange(agent, selection)
                          }
                        />
                        {!agent.builtIn && (
                          <>
                            <Switch
                              checked={agent.enabled}
                              disabled={busy}
                              aria-label={`${tr("Enabled")}: ${agent.label}`}
                              onCheckedChange={(enabled) =>
                                void perform(() =>
                                  update((current) => ({
                                    ...current,
                                    custom: current.custom.map((entry) =>
                                      entry.id === agent.id
                                        ? { ...entry, enabled }
                                        : entry,
                                    ),
                                  })),
                                )
                              }
                            />
                            <Button
                              variant="ghost"
                              size="icon"
                              disabled={busy}
                              aria-label={`${tr("Delete")}: ${agent.label}`}
                              className="size-7 text-muted-foreground hover:text-destructive"
                              onClick={() =>
                                void perform(() =>
                                  update((current) => ({
                                    ...current,
                                    custom: current.custom.filter(
                                      (entry) => entry.id !== agent.id,
                                    ),
                                  })),
                                )
                              }
                            >
                              <HugeiconsIcon icon={Delete02Icon} size={14} />
                            </Button>
                          </>
                        )}
                      </li>
                    ))}
                  </ul>
                </section>
              );
            })}
            {!agents.length && (
              <p className="text-ui-sm text-muted-foreground">
                {tr("No subagents found")}
              </p>
            )}
          </>
        )
      )}
    </div>
  );
}

type SubagentConfigBuiltinId =
  | "explore"
  | "code-review"
  | "security"
  | "general";

function SubagentForm({
  draft,
  busy,
  onChange,
  onSave,
  onCancel,
}: {
  draft: SubagentProfile;
  busy: boolean;
  onChange: (value: SubagentProfile) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const tr = useTranslation();
  const formId = useId();
  const endpoints = usePreferencesStore((s) => s.customEndpoints);
  let modelError = "";
  try {
    resolveSubagentModel(draft, {}, endpoints);
  } catch (error) {
    modelError = error instanceof Error ? tr(error.message) : String(error);
  }
  const canSave =
    draft.label.trim() &&
    draft.description.trim() &&
    draft.systemPrompt.trim() &&
    !modelError;
  return (
    <form
      className="space-y-4 rounded-lg border border-border/60 p-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (canSave && !busy) onSave();
      }}
    >
      <fieldset disabled={busy} className="space-y-4 disabled:opacity-60">
        <div className="grid gap-4 sm:grid-cols-2">
          <label
            htmlFor={`${formId}-name`}
            className="flex flex-col gap-1.5 text-ui-base"
          >
            {tr("Name")}
            <Input
              id={`${formId}-name`}
              value={draft.label}
              maxLength={128}
              placeholder="code-reviewer"
              onChange={(event) =>
                onChange({ ...draft, label: event.target.value })
              }
            />
          </label>
          <div className="space-y-1.5">
            <span className="text-ui-base">{tr("Color")}</span>
            <div
              role="radiogroup"
              aria-label={tr("Color")}
              className="flex h-9 items-center gap-1"
            >
              {SUBAGENT_COLORS.map((color) => (
                <label
                  key={color}
                  className="relative flex size-7 cursor-pointer items-center justify-center rounded-full hover:bg-accent"
                >
                  <input
                    type="radio"
                    name={`${formId}-color`}
                    value={color}
                    aria-label={tr(color)}
                    checked={draft.color === color}
                    onChange={() => onChange({ ...draft, color })}
                    className="peer sr-only"
                  />
                  <span
                    className={`size-3.5 rounded-full ${COLOR_CLASS[color]}`}
                  />
                  <span className="absolute inset-0 rounded-full peer-checked:ring-1 peer-focus-visible:ring-2 ring-ring" />
                </label>
              ))}
            </div>
          </div>
        </div>
        <label
          htmlFor={`${formId}-description`}
          className="flex flex-col gap-1.5 text-ui-base"
        >
          {tr("Description")}
          <Input
            id={`${formId}-description`}
            value={draft.description}
            maxLength={1024}
            onChange={(event) =>
              onChange({ ...draft, description: event.target.value })
            }
          />
        </label>
        <div className="space-y-1.5">
          <span className="text-ui-base">{tr("Model")}</span>
          <SubagentModelFields
            value={draft}
            disabled={busy}
            onChange={(selection) => onChange({ ...draft, ...selection })}
          />
          {modelError && (
            <p role="alert" className="text-ui-sm text-destructive">
              {modelError}
            </p>
          )}
        </div>
        <div className="space-y-2">
          <span className="text-ui-base">{tr("Allowed tools")}</span>
          <div className="grid grid-cols-2 gap-2 rounded-md border border-border/60 p-3">
            {READ_ONLY_TOOLS.map((tool) => (
              <label
                key={tool}
                htmlFor={`${formId}-${tool}`}
                className="flex items-center gap-2 text-ui-sm"
              >
                <Checkbox
                  id={`${formId}-${tool}`}
                  checked={draft.tools.includes(tool)}
                  onCheckedChange={(checked) =>
                    onChange({
                      ...draft,
                      tools: checked
                        ? [...draft.tools, tool]
                        : draft.tools.filter((name) => name !== tool),
                    })
                  }
                />
                {tool}
              </label>
            ))}
          </div>
        </div>
        <label
          htmlFor={`${formId}-prompt`}
          className="flex flex-col gap-1.5 text-ui-base"
        >
          {tr("System prompt")}
          <Textarea
            id={`${formId}-prompt`}
            value={draft.systemPrompt}
            maxLength={65536}
            className="min-h-32 max-h-64 resize-y font-mono text-ui-sm"
            onChange={(event) =>
              onChange({ ...draft, systemPrompt: event.target.value })
            }
          />
        </label>
        <label
          htmlFor={`${formId}-inject`}
          className="flex items-center justify-between rounded-md border border-border/60 px-3 py-2 text-ui-base"
        >
          {tr("Inject AGENTS.md")}
          <Switch
            id={`${formId}-inject`}
            checked={draft.injectAgentsMd}
            onCheckedChange={(injectAgentsMd) =>
              onChange({ ...draft, injectAgentsMd })
            }
          />
        </label>
      </fieldset>
      <div className="flex justify-end gap-2">
        <Button
          type="button"
          size="sm"
          variant="ghost"
          disabled={busy}
          onClick={onCancel}
        >
          {tr("Cancel")}
        </Button>
        <Button type="submit" size="sm" disabled={!canSave || busy}>
          {tr("Save")}
        </Button>
      </div>
    </form>
  );
}
