import { useMemo, useState, type CSSProperties } from "react";
import {
  TID_CHAT_CONTEXT_USAGE_TRIGGER,
  type ZCodeContextUsageBreakdownItem,
  type ZCodeProvider,
} from "@zcode/shared";
import {
  Context,
  ContextContent,
  ContextContentBody,
  ContextTrigger,
} from "@/components/ai-elements/context.js";
import { Progress } from "@/components/ui/progress.js";
import { useOptionalTabStore } from "@/store/TabStoreProvider.js";
import { isSettingsTab } from "@/store/tabStore.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { formatCompactTokenNumber } from "@/lib/tokenNumberFormat.js";

type ContextUsageBreakdownSource = ZCodeContextUsageBreakdownItem["source"];
const BREAKDOWN_SOURCE_LABEL_ID: Record<ContextUsageBreakdownSource, string> = {
  messages: "chat.contextUsage.breakdown.messages",
  system_prompt: "chat.contextUsage.breakdown.systemPrompt",
  meta_user_context: "chat.contextUsage.breakdown.metaUserContext",
  skills: "chat.contextUsage.breakdown.skills",
  tool_prompt: "chat.contextUsage.breakdown.toolPrompt",
  system_tool_schemas: "chat.contextUsage.breakdown.systemTools",
  mcp_tool_schemas: "chat.contextUsage.breakdown.mcpTools",
};
const BREAKDOWN_SOURCE_ORDER: Record<ContextUsageBreakdownSource, number> = {
  messages: 0,
  system_prompt: 1,
  meta_user_context: 2,
  skills: 3,
  tool_prompt: 4,
  system_tool_schemas: 5,
  mcp_tool_schemas: 6,
};
const CONTEXT_PROGRESS_TONE_COLORS = [
  "var(--color-usage-chart-1)",
  "color-mix(in oklab, var(--color-usage-chart-1) 78%, var(--color-surface))",
  "color-mix(in oklab, var(--color-usage-chart-1) 58%, var(--color-surface))",
  "color-mix(in oklab, var(--color-usage-chart-1) 42%, var(--color-surface))",
  "color-mix(in oklab, var(--color-usage-chart-1) 28%, var(--color-surface))",
] as const;

function getBreakdownToneStyle(index: number): CSSProperties {
  return {
    backgroundColor:
      CONTEXT_PROGRESS_TONE_COLORS[Math.min(index, CONTEXT_PROGRESS_TONE_COLORS.length - 1)] ??
      CONTEXT_PROGRESS_TONE_COLORS[0],
  };
}

function buildBreakdownSegments(breakdown: readonly ZCodeContextUsageBreakdownItem[] | undefined) {
  const charsBySource = new Map<ContextUsageBreakdownSource, number>();
  for (const item of breakdown ?? []) {
    if (Number.isFinite(item.chars) && item.chars > 0) {
      charsBySource.set(item.source, (charsBySource.get(item.source) ?? 0) + item.chars);
    }
  }
  const total = [...charsBySource.values()].reduce((sum, value) => sum + value, 0);
  if (total <= 0) return [];
  return [...charsBySource.entries()]
    .map(([source, chars]) => ({ source, chars, percent: chars / total }))
    .sort(
      (left, right) =>
        right.chars - left.chars ||
        BREAKDOWN_SOURCE_ORDER[left.source] - BREAKDOWN_SOURCE_ORDER[right.source],
    );
}

export function getRenderableTaskUsage<T extends { used: number; size: number }>(
  taskUsage: T | null,
): T | null {
  if (!taskUsage || !Number.isFinite(taskUsage.used) || !Number.isFinite(taskUsage.size)) {
    return null;
  }
  return taskUsage.used > 0 && taskUsage.size > 0 ? taskUsage : null;
}

export function getContextCompressionCommand(_provider: ZCodeProvider): string {
  return "/compact";
}

export function ChatContextUsage({
  taskUsage,
  intl,
  locale,
}: {
  taskUsage: {
    used: number;
    size: number;
    cache?: { hitRate: number | null };
    breakdown?: ZCodeContextUsageBreakdownItem[];
  } | null;
  selectedProvider: ZCodeProvider;
  intl: ReturnType<typeof useZCodeIntl>["intl"];
  locale: string;
  onSendCompressionCommand?: (command: string) => void;
  compressionDisabled?: boolean;
  codingPlanUsageRemaining?: unknown;
  startPlanBalance?: unknown;
}) {
  const isWorkspaceVisible = useOptionalTabStore(
    (state) => !state.tabs.some((tab) => tab.id === state.activeTabId && isSettingsTab(tab)),
  );
  const [open, setOpen] = useState(false);
  const usage = getRenderableTaskUsage(taskUsage);
  const segments = useMemo(() => buildBreakdownSegments(usage?.breakdown), [usage?.breakdown]);
  const percent = usage ? Math.min(Math.max(usage.used / usage.size, 0), 1) : 0;
  const summary = usage
    ? `${formatCompactTokenNumber(locale, usage.used)}/${formatCompactTokenNumber(locale, usage.size, { maximumFractionDigits: 0 })} (${new Intl.NumberFormat(locale, { style: "percent", maximumFractionDigits: 1 }).format(percent)})`
    : null;

  if (!isWorkspaceVisible || !usage || !summary) return null;

  return (
    <Context
      usedTokens={usage.used}
      maxTokens={usage.size}
      open={open}
      onOpenChange={setOpen}
    >
      <ContextTrigger
        aria-label={intl.formatMessage({ id: "chat.contextUsage.title" })}
        className="text-foreground-subtle"
        data-chat-toolbar-popover-trigger="true"
        data-testid={TID_CHAT_CONTEXT_USAGE_TRIGGER}
      />
      <ContextContent className="!rounded-xl !shadow-md" side="top" sideOffset={2}>
        <ContextContentBody className="space-y-3">
          <div className="space-y-2">
            <div className="flex min-w-0 items-center gap-3">
              <span className="shrink-0 text-ui-base font-medium text-foreground">
                {intl.formatMessage({ id: "chat.contextUsage.title" })}
              </span>
              <span className="ml-auto shrink-0 text-right font-mono text-ui-sm text-foreground-subtle">
                {summary}
              </span>
            </div>
            <Progress
              className="h-2 bg-surface"
              indicatorClassName="min-w-2"
              value={percent * 100}
              segments={segments.map((segment, index) => ({
                id: segment.source,
                percent: segment.percent,
                style: getBreakdownToneStyle(index),
              }))}
            />
          </div>
          {segments.length > 0 ? (
            <div
              aria-label={intl.formatMessage({ id: "chat.contextUsage.breakdown" })}
              className="space-y-1.5"
            >
              {segments.map((segment, index) => (
                <div className="flex min-w-0 items-center gap-2 text-ui-sm" key={segment.source}>
                  <span
                    aria-hidden="true"
                    className="size-2 shrink-0 rounded-sm border border-border"
                    style={getBreakdownToneStyle(index)}
                  />
                  <span className="min-w-0 truncate text-foreground-subtle">
                    {intl.formatMessage({ id: BREAKDOWN_SOURCE_LABEL_ID[segment.source] })}
                  </span>
                  <span className="ml-auto min-w-10 shrink-0 text-right font-mono text-ui-sm tabular-nums text-foreground">
                    {new Intl.NumberFormat(locale, {
                      style: "percent",
                      maximumFractionDigits: 1,
                    }).format(segment.percent)}
                  </span>
                </div>
              ))}
            </div>
          ) : null}
        </ContextContentBody>
      </ContextContent>
    </Context>
  );
}
