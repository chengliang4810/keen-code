import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/localized/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { AGENT_ICONS } from "@/modules/ai/components/AgentSwitcher";
import {
  type Agent,
  type AgentIconId,
  BUILTIN_AGENTS,
} from "@/modules/ai/lib/agents";
import { newAgentId, useAgentsStore } from "@/modules/ai/store/agentsStore";
import { DEFAULT_AGENT_INSTRUCTIONS } from "@/modules/ai/lib/agentInstructions";
import { useInstructionEditor } from "@/settings/lib/useInstructionEditor";
import {
  Add01Icon,
  CheckmarkCircle02Icon,
  Delete02Icon,
  Edit02Icon,
  SparklesIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useEffect, useState } from "react";
import { toast } from "sonner";

const ICON_OPTIONS: AgentIconId[] = [
  "coder",
  "architect",
  "reviewer",
  "security",
  "designer",
  "spark",
];

export function AgentsSection() {
  const tr = useTranslation();
  const customAgents = useAgentsStore((s) => s.customAgents);
  const activeAgentId = useAgentsStore((s) => s.activeId);
  const setActiveAgentId = useAgentsStore((s) => s.setActiveId);
  const upsertAgent = useAgentsStore((s) => s.upsert);
  const removeAgent = useAgentsStore((s) => s.remove);
  const hydrateAgents = useAgentsStore((s) => s.hydrate);
  const reloadAgents = useAgentsStore((s) => s.reload);
  useEffect(() => {
    void hydrateAgents();
  }, [hydrateAgents]);
  const [editingAgent, setEditingAgent] = useState<Agent | null>(null);
  return (
    <div className="flex flex-col gap-4">
      <CustomInstructionsSection />
      <section className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <h2 className="text-ui-base font-medium">{tr("Agents")}</h2>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              className="self-start"
              onClick={() => {
                void reloadAgents().catch((error) =>
                  toast.error(tr("Could not load agents"), {
                    description: String(error),
                  }),
                );
              }}
            >
              {tr("Reload from disk")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 gap-1.5 px-2 text-ui-base"
              onClick={() =>
                setEditingAgent({
                  id: newAgentId(),
                  name: "New agent",
                  description: "",
                  instructions: DEFAULT_AGENT_INSTRUCTIONS,
                  icon: "spark",
                  builtIn: false,
                })
              }
            >
              <HugeiconsIcon icon={Add01Icon} size={12} strokeWidth={1.75} />
              {tr("New agent")}
            </Button>
          </div>
        </div>
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
          {[...BUILTIN_AGENTS, ...customAgents].map((a) => (
            <AgentCard
              key={a.id}
              agent={a}
              active={a.id === activeAgentId}
              onActivate={() => setActiveAgentId(a.id)}
              onEdit={a.builtIn ? null : () => setEditingAgent(a)}
              onDelete={
                a.builtIn
                  ? null
                  : () => {
                      void removeAgent(a.id).catch((error) =>
                        toast.error(tr("Could not save agents"), {
                          description: String(error),
                        }),
                      );
                    }
              }
            />
          ))}
        </div>
      </section>
      <AgentEditorDialog
        agent={editingAgent}
        existing={customAgents}
        onClose={() => setEditingAgent(null)}
        onSave={async (a) => {
          await upsertAgent(a);
          setEditingAgent(null);
        }}
      />
    </div>
  );
}

function AgentCard({
  agent,
  active,
  onActivate,
  onEdit,
  onDelete,
}: {
  agent: Agent;
  active: boolean;
  onActivate: () => void;
  onEdit: (() => void) | null;
  onDelete: (() => void) | null;
}) {
  const tr = useTranslation();
  const Icon = AGENT_ICONS[agent.icon] ?? SparklesIcon;
  return (
    <div
      className={cn(
        "group relative flex flex-col gap-1.5 rounded-lg border bg-card/60 px-3 py-2.5 transition-colors",
        active
          ? "border-foreground/30 ring-1 ring-foreground/10"
          : "border-border/60 hover:border-border",
      )}
    >
      <div className="flex items-start gap-2">
        <div className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted/40">
          <HugeiconsIcon icon={Icon} size={14} strokeWidth={1.5} />
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="flex items-center gap-1.5 text-ui-base font-medium">
            {agent.builtIn ? tr(agent.name) : agent.name}
            {agent.builtIn ? (
              <span className="rounded bg-muted/50 px-1 py-0.5 text-ui-xs tracking-wide text-muted-foreground uppercase">
                {tr("Built-in")}
              </span>
            ) : null}
          </span>
          <span className="line-clamp-2 text-ui-sm leading-relaxed text-muted-foreground">
            {agent.builtIn ? tr(agent.description) : agent.description}
          </span>
        </div>
      </div>
      <div className="mt-0.5 flex items-center justify-between gap-1">
        <Button
          size="sm"
          variant={active ? "default" : "outline"}
          onClick={onActivate}
          className="h-6 gap-1 px-2 text-ui-base"
        >
          {active ? (
            <>
              <HugeiconsIcon
                icon={CheckmarkCircle02Icon}
                size={10}
                strokeWidth={2}
              />
              {tr("Active")}
            </>
          ) : (
            tr("Use agent")
          )}
        </Button>
        <div className="flex gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
          {onEdit ? (
            <Button
              size="icon"
              variant="ghost"
              className="size-6"
              onClick={onEdit}
              title={tr("Edit")}
            >
              <HugeiconsIcon icon={Edit02Icon} size={11} strokeWidth={1.75} />
            </Button>
          ) : null}
          {onDelete ? (
            <Button
              size="icon"
              variant="ghost"
              className="size-6 text-muted-foreground hover:text-destructive"
              onClick={onDelete}
              title={tr("Delete")}
            >
              <HugeiconsIcon icon={Delete02Icon} size={11} strokeWidth={1.75} />
            </Button>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function AgentEditorDialog({
  agent,
  existing,
  onClose,
  onSave,
}: {
  agent: Agent | null;
  existing: Agent[];
  onClose: () => void;
  onSave: (a: Agent) => Promise<void>;
}) {
  const tr = useTranslation();
  const [draft, setDraft] = useState<Agent | null>(agent);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => setDraft(agent), [agent]);
  if (!draft) return null;

  const isNew = !existing.some((a) => a.id === draft.id);
  const canSave =
    draft.name.trim().length > 0 && draft.instructions.trim().length > 0;

  return (
    <Dialog open={!!agent} onOpenChange={(o) => !o && !saving && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="text-ui-base">
            {isNew ? tr("New agent") : tr("Edit agent")}
          </DialogTitle>
        </DialogHeader>
        <div className="-mx-2 max-h-[calc(100vh-14rem)] overflow-y-auto px-2 flex flex-col gap-3">
          <div className="flex gap-2">
            <div className="flex flex-col gap-1">
              <Label>{tr("Icon")}</Label>
              <div
                role="group"
                aria-label={tr("Icon")}
                className="flex flex-wrap gap-1"
              >
                {ICON_OPTIONS.map((id) => {
                  const Icon = AGENT_ICONS[id] ?? SparklesIcon;
                  const active = draft.icon === id;
                  return (
                    <button
                      key={id}
                      type="button"
                      aria-label={id}
                      aria-pressed={active}
                      onClick={() => setDraft({ ...draft, icon: id })}
                      className={cn(
                        "flex size-7 items-center justify-center rounded-md border transition-colors",
                        active
                          ? "border-foreground/40 bg-accent"
                          : "border-border/60 hover:bg-accent/40",
                      )}
                    >
                      <HugeiconsIcon icon={Icon} size={13} strokeWidth={1.75} />
                    </button>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-1 flex-col gap-1">
              <Label>{tr("Name")}</Label>
              <Input
                aria-label={tr("Name")}
                value={draft.name}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                className="h-8 text-ui-base"
                placeholder={tr("e.g. Test Engineer")}
              />
            </div>
          </div>
          <div className="flex flex-col gap-1">
            <Label>{tr("Description")}</Label>
            <Input
              aria-label={tr("Description")}
              value={draft.description}
              onChange={(e) =>
                setDraft({ ...draft, description: e.target.value })
              }
              placeholder={tr("One line — shown in the agent picker")}
              className="h-8 text-ui-base"
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label>{tr("Instructions")}</Label>
            <Textarea
              aria-label={tr("Instructions")}
              value={draft.instructions}
              onChange={(e) =>
                setDraft({ ...draft, instructions: e.target.value })
              }
              placeholder={tr(
                "Complete instructions for this persona, including its operating rules.",
              )}
              className="min-h-40 resize-y text-ui-base leading-relaxed"
            />
          </div>
        </div>
        {error && (
          <p role="alert" className="text-ui-sm text-destructive">
            {error}
          </p>
        )}
        <DialogFooter>
          <Button variant="ghost" size="sm" disabled={saving} onClick={onClose}>
            {tr("Cancel")}
          </Button>
          <Button
            size="sm"
            disabled={!canSave || saving}
            onClick={async () => {
              setSaving(true);
              setError("");
              try {
                await onSave({ ...draft, builtIn: false });
              } catch (error) {
                setError(String(error));
              } finally {
                setSaving(false);
              }
            }}
          >
            {tr("Save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function CustomInstructionsSection() {
  const tr = useTranslation();
  const { editor, state } = useInstructionEditor();
  const { file, draft, incoming, saving, error } = state;
  return (
    <section className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <Label>{tr("Custom instructions")}</Label>
        <Button
          size="sm"
          variant="outline"
          disabled={saving || !file || !!incoming || draft === file.content}
          onClick={() => void editor.save()}
        >
          {tr("Save")}
        </Button>
      </div>
      {file && (
        <p className="break-all font-mono text-ui-xs text-muted-foreground">
          {file.path}
        </p>
      )}
      <Textarea
        aria-label={tr("Custom instructions")}
        value={draft}
        disabled={saving || !file}
        onChange={(e) => editor.edit(e.target.value)}
        className="min-h-40 resize-y bg-card/60 text-ui-base leading-relaxed"
      />
      {incoming && (
        <div className="flex flex-col gap-3 rounded-md border border-border p-3">
          <div className="flex items-center justify-between gap-3">
            <Label>{tr("File changed on disk")}</Label>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="ghost"
                disabled={saving}
                onClick={editor.discard}
              >
                {tr("Discard Changes")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={saving}
                onClick={() => void editor.save(true)}
              >
                {tr("Overwrite")}
              </Button>
            </div>
          </div>
          <Textarea
            aria-label={tr("File changed on disk")}
            value={incoming.content}
            readOnly
            className="min-h-24 resize-y bg-card/60 text-ui-base leading-relaxed"
          />
        </div>
      )}
      {error && (
        <p role="alert" className="text-ui-sm text-destructive">
          {tr(error)}
        </p>
      )}
    </section>
  );
}

function Label({ children }: { children: React.ReactNode }) {
  return (
    <span className="text-ui-base font-medium tracking-tight text-muted-foreground">
      {children}
    </span>
  );
}
