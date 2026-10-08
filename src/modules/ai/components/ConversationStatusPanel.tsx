import { useState, type ReactNode } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowDown01Icon,
  ArrowRight01Icon,
  CheckmarkSquare02Icon,
  File01Icon,
  FolderGitTwoIcon,
  FolderTreeIcon,
  MinusSignIcon,
  MoreHorizontalIcon,
  SquareIcon,
  TerminalIcon,
  UserMultipleIcon,
} from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/localized/spinner";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import { useConversationFiles } from "@/modules/ai/components/ConversationFiles";
import type { ConversationPanelMode } from "@/modules/ai/lib/conversationLayout";
import type {
  ConversationStatus,
  ConversationWork,
  ConversationWorkState,
} from "@/modules/ai/lib/conversationPresentation";
import type { GitStatusSnapshot } from "@/modules/ai/lib/native";
import type { Todo } from "@/modules/ai/lib/todos";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { agentStepLabel } from "@/modules/ai/lib/agentStepLabel";

export type ConversationGoal = {
  objective: string;
  status: ConversationWorkState;
  completed?: number;
  total?: number;
};

type Props = {
  model: ConversationStatus;
  todos: readonly Todo[];
  gitStatus: GitStatusSnapshot | null;
  mode: ConversationPanelMode;
  onModeChange: (mode: ConversationPanelMode) => void;
  busy: boolean;
  onStop: () => void;
  onOpenFiles: () => void;
  onOpenGit: () => void;
  onOpenTerminal: () => void;
  planReviewCount?: number;
  onReviewPlan?: () => void;
  // 扩展数据由 Runtime 提供；没有数据时不渲染占位状态或虚构进度。
  goal?: ConversationGoal;
  workflows?: readonly ConversationWork[];
};

export function ConversationStatusPanel({
  model,
  todos,
  gitStatus,
  mode,
  onModeChange,
  busy,
  onStop,
  onOpenFiles,
  onOpenGit,
  onOpenTerminal,
  planReviewCount = 0,
  onReviewPlan,
  goal,
  workflows = [],
}: Props) {
  const tr = useTranslation();
  const status = useChatStore((s) => s.agentMeta.status);
  const step = useChatStore((s) => s.agentMeta.step);
  const approvals = useChatStore((s) => s.agentMeta.approvalsPending);
  const files = model.files;
  const changedCount =
    gitStatus?.changedFiles.length ??
    files.filter((f) => f.state === "applied").length;
  const completed = todos.filter((todo) => todo.status === "completed").length;
  const runningTerminals = model.terminals.filter(
    (work) => work.state === "running",
  ).length;
  const runningAgents = model.agents.filter(
    (work) => work.state === "running",
  ).length;
  const hasContent = !!(
    gitStatus ||
    planReviewCount ||
    files.length ||
    todos.length ||
    model.terminals.length ||
    model.agents.length ||
    workflows.length ||
    goal
  );
  const activity =
    approvals || status === "awaiting-approval"
      ? tr("Waiting for approval")
      : status === "error"
        ? tr("Request failed.")
        : step
          ? agentStepLabel(step, tr)
          : tr("Thinking…");

  if (!hasContent) {
    return (
      <div
        className="absolute right-4 top-4 z-20 flex items-center gap-0.5 text-muted-foreground"
        data-conversation-tools
      >
        <WorkspaceActions
          onOpenFiles={onOpenFiles}
          onOpenGit={onOpenGit}
          onOpenTerminal={onOpenTerminal}
        />
      </div>
    );
  }

  return (
    <div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex justify-end px-4 pt-4 @min-[1280px]/conversation:left-auto @min-[1280px]/conversation:right-4 @min-[1280px]/conversation:px-0">
      <aside
        aria-label={tr("Conversation status")}
        data-conversation-status
        data-display-mode={mode}
        className={cn(
          "pointer-events-auto relative flex max-w-[calc(100cqw-2rem)] flex-col overflow-hidden rounded-2xl border border-border bg-popover text-popover-foreground shadow-md transition-[border-radius,padding,background-color,box-shadow] duration-300 motion-reduce:transition-none",
          mode === "panel"
            ? "max-h-[min(64dvh,32rem)] w-80"
            : mode === "mini"
              ? "max-h-8.5 w-max"
              : "max-h-8.5 w-max @min-[1280px]/conversation:max-h-[min(64dvh,32rem)] @min-[1280px]/conversation:w-80",
        )}
      >
        <div
          className={cn(
            "absolute right-3 top-3 z-10 items-center gap-1",
            mode === "mini"
              ? "hidden"
              : mode === "auto"
                ? "hidden @min-[1280px]/conversation:flex"
                : "flex",
          )}
        >
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon-sm"
                className="size-6"
                aria-label={tr("Status panel display")}
                title={tr("Status panel display")}
              >
                <HugeiconsIcon icon={MoreHorizontalIcon} size={14} />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-44">
              <DropdownMenuRadioGroup
                value={mode}
                onValueChange={(value) =>
                  onModeChange(value as ConversationPanelMode)
                }
              >
                <DropdownMenuRadioItem value="auto">
                  {tr("Automatic")}
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="panel">
                  {tr("Expanded panel")}
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="mini">
                  {tr("Compact summary")}
                </DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
          <Button
            variant="ghost"
            size="icon-sm"
            className="size-6"
            aria-label={tr("Collapse status panel")}
            title={tr("Collapse status panel")}
            onClick={() => onModeChange("mini")}
          >
            <HugeiconsIcon icon={MinusSignIcon} size={14} />
          </Button>
        </div>

        <div
          className={cn(
            "min-h-0 flex-1 flex-col gap-2 overflow-y-auto overflow-x-hidden p-2",
            mode === "mini"
              ? "hidden"
              : mode === "auto"
                ? "hidden @min-[1280px]/conversation:flex"
                : "flex",
          )}
          data-status-panel-body
        >
          {(gitStatus || files.length > 0) && (
            <PanelSection
              title={tr("Changes")}
              icon={FolderGitTwoIcon}
              count={changedCount}
              first
            >
              {gitStatus && (
                <div className="mb-1 flex min-w-0 items-center gap-2 px-2 text-ui-caption text-muted-foreground">
                  <span className="truncate font-mono" title={gitStatus.branch}>
                    {gitStatus.branch}
                  </span>
                  {gitStatus.ahead > 0 && <span>↑{gitStatus.ahead}</span>}
                  {gitStatus.behind > 0 && <span>↓{gitStatus.behind}</span>}
                </div>
              )}
              <div className="max-h-52 overflow-y-auto">
                {gitStatus?.changedFiles.map((file) => (
                  <FileRow
                    key={file.path}
                    path={file.path}
                    status={file.statusLabel}
                    onOpen={onOpenGit}
                  />
                ))}
                {files
                  .filter(
                    (file) =>
                      !gitStatus?.changedFiles.some(
                        (changed) =>
                          file.path
                            .replace(/\\/g, "/")
                            .endsWith(`/${changed.path}`) ||
                          file.path === changed.path,
                      ),
                  )
                  .map((file) => (
                    <FileRow
                      key={file.path}
                      path={file.path}
                      status={tr(
                        file.state === "applied"
                          ? "Modified"
                          : file.state === "failed"
                            ? "Failed"
                            : "Pending",
                      )}
                      onOpen={onOpenGit}
                      canOpen={file.state === "applied"}
                    />
                  ))}
                {gitStatus && !changedCount && !files.length && (
                  <p className="px-2 py-1 text-ui-sm text-muted-foreground">
                    {tr("No changes")}
                  </p>
                )}
                {gitStatus?.truncated && (
                  <p className="px-2 py-1 text-ui-caption text-muted-foreground">
                    {tr("More changes in source control")}
                  </p>
                )}
              </div>
            </PanelSection>
          )}
          {goal && (
            <PanelSection
              title={tr("Goal")}
              icon={CheckmarkSquare02Icon}
              first={!gitStatus && !files.length}
            >
              <div className="flex items-start gap-2 px-2 py-1 text-ui-sm">
                <WorkIndicator state={goal.status} />
                <span className="min-w-0 flex-1 leading-relaxed">
                  {goal.objective}
                </span>
              </div>
              {goal.total !== undefined && (
                <p className="px-2 text-ui-caption text-muted-foreground">
                  {goal.completed ?? 0}/{goal.total}
                </p>
              )}
            </PanelSection>
          )}
          {todos.length > 0 && (
            <PanelSection
              title={tr("Plan")}
              icon={CheckmarkSquare02Icon}
              count={`${completed}/${todos.length}`}
              first={!gitStatus && !files.length && !goal}
            >
              <ol className="flex max-h-80 flex-col overflow-y-auto pr-1">
                {todos.map((todo) => (
                  <TodoStatusRow key={todo.id} todo={todo} />
                ))}
              </ol>
            </PanelSection>
          )}
          {planReviewCount > 0 && onReviewPlan && (
            <PanelSection
              title={tr("Plan review")}
              icon={File01Icon}
              count={planReviewCount}
              first={!gitStatus && !files.length && !goal && !todos.length}
            >
              <Button
                size="xs"
                variant="ghost"
                className="mx-2 mb-1"
                onClick={onReviewPlan}
              >
                {tr("Review proposed changes ({count})", {
                  count: planReviewCount,
                })}
              </Button>
            </PanelSection>
          )}
          {model.terminals.length > 0 && (
            <WorkSection
              title={tr("Terminals")}
              icon={TerminalIcon}
              works={model.terminals}
              onOpen={onOpenTerminal}
              first={!gitStatus && !files.length && !goal && !todos.length}
            />
          )}
          {workflows.length > 0 && (
            <WorkSection
              title={tr("Workflows")}
              icon={CheckmarkSquare02Icon}
              works={workflows}
            />
          )}
          {model.agents.length > 0 && (
            <WorkSection
              title={tr("Agents")}
              icon={UserMultipleIcon}
              works={model.agents}
              first={
                !gitStatus &&
                !files.length &&
                !goal &&
                !todos.length &&
                !model.terminals.length &&
                !workflows.length
              }
            />
          )}
          {(busy || approvals > 0 || status === "error") && (
            <div
              role="status"
              className="flex items-center gap-2 px-2 py-2 text-ui-sm text-muted-foreground"
            >
              <WorkIndicator
                state={
                  status === "error"
                    ? "failed"
                    : approvals
                      ? "pending"
                      : "running"
                }
              />
              <span className="min-w-0 flex-1 truncate" title={activity}>
                {activity}
              </span>
              {busy && (
                <Button size="xs" variant="ghost" onClick={onStop}>
                  {tr("Stop")}
                </Button>
              )}
            </div>
          )}
          <div className="flex items-center justify-end gap-0.5 border-t border-border/60 pt-1">
            <WorkspaceActions
              onOpenFiles={onOpenFiles}
              onOpenGit={onOpenGit}
              onOpenTerminal={onOpenTerminal}
            />
          </div>
        </div>

        <div
          className={cn(
            "items-center",
            mode === "panel"
              ? "hidden"
              : mode === "auto"
                ? "flex @min-[1280px]/conversation:hidden"
                : "flex",
          )}
        >
          <button
            type="button"
            className="flex h-8 min-w-0 max-w-72 items-center gap-2 overflow-hidden px-3 text-ui-sm transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
            onClick={() => onModeChange("panel")}
            aria-label={tr("Expand status panel")}
            title={tr("Expand status panel")}
          >
            {(busy || approvals > 0) && (
              <>
                <WorkIndicator state={approvals ? "pending" : "running"} />
                <span className="max-w-36 truncate">{activity}</span>
              </>
            )}
            {changedCount > 0 && (
              <span className="inline-flex shrink-0 items-center gap-1">
                <HugeiconsIcon icon={FolderGitTwoIcon} size={12} />
                {changedCount}
              </span>
            )}
            {todos.length > 0 && (
              <span className="inline-flex shrink-0 items-center gap-1">
                <HugeiconsIcon icon={CheckmarkSquare02Icon} size={12} />
                {completed}/{todos.length}
              </span>
            )}
            {planReviewCount > 0 && (
              <span className="inline-flex shrink-0 items-center gap-1">
                <HugeiconsIcon icon={File01Icon} size={12} />
                {planReviewCount}
              </span>
            )}
            {runningTerminals > 0 && (
              <span className="inline-flex shrink-0 items-center gap-1">
                <HugeiconsIcon icon={TerminalIcon} size={12} />
                {runningTerminals}
              </span>
            )}
            {model.agents.length > 0 && (
              <span className="inline-flex shrink-0 items-center gap-1">
                <HugeiconsIcon icon={UserMultipleIcon} size={12} />
                {runningAgents || model.agents.length}
              </span>
            )}
            {!busy &&
              !changedCount &&
              !todos.length &&
              !model.agents.length && <span>{tr("Conversation status")}</span>}
            <HugeiconsIcon
              icon={ArrowDown01Icon}
              size={12}
              className="shrink-0 text-muted-foreground"
            />
          </button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                size="icon-sm"
                variant="ghost"
                className="mr-1 size-6 shrink-0"
                aria-label={tr("Status panel display")}
              >
                <HugeiconsIcon icon={MoreHorizontalIcon} size={13} />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuRadioGroup
                value={mode}
                onValueChange={(value) =>
                  onModeChange(value as ConversationPanelMode)
                }
              >
                <DropdownMenuRadioItem value="auto">
                  {tr("Automatic")}
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="panel">
                  {tr("Expanded panel")}
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="mini">
                  {tr("Compact summary")}
                </DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>
    </div>
  );
}

function PanelSection({
  title,
  icon: _icon,
  count,
  first = false,
  children,
  action,
}: {
  title: string;
  icon: typeof TerminalIcon;
  count?: number | string;
  first?: boolean;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <Collapsible
      defaultOpen
      className={cn(
        "group/status-section",
        !first && "border-t border-border/60 pt-2",
      )}
    >
      <div
        className={cn(
          "mb-0.5 flex h-8 min-w-0 shrink-0 items-center gap-1.5 px-2",
          first ? "pr-16" : "pr-8",
        )}
      >
        <CollapsibleTrigger className="group/status-heading flex min-w-0 shrink-0 items-center gap-1 text-left text-ui-base text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring">
          <span className="truncate">{title}</span>
          <HugeiconsIcon
            icon={ArrowRight01Icon}
            size={14}
            className="shrink-0 text-muted-foreground opacity-0 transition-transform transition-opacity group-hover/status-heading:opacity-100 group-focus-visible/status-heading:opacity-100 group-data-[state=open]/status-section:rotate-90 [@media(hover:none)]:opacity-100"
          />
        </CollapsibleTrigger>
        {count !== undefined && (
          <span className="shrink-0 text-ui-sm tabular-nums text-muted-foreground">
            {count}
          </span>
        )}
        {action}
      </div>
      <CollapsibleContent className="rcode-collapsible-content">
        {children}
      </CollapsibleContent>
    </Collapsible>
  );
}

function FileRow({
  path,
  status,
  onOpen,
  canOpen = true,
}: {
  path: string;
  status: string;
  onOpen: () => void;
  canOpen?: boolean;
}) {
  const { openFile } = useConversationFiles();
  const segments = path.split(/[\\/]/);
  const name = segments[segments.length - 1] ?? path;
  return (
    <button
      type="button"
      disabled={!canOpen}
      onClick={() => (openFile ? openFile(path) : onOpen())}
      className="flex min-h-8 w-full min-w-0 items-center gap-2 rounded-lg px-2 py-1.5 text-left text-ui-base enabled:hover:bg-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      title={path}
    >
      <HugeiconsIcon
        icon={File01Icon}
        size={13}
        className="shrink-0 text-muted-foreground"
      />
      <span className="min-w-0 flex-1 truncate">{name}</span>
      <span className="max-w-24 truncate text-ui-caption text-muted-foreground">
        {status}
      </span>
    </button>
  );
}

function TodoStatusRow({ todo }: { todo: Todo }) {
  return (
    <li
      className="flex min-h-8 items-start gap-2 rounded-lg px-2 py-1.5 text-ui-base hover:bg-accent"
      title={todo.description}
    >
      <WorkIndicator
        state={
          todo.status === "completed"
            ? "completed"
            : todo.status === "in_progress"
              ? "running"
              : "pending"
        }
      />
      <span
        className={cn(
          "line-clamp-2 min-w-0 flex-1 break-words leading-5",
          todo.status === "completed" &&
            "text-muted-foreground/70 line-through",
        )}
      >
        {todo.title}
      </span>
    </li>
  );
}

function WorkSection({
  title,
  icon,
  works,
  first,
  onOpen,
}: {
  title: string;
  icon: typeof TerminalIcon;
  works: readonly ConversationWork[];
  first?: boolean;
  onOpen?: () => void;
}) {
  const tr = useTranslation();
  const active = works.filter(
    (work) => work.state === "running" || work.state === "pending",
  );
  const ended = works.filter(
    (work) => work.state !== "running" && work.state !== "pending",
  );
  const [endedLimit, setEndedLimit] = useState(12);
  return (
    <PanelSection
      title={title}
      icon={icon}
      count={active.length}
      first={first}
      action={
        onOpen && (
          <Button
            size="icon-sm"
            variant="ghost"
            className="size-6"
            onClick={onOpen}
            aria-label={tr("Open terminal")}
            title={tr("Open terminal")}
          >
            <HugeiconsIcon icon={TerminalIcon} size={12} />
          </Button>
        )
      }
    >
      <div className="max-h-48 overflow-y-auto pr-1">
        {active.map((work) => (
          <WorkRow key={work.id} work={work} />
        ))}
        {ended.length > 0 && (
          <Collapsible className="group/ended">
            <CollapsibleTrigger className="flex w-full items-center gap-1.5 rounded-lg px-2 py-1 text-ui-caption text-muted-foreground hover:bg-accent">
              <HugeiconsIcon
                icon={ArrowRight01Icon}
                size={11}
                className="transition-transform group-data-[state=open]/ended:rotate-90"
              />
              {tr("Completed work ({count})", { count: ended.length })}
            </CollapsibleTrigger>
            <CollapsibleContent className="rcode-collapsible-content">
              {ended
                .slice(-endedLimit)
                .reverse()
                .map((work) => (
                  <WorkRow key={work.id} work={work} />
                ))}
              {ended.length > endedLimit && (
                <Button
                  size="xs"
                  variant="ghost"
                  className="mx-2"
                  onClick={() => setEndedLimit((limit) => limit + 12)}
                >
                  {tr("Show more")}
                </Button>
              )}
            </CollapsibleContent>
          </Collapsible>
        )}
      </div>
    </PanelSection>
  );
}

function WorkRow({ work }: { work: ConversationWork }) {
  const tr = useTranslation();
  return (
    <Collapsible className="group/status-work">
      <CollapsibleTrigger
        disabled={!work.detail}
        className="flex min-h-8 w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left text-ui-base hover:bg-accent disabled:hover:bg-transparent"
        title={work.title}
      >
        <WorkIndicator state={work.state} />
        <span className="min-w-0 flex-1 truncate">{work.title}</span>
        {work.durationMs !== undefined && (
          <span className="shrink-0 text-ui-caption tabular-nums text-muted-foreground">
            {Math.round(work.durationMs / 1000)}s
          </span>
        )}
        <span className="sr-only">{tr(work.state)}</span>
        {work.detail && (
          <HugeiconsIcon
            icon={ArrowRight01Icon}
            size={11}
            className="mt-1 shrink-0 text-muted-foreground group-data-[state=open]/status-work:rotate-90"
          />
        )}
      </CollapsibleTrigger>
      {work.detail && (
        <CollapsibleContent className="rcode-collapsible-content">
          <pre className="mx-2 mb-1 max-h-40 overflow-auto whitespace-pre-wrap break-words border-l border-border pl-2 text-ui-caption leading-relaxed text-muted-foreground">
            {work.detail}
          </pre>
        </CollapsibleContent>
      )}
    </Collapsible>
  );
}

function WorkIndicator({ state }: { state: ConversationWorkState }) {
  const tr = useTranslation();
  return (
    <span
      role="img"
      className="mt-0.5 flex size-3.5 shrink-0 items-center justify-center"
      aria-label={tr(state)}
    >
      {state === "running" ? (
        <Spinner className="size-3" />
      ) : state === "completed" ? (
        <HugeiconsIcon
          icon={CheckmarkSquare02Icon}
          size={13}
          className="text-muted-foreground"
        />
      ) : state === "pending" ? (
        <HugeiconsIcon
          icon={SquareIcon}
          size={13}
          className="text-muted-foreground"
        />
      ) : (
        <span
          className={cn(
            "size-1.5 rounded-full",
            state === "failed" ? "bg-destructive" : "bg-muted-foreground/60",
          )}
        />
      )}
    </span>
  );
}

function WorkspaceActions({
  onOpenFiles,
  onOpenGit,
  onOpenTerminal,
}: {
  onOpenFiles: () => void;
  onOpenGit: () => void;
  onOpenTerminal: () => void;
}) {
  const tr = useTranslation();
  return (
    <>
      {[
        { label: "Files", icon: FolderTreeIcon, action: onOpenFiles },
        { label: "Review changes", icon: FolderGitTwoIcon, action: onOpenGit },
        { label: "Terminal", icon: TerminalIcon, action: onOpenTerminal },
      ].map(({ label, icon, action }) => (
        <Button
          key={label}
          variant="ghost"
          size="icon-sm"
          className="size-7"
          aria-label={tr(label)}
          title={tr(label)}
          onClick={action}
        >
          <HugeiconsIcon icon={icon} size={14} />
        </Button>
      ))}
    </>
  );
}
