import { memo, useMemo, type ReactNode } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowRight01Icon,
  CheckListIcon,
  File01Icon,
  FileEditIcon,
  GlobalSearchIcon,
  RobotIcon,
  TerminalIcon,
  ToolsIcon,
} from "@hugeicons/core-free-icons";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import {
  conversationWorkState,
  type ConversationTool,
} from "@/modules/ai/lib/conversationPresentation";
import {
  boundedConversationToolText,
  conversationToolDiff,
  conversationToolFamily,
  conversationToolPath,
  conversationToolResultText,
  conversationToolSummary,
  conversationFilePresentation,
  type ConversationDiff,
} from "@/modules/ai/lib/conversationToolPresentation";
import { useConversationDisclosure } from "@/modules/ai/components/ConversationDisclosure";
import { useConversationFiles } from "@/modules/ai/components/ConversationFiles";
import { ConversationToolDiffPreview } from "@/modules/ai/components/ConversationToolDiffPreview";
import { ToolInput, ToolOutput } from "@/modules/ai/components/elements/tool";

const META = {
  read: { icon: GlobalSearchIcon, label: "Read", active: "Reading" },
  search: { icon: GlobalSearchIcon, label: "Search", active: "Searching" },
  edit: { icon: FileEditIcon, label: "Edited", active: "Editing file" },
  terminal: { icon: TerminalIcon, label: "Terminal", active: "Running" },
  agent: { icon: RobotIcon, label: "Agent", active: "Running" },
  todo: { icon: CheckListIcon, label: "Plan", active: "Updating plan" },
  other: { icon: ToolsIcon, label: "Tool", active: "Running" },
};

export const ConversationToolRow = memo(function ConversationToolRow({
  tool,
  entryKey,
}: {
  tool: ConversationTool;
  entryKey: string;
}) {
  const tr = useTranslation();
  const { workspaceRoot, openFile } = useConversationFiles();
  const family = conversationToolFamily(tool.name);
  const state = conversationWorkState(tool);
  const running = state === "running" || state === "pending";
  const failed = state === "failed";
  const [open, setOpen] = useConversationDisclosure(
    `tool:${entryKey}`,
    failed || (family === "edit" && state === "completed"),
  );
  const path = conversationToolPath(tool);
  const summary = conversationToolSummary(tool);
  const { name: fileName, parent } = conversationFilePresentation(
    path,
    workspaceRoot,
  );
  const preview = useMemo(
    () =>
      family === "edit" && state === "completed"
        ? conversationToolDiff({ name: tool.name, input: tool.input })
        : null,
    [family, state, tool.name, tool.input],
  );
  const canOpenFile =
    !!openFile &&
    state === "completed" &&
    !!path &&
    !["list_directory", "create_directory"].includes(tool.name);
  const meta = META[family];
  const canToggle = failed || !["read", "search"].includes(family);
  const leading = (
    <>
      <HugeiconsIcon
        icon={meta.icon}
        size={16}
        className="shrink-0 text-muted-foreground"
      />
      <span
        className={cn(
          "shrink-0 whitespace-nowrap font-medium text-muted-foreground",
          running && "animate-pulse motion-reduce:animate-none",
        )}
      >
        {family === "other"
          ? tool.name
          : tr(running ? meta.active : meta.label)}
      </span>
      {summary && !(open && family === "terminal") && (
        <span className="flex min-w-0 max-w-full items-center gap-2 text-muted-foreground">
          {(family === "read" || family === "edit") && path ? (
            <>
              {canOpenFile ? (
                <button
                  type="button"
                  className="pointer-events-auto relative z-10 inline-flex min-w-0 items-center gap-1.5 hover:underline focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  onClick={(event) => {
                    event.stopPropagation();
                    openFile?.(path);
                  }}
                  title={path}
                  aria-label={tr("Open file {value0}", { value0: fileName })}
                >
                  <HugeiconsIcon
                    icon={File01Icon}
                    size={16}
                    className="shrink-0"
                  />
                  <span className="truncate">{fileName}</span>
                </button>
              ) : (
                <span className="inline-flex min-w-0 items-center gap-1.5">
                  <HugeiconsIcon
                    icon={File01Icon}
                    size={16}
                    className="shrink-0"
                  />
                  <span className="truncate">{fileName}</span>
                </span>
              )}
              {parent && (
                <span className="min-w-0 truncate opacity-70">{parent}</span>
              )}
            </>
          ) : (
            <span className="min-w-0 truncate font-sans">{summary}</span>
          )}
        </span>
      )}
      {preview && (preview.added > 0 || preview.removed > 0) && (
        <span
          className="flex shrink-0 gap-1 text-ui-sm tabular-nums"
          title={tr("Submitted changes")}
        >
          {preview.added > 0 && (
            <span className="text-green-600 dark:text-green-400">
              +{preview.added}
            </span>
          )}
          {preview.removed > 0 && (
            <span className="text-destructive">-{preview.removed}</span>
          )}
        </span>
      )}
      {(failed || state === "cancelled") && (
        <span className="shrink-0 text-destructive">
          {tr(
            failed
              ? "failed"
              : tool.state === "output-denied"
                ? "Denied"
                : "cancelled",
          )}
        </span>
      )}
    </>
  );
  const summaryClass =
    "group/tool-summary inline-flex max-w-full items-center gap-2 self-start text-left text-ui-base leading-5 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";
  const summaryContent = (
    <>
      {leading}
      {canToggle && (
        <HugeiconsIcon
          icon={ArrowRight01Icon}
          size={16}
          className={cn(
            "shrink-0 text-muted-foreground transition-transform",
            open
              ? "rotate-90"
              : "opacity-0 group-hover/tool-summary:opacity-100 group-focus-visible/tool-summary:opacity-100 [@media(hover:none)]:opacity-100",
          )}
        />
      )}
    </>
  );
  return (
    <Collapsible
      open={open && canToggle}
      onOpenChange={setOpen}
      className="flex w-full min-w-0 flex-col not-prose"
      data-conversation-tool={family}
    >
      {canToggle ? (
        <CollapsibleTrigger asChild>
          {/* biome-ignore lint/a11y/useSemanticElements: 文件按钮独立交互，不能嵌套原生 button。 */}
          <div
            className={cn(summaryClass, "cursor-pointer")}
            title={summary}
            role="button"
            tabIndex={0}
            aria-label={tr("Tool details: {value0}", {
              value0: summary || tool.name,
            })}
            onKeyDown={(event) => {
              // 文件按钮独立响应键盘，避免 Enter 同时打开文件和折叠工具详情。
              if (event.target !== event.currentTarget) return;
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                event.currentTarget.click();
              }
            }}
          >
            {summaryContent}
          </div>
        </CollapsibleTrigger>
      ) : (
        <div className={summaryClass} title={summary}>
          {summaryContent}
        </div>
      )}
      {canToggle && (
        <CollapsibleContent className="rcode-collapsible-content">
          <div className="pt-2">
            {open && (
              <ToolDetails
                tool={tool}
                family={family}
                failed={failed}
                preview={preview}
              />
            )}
          </div>
        </CollapsibleContent>
      )}
    </Collapsible>
  );
});

function ToolDetails({
  tool,
  family,
  failed,
  preview,
}: {
  tool: ConversationTool;
  family: keyof typeof META;
  failed: boolean;
  preview: ConversationDiff | null;
}) {
  const tr = useTranslation();
  const result = boundedConversationToolText(conversationToolResultText(tool));
  let content: ReactNode;
  if (family === "terminal") {
    content = (
      <div className="mb-2 space-y-3 rounded-xl border border-border bg-card px-4 py-3">
        {typeof tool.input.command === "string" && (
          <div className="flex items-start gap-2 text-ui-base">
            <span className="shrink-0 text-muted-foreground">$</span>
            <pre className="max-h-15 min-w-0 flex-1 overflow-auto whitespace-pre-wrap break-words font-sans">
              {tool.input.command}
            </pre>
          </div>
        )}
        <pre
          className={cn(
            "max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-ui-base leading-5",
            failed ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {result.text || tr("No output")}
        </pre>
      </div>
    );
  } else if (family === "edit" && preview?.lines.length && !failed) {
    content = <ConversationToolDiffPreview preview={preview} />;
  } else if (failed || family === "agent") {
    content = (
      <pre
        className={cn(
          "max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-xl border border-border bg-card px-4 py-3 font-mono text-ui-base leading-5",
          failed && "text-destructive",
        )}
      >
        {result.text || tr("No output")}
      </pre>
    );
  } else {
    content = (
      <div className="space-y-2 rounded-xl border border-border bg-card px-4 py-3">
        <ToolInput toolName={tool.name} input={tool.input} />
        <ToolOutput toolName={tool.name} output={tool.result} />
      </div>
    );
  }
  return (
    <>
      {content}
      {result.truncated && (
        <p className="mt-1 text-ui-sm text-muted-foreground">
          {tr("Preview truncated")}
        </p>
      )}
    </>
  );
}
