import type {
  Dispatch,
  MutableRefObject,
  RefObject,
  SetStateAction,
} from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { AcpSessionView } from "@/lib/acp/store";
import type { ContextUsageDisplay } from "@/lib/contextUsage";
import { hasContextUsageData } from "@/lib/contextUsage";
import type {
  CustomProvider,
  TaskCacheUsage,
} from "@/lib/api";
import type { Attachment } from "@/lib/attachments";
import {
  findActiveModel,
  formatSessionModelReference,
  providerIdFromSessionReference,
  type ModelOption,
} from "@/lib/modelCatalog";
import type { SessionSnapshot } from "@/lib/session";
import type { SettingsSectionId } from "@/lib/settingsCatalog";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import {
  IconPlus,
  IconArrowUp,
  IconStop,
} from "@/components/icons";
import { ComposerGoalChip } from "@/components/ComposerGoalProgress";
import { ComposerPlanModeChip } from "@/components/ComposerPlanModeChip";
import { ComposerModelMenu } from "@/components/ComposerModelMenu";
import { ComposerReasoningMenu } from "@/components/ComposerReasoningMenu";
import { ContextUsageChip } from "@/components/ContextUsageChip";
import { isTauri, providersSelectModel } from "@/lib/api";
import {
  createOperationId,
  sessionSetEffort,
  sessionSetModel,
} from "@/lib/acp/api";
import { isDraftEmpty, parseStoredContent } from "@/lib/draftDoc";
import { localizeUiError } from "@/lib/session";
import { shouldEnqueueSend } from "@/lib/sendQueue";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;
type ComposerPanel = "model" | "reasoning" | null;

export interface ComposerToolbarProps {
  tr: Translator;
  locale: Locale;
  session: SessionSnapshot;
  composerPlusTriggerRef: RefObject<HTMLButtonElement | null>;
  composerMenuOpen: boolean;
  setShowComposerPlus: SetState<boolean>;
  closeComposerMenu: () => void;
  goalModeSessionKey: string | null;
  setGoalModeSessionKey: SetState<string | null>;
  planModeSessionKey: string | null;
  setPlanModeSessionKey: SetState<string | null>;
  ultraModeSessionKey: string | null;
  setUltraModeSessionKey: SetState<string | null>;
  acpSessionView: AcpSessionView | null;
  confirmClearCurrentGoal: () => void;
  modelId: string;
  /** 当前会话实际绑定的供应商；草稿尚未选定或引用缺失时为空。 */
  sessionProviderId: string | null;
  setSessionModelReference: SetState<string>;
  availableModels: ModelOption[];
  activeCustomProvider: CustomProvider | null;
  refreshProviderRoute: () => Promise<void>;
  showToast: (message: string, duration?: number) => void;
  composerPanel: ComposerPanel;
  setComposerPanel: SetState<ComposerPanel>;
  effort: string;
  setEffort: SetState<string>;
  isValidEffort: (id: string, model?: ModelOption | null) => boolean;
  isValidModelId: (id: string, catalog: ModelOption[]) => boolean;
  modelBySessionRef: MutableRefObject<Map<string, string>>;
  viewingSessionIdRef: MutableRefObject<string | null>;
  /** 清除当前 Session 的旧上下文用量，等待新模型上报。 */
  invalidateContextUsage: (sessionId: string) => void;
  navigateSettings: (
    section?: SettingsSectionId,
    providerId?: string | null,
  ) => void;
  contextUsageDisplay: ContextUsageDisplay;
  taskCacheUsage: TaskCacheUsage | null;
  draft: string;
  attachments: Attachment[];
  connecting: boolean;
  effectiveCanSend: boolean;
  effectiveCanStop: boolean;
  hasConfiguredModel: boolean;
  send: () => Promise<void>;
  stop: () => Promise<void>;
}

export function ComposerToolbar({
  tr,
  locale,
  session,
  composerPlusTriggerRef,
  composerMenuOpen,
  setShowComposerPlus,
  closeComposerMenu,
  goalModeSessionKey,
  setGoalModeSessionKey,
  planModeSessionKey,
  setPlanModeSessionKey,
  ultraModeSessionKey,
  setUltraModeSessionKey,
  acpSessionView,
  confirmClearCurrentGoal,
  modelId,
  sessionProviderId,
  setSessionModelReference,
  availableModels,
  activeCustomProvider,
  refreshProviderRoute,
  showToast,
  composerPanel,
  setComposerPanel,
  effort,
  setEffort,
  isValidEffort,
  isValidModelId,
  modelBySessionRef,
  viewingSessionIdRef,
  invalidateContextUsage,
  navigateSettings,
  contextUsageDisplay,
  taskCacheUsage,
  draft,
  attachments,
  connecting,
  effectiveCanSend,
  effectiveCanStop,
  hasConfiguredModel,
  send,
  stop,
}: ComposerToolbarProps) {
  const sessionKey = session.sessionId ?? "__draft__";
  const currentGoalActive =
    acpSessionView?.session_id === session.sessionId &&
    acpSessionView.goal.goal;
  const goalActive = Boolean(
    currentGoalActive || goalModeSessionKey === sessionKey,
  );
  // 会话模型按会话自身供应商解析：同一模型 ID 可能存在于多个供应商，
  // 只按全局活跃供应商查找会让思考强度控件读错模型能力。
  const activeModel = findActiveModel(modelId, sessionProviderId, availableModels);
  const currentTaskCacheUsage =
    taskCacheUsage?.sessionId === session.sessionId ? taskCacheUsage : null;
  const hasBody =
    !isDraftEmpty(parseStoredContent(draft)) || attachments.length > 0;

  return (
    <div className="composer__row">
      <Tip label={tr("composer.add")}>
        <Button
          ref={composerPlusTriggerRef}
          type="button"
          className={
            "icon-btn icon-btn--plus" +
            (composerMenuOpen ? " is-open" : "")
          }
          aria-label={tr("composer.add")}
          onClick={() => {
            if (composerMenuOpen) closeComposerMenu();
            else setShowComposerPlus(true);
          }}
        >
          <IconPlus size={18} />
        </Button>
      </Tip>

      {goalActive ? (
        <ComposerGoalChip
          locale={locale}
          onClear={
            currentGoalActive
              ? () => {
                  setGoalModeSessionKey(null);
                  confirmClearCurrentGoal();
                }
              : () => setGoalModeSessionKey(null)
          }
        />
      ) : null}

      {planModeSessionKey === sessionKey ? (
        <ComposerPlanModeChip
          locale={locale}
          active
          onToggle={() => setPlanModeSessionKey(null)}
        />
      ) : null}

      <ComposerModelMenu
        open={composerPanel === "model"}
        onOpenChange={(open) =>
          setComposerPanel((current) =>
            open ? "model" : current === "model" ? null : current,
          )
        }
        providerId={sessionProviderId}
        modelId={modelId}
        models={availableModels}
        labels={{
          model: tr("composer.model"),
          vision: tr("composer.modelVision"),
          addModel: tr("composer.addModel"),
          manageModels: tr("composer.manageModels"),
        }}
        onModel={(nextModelId, providerId) => {
          if (!isValidModelId(nextModelId, availableModels)) return;
          if (!providerId) return;
          // 先落地本地选择：同一模型 ID 可能属于多个供应商，必须连同供应商一起记住，
          // 否则切换后菜单仍会显示上一个供应商。
          setSessionModelReference(
            formatSessionModelReference(providerId, nextModelId),
          );
          if (!isTauri()) return;
          const activeSessionId = viewingSessionIdRef.current;
          if (activeSessionId) {
            invalidateContextUsage(activeSessionId);
            modelBySessionRef.current.set(activeSessionId, `${providerId}::${nextModelId}`);
            void sessionSetModel({
              sessionId: activeSessionId,
              providerId,
              modelId: nextModelId,
              operationId: createOperationId("session-model"),
            }).catch((error: unknown) => {
              modelBySessionRef.current.delete(activeSessionId);
              showToast(localizeUiError(error, locale), 4000);
            });
          } else {
            void providersSelectModel(providerId, nextModelId)
              .then(() => refreshProviderRoute())
              .catch((error: unknown) =>
                showToast(localizeUiError(error, locale), 4000),
              );
          }
        }}
        onAddModel={() => {
          // 会话内切换的模型可能不属于全局活跃供应商；管理模型入口优先带出该对话实际使用的供应商。
          const reference = session.sessionId
            ? modelBySessionRef.current.get(session.sessionId)
            : undefined;
          const referenceProviderId = reference
            ? providerIdFromSessionReference(reference)
            : null;
          navigateSettings(
            "account",
            referenceProviderId ?? sessionProviderId ?? activeCustomProvider?.id ?? null,
          );
        }}
      />

      <ComposerReasoningMenu
        open={composerPanel === "reasoning"}
        onOpenChange={(open) =>
          setComposerPanel((current) =>
            open
              ? "reasoning"
              : current === "reasoning"
                ? null
                : current,
          )
        }
        model={activeModel}
        effort={effort}
        ultra={ultraModeSessionKey === sessionKey}
        labels={{
          reasoning: tr("composer.effort"),
          reasoningUnsupported: tr("composer.reasoningUnsupported"),
          ultra: tr("composer.ultra"),
          effortNone: tr("effort.none"),
          effortMinimal: tr("effort.minimal"),
          effortHigh: tr("effort.high"),
          effortMedium: tr("effort.medium"),
          effortLow: tr("effort.low"),
          effortXHigh: tr("effort.xhigh"),
          effortMax: tr("effort.max"),
        }}
        onEffort={(nextEffort) => {
          if (!isValidEffort(nextEffort, activeModel)) return;
          setEffort(nextEffort);
          const activeSessionId = viewingSessionIdRef.current;
          if (isTauri() && activeSessionId) {
            void sessionSetEffort({
              sessionId: activeSessionId,
              effort: nextEffort,
              operationId: createOperationId("session-effort"),
            }).catch((error: unknown) =>
              showToast(localizeUiError(error, locale), 4000),
            );
          }
        }}
        onUltra={(enabled) =>
          setUltraModeSessionKey(enabled ? sessionKey : null)
        }
      />

      {hasContextUsageData(contextUsageDisplay, currentTaskCacheUsage) ? (
        <ContextUsageChip
          display={contextUsageDisplay}
          taskCacheUsage={currentTaskCacheUsage}
          labels={{
            aria: tr("context.chipAria"),
            contextUsageRate: tr("context.usageRate"),
            taskCacheHitRate: tr("context.taskCacheHitRate"),
          }}
        />
      ) : null}

      <span className="composer__spacer" />
      {effectiveCanStop ? (
        hasConfiguredModel &&
        hasBody &&
        shouldEnqueueSend(session.state, connecting) ? (
          <Tip label={tr("composer.send")}>
            <Button
              type="button"
              className="icon-btn icon-btn--primary"
              onClick={() => void send()}
              aria-label={tr("composer.send")}
            >
              <IconArrowUp size={16} />
            </Button>
          </Tip>
        ) : (
          <Tip label={tr("composer.stop")}>
            <Button
              type="button"
              className="icon-btn icon-btn--danger"
              onClick={() => void stop()}
              aria-label={tr("composer.stop")}
            >
              <IconStop size={14} />
            </Button>
          </Tip>
        )
      ) : (
        <Tip label={tr("composer.send")}>
          <Button
            type="button"
            className="icon-btn icon-btn--primary"
            disabled={
              !hasConfiguredModel ||
              (!effectiveCanSend &&
                !shouldEnqueueSend(session.state, connecting)) ||
              !hasBody
            }
            onClick={() => void send()}
            aria-label={tr("composer.send")}
          >
            <IconArrowUp size={16} />
          </Button>
        </Tip>
      )}
    </div>
  );
}
