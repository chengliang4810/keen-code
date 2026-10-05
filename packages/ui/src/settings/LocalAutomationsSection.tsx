import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import type { ZCodeAutomation, ZCodeAutomationRun } from "@zcode/shared";
import {
  TID_AUTOMATIONS_LIST,
  TID_AUTOMATION_ACTION_DELETE,
  TID_AUTOMATION_ACTION_HISTORY,
  TID_AUTOMATION_ACTION_TOGGLE,
  TID_AUTOMATION_CARD,
  TID_AUTOMATION_CARD_MENU,
  TID_AUTOMATION_CREATE_MANUALLY,
  TID_AUTOMATION_EDIT_BACK,
  TID_AUTOMATION_FORM_PROMPT,
  TID_AUTOMATION_FORM_SUBMIT,
  TID_AUTOMATION_FORM_TITLE,
  TID_AUTOMATION_RUN_NOW,
  TID_AUTOMATION_RUN_OPEN_SESSION,
} from "@zcode/shared";
import { Button } from "@/components/ui/button.js";
import { Input } from "@/components/ui/input.js";
import { Spinner } from "@/components/ui/spinner.js";
import { Textarea } from "@/components/ui/textarea.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu.js";
import { toast } from "@/components/ui/toast.js";
import { useConfirmDialog } from "@/hooks/useConfirmDialog.js";
import { useWorkspaceServicesResolution } from "@/hooks/useWorkspaceServices.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { cn } from "@/components/lib/utils.js";
import { useAutomationManagementStore } from "@/store/automationManagementStore.js";
import { AutomationSwitchToggle } from "@/settings/AutomationSwitchToggle.js";
import {
  AutomationEditActionIcon,
  AutomationMoreHorizontalIcon,
  AutomationRefreshIcon,
  AutomationRunNowIcon,
  AutomationTrashIcon,
} from "@/settings/AutomationDesignPrimitives.js";
import {
  describeCron,
  formatAutomationCardNextRun,
  formatDateTime,
  resolveAutomationStatusKind,
} from "@/settings/automationFormat.js";
import { resolveAutomationEditRequiredFieldErrors } from "@/settings/automationEditValidation.js";
import { SETTINGS_FRAME_CONTENT_CLASSNAME } from "@/settings/SettingsPageParts.js";

type AutomationView =
  | { kind: "list" }
  | { kind: "form"; automationId?: string }
  | { kind: "history"; automationId: string };

interface AutomationFormState {
  title: string;
  cronExpr: string;
  prompt: string;
  recurring: boolean;
  maxRuns: string;
}

interface LocalAutomationsSectionProps {
  header?: ReactNode;
  workspacePath?: string | null;
  workspaceIdentity?: string;
  openAutomationId?: string | null;
  onOpenAutomationConsumed?: () => void;
  onOpenSession?: (params: {
    sessionId: string;
    workspacePath: string;
    workspaceIdentity?: string;
  }) => void;
}

const DEFAULT_FORM: AutomationFormState = {
  title: "",
  cronExpr: "0 9 * * 1-5",
  prompt: "",
  recurring: true,
  maxRuns: "",
};

function formFromAutomation(automation: ZCodeAutomation): AutomationFormState {
  return {
    title: automation.title,
    cronExpr: automation.cronExpr,
    prompt: automation.prompt,
    recurring: automation.recurring,
    maxRuns: automation.maxRuns ? String(automation.maxRuns) : "",
  };
}

function runStatusLabel(run: ZCodeAutomationRun, intl: ReturnType<typeof useZCodeIntl>["intl"]): string {
  if (run.outcome === "running") return intl.formatMessage({ id: "automations.runs.status.running" });
  if (run.outcome === "succeeded") return intl.formatMessage({ id: "automations.runs.status.succeeded" });
  if (run.outcome === "stopped") return intl.formatMessage({ id: "automations.runs.status.stopped" });
  if (run.outcome === "failed" || run.dispatchStatus === "failed_to_dispatch") {
    return intl.formatMessage({ id: "automations.runs.status.failed" });
  }
  return run.dispatchStatus;
}

function normalizeAutomationWorkspacePath(path: string): string {
  const normalized = path.trim().replaceAll("\\", "/").replace(/\/+$/u, "");
  // Rust 的 canonicalize 在 Windows 可能返回 \\?\ 扩展路径，而工作区 tab
  // 仍保存普通盘符路径；两者指向同一目录时必须进入同一个本地 automation 列表。
  if (normalized.startsWith("//?/UNC/")) return `//${normalized.slice("//?/UNC/".length)}`.toLowerCase();
  if (normalized.startsWith("//?/")) return normalized.slice("//?/".length).toLowerCase();
  return normalized.toLowerCase();
}

function automationMatchesWorkspace(
  automation: ZCodeAutomation,
  workspacePath: string,
  workspaceIdentity?: string,
): boolean {
  if (workspaceIdentity?.trim()) {
    return (
      automation.workspaceIdentity === workspaceIdentity || automation.workspaceKey === workspaceIdentity
    );
  }
  return (
    normalizeAutomationWorkspacePath(automation.workspacePath) ===
    normalizeAutomationWorkspacePath(workspacePath)
  );
}

/**
 * 本地 automation 管理页。列表、表单、启停、立即运行和历史都通过 zcode-agent
 * RPC 进入 Rust 的 automation store；组件不维护第二份持久化状态。
 */
export function LocalAutomationsSection({
  header,
  workspacePath,
  workspaceIdentity,
  openAutomationId,
  onOpenAutomationConsumed,
  onOpenSession,
}: LocalAutomationsSectionProps) {
  const { intl } = useZCodeIntl();
  const requestConfirmation = useConfirmDialog();
  const resolution = useWorkspaceServicesResolution(workspacePath, undefined, workspaceIdentity);
  const agentService = resolution.services.zcodeAgentService;
  const rpcReady = resolution.rpcReady && !resolution.isRemoteTarget;
  const automations = useAutomationManagementStore((state) => state.automations);
  const loading = useAutomationManagementStore((state) => state.loading);
  const error = useAutomationManagementStore((state) => state.error);
  const operationId = useAutomationManagementStore((state) => state.operationId);
  const runsCache = useAutomationManagementStore((state) => state.runsCache);
  const initialize = useAutomationManagementStore((state) => state.initialize);
  const refresh = useAutomationManagementStore((state) => state.refresh);
  const createAutomation = useAutomationManagementStore((state) => state.createAutomation);
  const updateAutomation = useAutomationManagementStore((state) => state.updateAutomation);
  const deleteAutomation = useAutomationManagementStore((state) => state.deleteAutomation);
  const setEnabled = useAutomationManagementStore((state) => state.setEnabled);
  const restartAutomation = useAutomationManagementStore((state) => state.restartAutomation);
  const runAutomationNow = useAutomationManagementStore((state) => state.runAutomationNow);
  const loadRuns = useAutomationManagementStore((state) => state.loadRuns);
  const deleteRun = useAutomationManagementStore((state) => state.deleteRun);

  const [view, setView] = useState<AutomationView>({ kind: "list" });
  const [form, setForm] = useState<AutomationFormState>(DEFAULT_FORM);
  const [requiredErrors, setRequiredErrors] = useState<Set<"title" | "schedule" | "prompt">>(
    new Set(),
  );
  const consumedOpenId = useRef<string | null>(null);

  const scopedAutomations = useMemo(() => {
    if (!workspacePath || !rpcReady) return [];
    return automations.filter((automation) =>
      automationMatchesWorkspace(automation, workspacePath, workspaceIdentity),
    );
  }, [automations, rpcReady, workspaceIdentity, workspacePath]);

  useEffect(() => {
    if (!workspacePath || !rpcReady) return;
    void initialize({
      workspacePath,
      ...(workspaceIdentity ? { workspaceIdentity } : {}),
      agentService,
    });
  }, [agentService, initialize, rpcReady, workspaceIdentity, workspacePath]);

  useEffect(() => {
    if (!openAutomationId || consumedOpenId.current === openAutomationId) return;
    if (loading) return;
    const automation = scopedAutomations.find((candidate) => candidate.automationId === openAutomationId);
    consumedOpenId.current = openAutomationId;
    if (automation) {
      setForm(formFromAutomation(automation));
      setRequiredErrors(new Set());
      setView({ kind: "form", automationId: automation.automationId });
    }
    onOpenAutomationConsumed?.();
  }, [loading, onOpenAutomationConsumed, openAutomationId, scopedAutomations]);

  const openCreate = useCallback(() => {
    setRequiredErrors(new Set());
    setForm(DEFAULT_FORM);
    setView({ kind: "form" });
  }, []);

  const openEdit = useCallback((automation: ZCodeAutomation) => {
    setRequiredErrors(new Set());
    setForm(formFromAutomation(automation));
    setView({ kind: "form", automationId: automation.automationId });
  }, []);

  const handleRefresh = useCallback(() => {
    if (rpcReady) void refresh(agentService);
  }, [agentService, refresh, rpcReady]);

  const handleFormSubmit = useCallback(
    async (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const missing = resolveAutomationEditRequiredFieldErrors({
        title: form.title,
        cronExpr: form.cronExpr,
        prompt: form.prompt,
      });
      setRequiredErrors(new Set(missing));
      if (missing.length > 0 || !workspacePath || !rpcReady) return;

      const maxRuns = form.maxRuns.trim() ? Number(form.maxRuns) : undefined;
      if (!form.recurring && (!Number.isSafeInteger(maxRuns) || (maxRuns ?? 0) <= 0)) {
        setRequiredErrors(new Set(["schedule"]));
        return;
      }

      if (view.kind === "form" && view.automationId) {
        const saved = await updateAutomation(
          view.automationId,
          {
            title: form.title.trim(),
            cronExpr: form.cronExpr.trim(),
            prompt: form.prompt.trim(),
            recurring: form.recurring,
            maxRuns: form.recurring ? null : maxRuns,
            scheduleEditedByUser: true,
          },
          agentService,
        );
        if (!saved) return;
        toast(intl.formatMessage({ id: "automations.form.save" }));
      } else {
        const created = await createAutomation(
          {
            title: form.title.trim(),
            cronExpr: form.cronExpr.trim(),
            prompt: form.prompt.trim(),
            recurring: form.recurring,
            ...(form.recurring ? {} : { maxRuns }),
            workspacePath,
            ...(workspaceIdentity ? { workspaceIdentity } : {}),
          },
          agentService,
        );
        if (!created) return;
        toast(intl.formatMessage({ id: "automations.createdLabel" }));
      }
      setView({ kind: "list" });
    },
    [
      agentService,
      createAutomation,
      form,
      intl,
      rpcReady,
      updateAutomation,
      view,
      workspaceIdentity,
      workspacePath,
    ],
  );

  const handleDelete = useCallback(
    async (automation: ZCodeAutomation) => {
      const confirmed = await requestConfirmation({
        title: intl.formatMessage({ id: "automations.delete.title" }),
        description: intl.formatMessage(
          { id: "automations.delete.description" },
          { title: automation.title },
        ),
        testId: `automation-delete-confirm-${automation.automationId}`,
        presentation: "automation-confirmation",
        confirmLabel: intl.formatMessage({ id: "automations.delete" }),
        confirmVariant: "destructive",
      });
      if (!confirmed) return;
      await deleteAutomation(automation.automationId, agentService);
      setView((current) =>
        current.kind === "history" && current.automationId === automation.automationId
          ? { kind: "list" }
          : current,
      );
    },
    [agentService, deleteAutomation, intl, requestConfirmation],
  );

  const handleRunNow = useCallback(
    async (automation: ZCodeAutomation) => {
      const result = await runAutomationNow(automation.automationId, agentService);
      if (result === "queued") {
        toast(intl.formatMessage({ id: "automations.runNowQueued" }));
      } else if (result === "duplicate") {
        toast(intl.formatMessage({ id: "automations.runNowAlreadyRunning" }));
      } else {
        toast(intl.formatMessage({ id: "automations.runNowFailed" }));
      }
    },
    [agentService, intl, runAutomationNow],
  );

  const handleToggle = useCallback(
    (automation: ZCodeAutomation) => {
      void setEnabled(automation.automationId, !automation.enabled, agentService);
    },
    [agentService, setEnabled],
  );

  const handleOpenHistory = useCallback(
    (automation: ZCodeAutomation) => {
      setView({ kind: "history", automationId: automation.automationId });
      void loadRuns(automation.automationId, agentService, true);
    },
    [agentService, loadRuns],
  );

  const selectedHistory =
    view.kind === "history"
      ? scopedAutomations.find((automation) => automation.automationId === view.automationId)
      : undefined;

  if (!workspacePath || !rpcReady) {
    return (
      <div data-automations-content className={cn(SETTINGS_FRAME_CONTENT_CLASSNAME, "flex flex-col")}>
        {header}
        <p className="mt-8 text-ui-base text-foreground-subtle">
          {intl.formatMessage({ id: "automations.form.project.localRequired" })}
        </p>
      </div>
    );
  }

  if (view.kind === "form") {
    const editing = view.automationId
      ? scopedAutomations.find((automation) => automation.automationId === view.automationId)
      : undefined;
    return (
      <div data-automations-content className={cn(SETTINGS_FRAME_CONTENT_CLASSNAME, "flex flex-col")}>
        <div className="flex items-center gap-3">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={intl.formatMessage({ id: "automations.edit.tab.settings" })}
            data-testid={TID_AUTOMATION_EDIT_BACK}
            onClick={() => setView({ kind: "list" })}
          >
            <ArrowLeft className="size-4" aria-hidden="true" />
          </Button>
          <div>
            <h1 className="text-ui-xl font-medium text-foreground">
              {intl.formatMessage({
                id: editing ? "automations.form.editTitle" : "automations.form.createTitle",
              })}
            </h1>
            <p className="mt-1 text-ui-base text-foreground-subtle">
              {intl.formatMessage({
                id: editing
                  ? "automations.edit.editSubtitle"
                  : "automations.edit.createSubtitle",
              })}
            </p>
          </div>
        </div>
        <form className="mt-8 flex max-w-2xl flex-col gap-5" onSubmit={handleFormSubmit}>
          <label className="flex flex-col gap-1.5 text-ui-base text-foreground">
            <span>{intl.formatMessage({ id: "automations.form.title.label" })}</span>
            <Input
              data-testid={TID_AUTOMATION_FORM_TITLE}
              value={form.title}
              placeholder={intl.formatMessage({ id: "automations.form.title.placeholder" })}
              aria-invalid={requiredErrors.has("title")}
              onChange={(event) => {
                setForm((current) => ({ ...current, title: event.target.value }));
                setRequiredErrors((current) => {
                  const next = new Set(current);
                  next.delete("title");
                  return next;
                });
              }}
            />
          </label>
          <label className="flex flex-col gap-1.5 text-ui-base text-foreground">
            <span>{intl.formatMessage({ id: "automations.form.schedule.label" })}</span>
            <Input
              data-testid="automation-form-schedule"
              value={form.cronExpr}
              placeholder="0 9 * * 1-5"
              aria-invalid={requiredErrors.has("schedule")}
              onChange={(event) => setForm((current) => ({ ...current, cronExpr: event.target.value }))}
            />
            <span className="text-ui-sm text-foreground-subtle">
              {intl.formatMessage(
                { id: "automations.form.schedule.preview" },
                { summary: describeCron(form.cronExpr, intl) },
              )}
            </span>
          </label>
          <label className="flex flex-col gap-1.5 text-ui-base text-foreground">
            <span>{intl.formatMessage({ id: "automations.form.prompt.label" })}</span>
            <Textarea
              data-testid={TID_AUTOMATION_FORM_PROMPT}
              rows={5}
              value={form.prompt}
              placeholder={intl.formatMessage({ id: "automations.form.prompt.placeholder" })}
              aria-invalid={requiredErrors.has("prompt")}
              onChange={(event) => {
                setForm((current) => ({ ...current, prompt: event.target.value }));
                setRequiredErrors((current) => {
                  const next = new Set(current);
                  next.delete("prompt");
                  return next;
                });
              }}
            />
          </label>
          <div className="flex items-center justify-between gap-3 border-t border-border pt-4">
            <div>
              <div className="text-ui-base font-medium text-foreground">
                {intl.formatMessage({ id: "automations.form.recurring.label" })}
              </div>
              <div className="mt-1 text-ui-sm text-foreground-subtle">
                {intl.formatMessage({ id: "automations.form.recurring.hint" })}
              </div>
            </div>
            <AutomationSwitchToggle
              checked={form.recurring}
              ariaLabel={intl.formatMessage({ id: "automations.form.recurring.label" })}
              onChange={(recurring) => setForm((current) => ({ ...current, recurring }))}
            />
          </div>
          {!form.recurring ? (
            <label className="flex max-w-xs flex-col gap-1.5 text-ui-base text-foreground">
              <span>{intl.formatMessage({ id: "automations.form.maxRuns.label" })}</span>
              <Input
                type="text"
                inputMode="numeric"
                pattern="[0-9]*"
                value={form.maxRuns}
                placeholder={intl.formatMessage({ id: "automations.form.maxRuns.placeholder" })}
                aria-invalid={requiredErrors.has("schedule")}
                onChange={(event) => setForm((current) => ({ ...current, maxRuns: event.target.value }))}
              />
            </label>
          ) : null}
          {requiredErrors.size > 0 ? (
            <p className="text-ui-sm text-destructive" role="alert">
              {intl.formatMessage({ id: "automations.error.update" })}
            </p>
          ) : null}
          {error ? (
            <p className="text-ui-sm text-destructive" role="alert">
              {error}
            </p>
          ) : null}
          <div className="flex items-center gap-2">
            <Button
              type="submit"
              data-testid={TID_AUTOMATION_FORM_SUBMIT}
              disabled={Boolean(operationId)}
            >
              {intl.formatMessage({
                id: operationId ? "automations.form.saving" : "automations.form.save",
              })}
            </Button>
            <Button type="button" variant="outline" onClick={() => setView({ kind: "list" })}>
              {intl.formatMessage({ id: "automations.form.cancel" })}
            </Button>
          </div>
        </form>
      </div>
    );
  }

  if (view.kind === "history" && selectedHistory) {
    const cache = runsCache[selectedHistory.automationId];
    const runs = cache?.runs ?? [];
    return (
      <div data-automations-content className={cn(SETTINGS_FRAME_CONTENT_CLASSNAME, "flex flex-col")}>
        <div className="flex items-center gap-3">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={intl.formatMessage({ id: "automations.edit.tab.settings" })}
            data-testid={TID_AUTOMATION_EDIT_BACK}
            onClick={() => setView({ kind: "list" })}
          >
            <ArrowLeft className="size-4" aria-hidden="true" />
          </Button>
          <div className="min-w-0">
            <h1 className="truncate text-ui-xl font-medium text-foreground">{selectedHistory.title}</h1>
            <p className="mt-1 text-ui-base text-foreground-subtle">
              {intl.formatMessage({ id: "automations.runs.title" })}
            </p>
          </div>
        </div>
        <div className="mt-8 overflow-hidden rounded-xl border border-border bg-card">
          <div className="flex items-center justify-between border-b border-border px-4 py-3">
            <span className="text-ui-base font-medium text-foreground">
              {intl.formatMessage({ id: "automations.runs.title" })}
            </span>
          <Button
            type="button"
            variant="outline"
            size="icon"
            aria-label={intl.formatMessage({ id: "automations.refresh" })}
            data-testid="automations-refresh"
            onClick={() => void loadRuns(selectedHistory.automationId, agentService, true)}
            >
              <AutomationRefreshIcon className="size-3.5" aria-hidden="true" />
            </Button>
          </div>
          {cache?.status === "loading" ? (
            <div className="flex h-28 items-center justify-center">
              <Spinner className="size-5" />
            </div>
          ) : runs.length === 0 ? (
            <p className="px-4 py-8 text-center text-ui-base text-foreground-subtle">
              {intl.formatMessage({ id: "automations.runs.empty" })}
            </p>
          ) : (
            <div className="divide-y divide-border">
              {runs.map((run) => (
                <div
                  key={run.runId}
                  data-testid={`automation-run-row-${run.runId}`}
                  className="flex items-center gap-3 px-4 py-3"
                >
                  <div className="min-w-0 flex-1">
                    <div className="text-ui-base text-foreground">
                      {runStatusLabel(run, intl)}
                    </div>
                    <div className="mt-1 text-ui-sm text-foreground-subtle">
                      {formatDateTime(run.createdAt)} · {intl.formatMessage({ id: `automations.trigger.${run.trigger}` })}
                    </div>
                    {run.error ? <div className="mt-1 truncate text-ui-sm text-destructive">{run.error}</div> : null}
                  </div>
                  {run.sessionId && onOpenSession ? (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      data-testid={TID_AUTOMATION_RUN_OPEN_SESSION}
                      onClick={() =>
                        onOpenSession({
                          sessionId: run.sessionId!,
                          workspacePath: selectedHistory.workspacePath,
                          ...(selectedHistory.workspaceIdentity
                            ? { workspaceIdentity: selectedHistory.workspaceIdentity }
                            : {}),
                        })
                      }
                    >
                      {intl.formatMessage({ id: "automations.runs.openSession" })}
                    </Button>
                  ) : null}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: "automations.runs.delete" })}
                    onClick={() =>
                      void deleteRun(selectedHistory.automationId, run.runId, agentService)
                    }
                  >
                    <AutomationTrashIcon />
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    );
  }

  const now = Date.now();
  return (
    <div data-automations-content className={cn(SETTINGS_FRAME_CONTENT_CLASSNAME, "flex flex-col")}>
      {header}
      <div className="mt-8 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-ui-base font-medium leading-5 text-foreground">
            {intl.formatMessage({ id: "automations.list.title" })}
            {scopedAutomations.length > 0 ? (
              <span className="ml-1 font-normal text-foreground-subtlest">{scopedAutomations.length}</span>
            ) : null}
          </h2>
          <p className="mt-1 text-ui-base text-foreground-subtle">
            {intl.formatMessage({ id: "automations.description.populated" })}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="icon"
            aria-label={intl.formatMessage({ id: "automations.refresh" })}
            data-testid="automations-refresh"
            onClick={handleRefresh}
          >
            <AutomationRefreshIcon className="size-3.5" aria-hidden="true" />
          </Button>
          <Button
            type="button"
            data-testid={TID_AUTOMATION_CREATE_MANUALLY}
            data-icon="inline-start"
            onClick={openCreate}
          >
            <AutomationEditActionIcon className="size-3.5" aria-hidden="true" />
            {intl.formatMessage({ id: "automations.createManually" })}
          </Button>
        </div>
      </div>
      {error ? (
        <p className="mt-4 text-ui-sm text-destructive" role="alert">
          {error}
        </p>
      ) : null}
      <div data-testid={TID_AUTOMATIONS_LIST} className="mt-5 flex flex-col gap-3">
        {loading && scopedAutomations.length === 0 ? (
          <div className="flex h-36 items-center justify-center">
            <Spinner className="size-5" />
          </div>
        ) : scopedAutomations.length === 0 ? (
          <div className="flex min-h-[226px] flex-col items-center justify-center rounded-xl border border-dashed border-card-border bg-background px-4 text-center">
            <p className="text-ui-base font-medium text-foreground-subtle">
              {intl.formatMessage({ id: "automations.empty.title" })}
            </p>
            <p className="mt-1 text-ui-base text-foreground-subtle">
              {intl.formatMessage({ id: "automations.empty.description" })}
            </p>
            <Button className="mt-5" type="button" onClick={openCreate}>
              {intl.formatMessage({ id: "automations.empty.createManually" })}
            </Button>
          </div>
        ) : (
          scopedAutomations.map((automation) => {
            const status = resolveAutomationStatusKind(automation);
            const nextRun = formatAutomationCardNextRun(automation.nextRunAt, now, intl);
            const busy = operationId?.includes(automation.automationId) ?? false;
            return (
              <div
                key={automation.automationId}
                data-testid={TID_AUTOMATION_CARD}
                data-automation-id={automation.automationId}
                className="rounded-xl border border-card-border bg-background p-4"
              >
                <div className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex min-w-0 items-center gap-2">
                      <h3 className="truncate text-ui-base font-medium text-foreground">{automation.title}</h3>
                      <span className="shrink-0 rounded-md bg-surface px-2 py-0.5 text-ui-xs text-foreground-subtle">
                        {intl.formatMessage({ id: `automations.lifecycle.${status}` })}
                      </span>
                    </div>
                    <p className="mt-2 line-clamp-2 text-ui-base text-foreground-subtle">{automation.prompt}</p>
                    <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-ui-sm text-foreground-subtle">
                      <span>{describeCron(automation.cronExpr, intl)}</span>
                      {nextRun ? (
                        <span>
                          {intl.formatMessage({ id: "automations.nextRun" }, { when: nextRun })}
                        </span>
                      ) : null}
                      <span>
                        {intl.formatMessage(
                          { id: automation.maxRuns ? "automations.runCountLimited" : "automations.runCount" },
                          automation.maxRuns
                            ? { count: String(automation.runCount), max: String(automation.maxRuns) }
                            : { count: String(automation.runCount) },
                        )}
                      </span>
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <AutomationSwitchToggle
                      checked={automation.enabled}
                      ariaLabel={intl.formatMessage({
                        id: automation.enabled ? "automations.pause" : "automations.resume",
                      })}
                      onChange={() => handleToggle(automation)}
                    />
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-sm"
                          aria-label={intl.formatMessage({ id: "automations.moreActions" })}
                          data-testid={TID_AUTOMATION_CARD_MENU}
                          disabled={busy}
                        >
                          <AutomationMoreHorizontalIcon className="size-4" aria-hidden="true" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem onSelect={() => openEdit(automation)}>
                          <AutomationEditActionIcon className="size-4" aria-hidden="true" />
                          {intl.formatMessage({ id: "automations.edit" })}
                        </DropdownMenuItem>
                        <DropdownMenuItem onSelect={() => void handleRunNow(automation)}>
                          <AutomationRunNowIcon className="size-4" aria-hidden="true" />
                          {intl.formatMessage({ id: "automations.runNow" })}
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          data-testid={TID_AUTOMATION_ACTION_HISTORY}
                          onSelect={() => handleOpenHistory(automation)}
                        >
                          {intl.formatMessage({ id: "automations.viewHistory" })}
                        </DropdownMenuItem>
                        {automation.lifecycleStatus === "failed" || automation.lifecycleStatus === "completed" ? (
                          <DropdownMenuItem
                            onSelect={() => void restartAutomation(automation.automationId, agentService)}
                          >
                            {intl.formatMessage({ id: "automations.restart" })}
                          </DropdownMenuItem>
                        ) : null}
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          variant="destructive"
                          data-testid={TID_AUTOMATION_ACTION_DELETE}
                          onSelect={() => void handleDelete(automation)}
                        >
                          <AutomationTrashIcon />
                          {intl.formatMessage({ id: "automations.delete" })}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </div>
                <div className="mt-3 flex items-center justify-end gap-2 border-t border-border pt-3">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    data-testid={TID_AUTOMATION_RUN_NOW}
                    data-icon="inline-start"
                    disabled={busy}
                    onClick={() => void handleRunNow(automation)}
                  >
                    <AutomationRunNowIcon className="size-3" aria-hidden="true" />
                    {intl.formatMessage({ id: "automations.runNow" })}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    data-testid={TID_AUTOMATION_ACTION_TOGGLE}
                    disabled={busy}
                    onClick={() => handleToggle(automation)}
                  >
                    {intl.formatMessage({ id: automation.enabled ? "automations.pause" : "automations.resume" })}
                  </Button>
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
