import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { respondToToolApproval } from "@/modules/ai/lib/nativeApproval";
import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from "@/modules/ai/components/elements/conversation";
import {
  Message,
  MessageAction,
  MessageActions,
  MessageContent,
  MessageResponse,
  type MessageResponseProps,
} from "@/modules/ai/components/elements/message";
import { MarkdownCode } from "@/modules/ai/components/elements/markdown-code";
import {
  MarkdownLink,
  type MarkdownLinkProps,
} from "@/modules/markdown/MarkdownLink";
import { ConversationToolRow } from "@/modules/ai/components/ConversationToolRow";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { cn } from "@/lib/utils";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowRight01Icon,
  CodeIcon,
  Copy01Icon,
  File01Icon,
  HashtagIcon,
  RefreshIcon,
  TerminalIcon,
  Tick01Icon,
} from "@hugeicons/core-free-icons";
import { SLASH_COMMANDS, RCODE_CMD_RE } from "@/modules/ai/lib/slashCommands";
import { Spinner } from "@/components/localized/spinner";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { sendMessage } from "@/modules/ai/store/chatRuntime";
import {
  CONVERSATION_COLUMN,
  CONVERSATION_PANEL_OFFSET,
} from "@/modules/ai/lib/conversationLayout";
import {
  attachmentPreviewUrl,
  conversationTool,
  conversationWorkState,
  groupConversationTurns,
  presentConversationTurn,
  type ConversationEntry,
  type ConversationTurn,
} from "@/modules/ai/lib/conversationPresentation";
import type {
  ChatStatus,
  DynamicToolUIPart,
  ToolUIPart,
  UIMessage,
  UIMessagePart,
} from "ai";
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { toast } from "sonner";
import { AiToolApproval } from "@/modules/ai/components/AiToolApproval";
import {
  ConversationDisclosure,
  useConversationDisclosure,
  useConversationHasExpanded,
} from "@/modules/ai/components/ConversationDisclosure";
import { ConversationTimeline } from "@/modules/ai/components/ConversationTimeline";

function CommandSnippet({ name }: { name: string }) {
  const tr = useTranslation();
  const meta = Object.keys(SLASH_COMMANDS).includes(name) ? SLASH_COMMANDS[name] : null;
  if (!meta) {
    return (
      <div className="inline-flex items-center gap-1.5 rounded-md border border-border/50 bg-muted/40 px-2 py-1 font-mono text-ui-base">
        /{name}
      </div>
    );
  }
  return (
    <div className="inline-flex max-w-full items-center gap-2 rounded-md border border-border/50 bg-muted/40 px-2 py-1">
      <HugeiconsIcon
        icon={meta.icon}
        size={12}
        strokeWidth={1.75}
        className="shrink-0 text-foreground"
      />
      <span className="font-mono text-ui-base text-foreground">
        {meta.invocation}
      </span>
      <span className="truncate text-ui-sm text-muted-foreground">
        {tr(meta.label)}
      </span>
    </div>
  );
}

type AnyToolPart = ToolUIPart | DynamicToolUIPart;

type ContextChip = { id: string } & (
  | { kind: "selection"; source: "terminal" | "editor"; lines: number }
  | { kind: "file"; name: string; lines: number }
  | { kind: "snippet"; name: string }
);

const SELECTION_RE =
  /<selection\s+source="(terminal|editor)">\n?([\s\S]*?)\n?<\/selection>/g;
const FILE_RE = /<file\s+name="([^"]+)"[^>]*>\n?([\s\S]*?)\n?<\/file>/g;
const SNIPPET_RE = /<snippet\s+name="([^"]+)">\n?[\s\S]*?\n?<\/snippet>/g;

function countLines(s: string): number {
  if (!s) return 0;
  const trimmed = s.replace(/\n+$/, "");
  if (!trimmed) return 0;
  return trimmed.split("\n").length;
}

function stripUserContextBlocks(text: string): {
  text: string;
  chips: ContextChip[];
} {
  const chips: ContextChip[] = [];
  let out = text;
  out = out.replace(
    SELECTION_RE,
    (_m, source: string, body: string, offset: number) => {
      chips.push({
        id: `selection:${offset}`,
        kind: "selection",
        source: source === "editor" ? "editor" : "terminal",
        lines: countLines(body),
      });
      return "";
    },
  );
  out = out.replace(
    FILE_RE,
    (_m, name: string, body: string, offset: number) => {
      chips.push({
        id: `file:${offset}`,
        kind: "file",
        name,
        lines: countLines(body),
      });
      return "";
    },
  );
  out = out.replace(SNIPPET_RE, (_m, name: string, offset: number) => {
    chips.push({ id: `snippet:${offset}`, kind: "snippet", name });
    return "";
  });
  return { text: out.trim(), chips };
}

const ContextChips = memo(function ContextChips({
  chips,
}: {
  chips: ContextChip[];
}) {
  const tr = useTranslation();
  return (
    <div className="mb-1 flex flex-wrap gap-1">
      {chips.map((c) => (
        <span
          key={c.id}
          className="inline-flex items-center gap-1 rounded-md border border-border/50 bg-card/60 px-1.5 py-0.5 text-ui-caption text-muted-foreground"
        >
          {chipIcon(c)}
          <span className="font-medium text-foreground">
            {tr(chipLabel(c))}
          </span>
          {"lines" in c && c.lines > 0 ? (
            <span className="opacity-70">
              · {c.lines}
              {tr("L")}
            </span>
          ) : null}
        </span>
      ))}
    </div>
  );
});

function chipIcon(c: ContextChip) {
  if (c.kind === "selection") {
    return (
      <HugeiconsIcon
        icon={c.source === "editor" ? CodeIcon : TerminalIcon}
        size={10}
        strokeWidth={1.75}
      />
    );
  }
  if (c.kind === "file") {
    return <HugeiconsIcon icon={File01Icon} size={10} strokeWidth={1.75} />;
  }
  return <HugeiconsIcon icon={HashtagIcon} size={10} strokeWidth={1.75} />;
}

function chipLabel(c: ContextChip): string {
  if (c.kind === "selection") {
    return c.source === "editor" ? "Editor selection" : "Terminal selection";
  }
  if (c.kind === "file") return c.name;
  return `#${c.name}`;
}
type AnyPart = UIMessagePart<Record<string, never>, Record<string, never>>;

type ApprovalArg = {
  id: string;
  approved: boolean;
  reason?: string;
};

type Props = {
  messages: UIMessage[];
  status: ChatStatus;
  error: Error | undefined;
  clearError: () => void;
  addToolApprovalResponse: (arg: ApprovalArg) => void | PromiseLike<void>;
  stop: () => void | PromiseLike<void>;
  bottomDock?: ReactNode;
  emptyState?: ReactNode;
  panelOffset?: boolean;
  onRegenerate?: () => void | PromiseLike<void>;
};

export function AiChatView({
  messages,
  status,
  error,
  clearError,
  addToolApprovalResponse,
  bottomDock,
  emptyState,
  panelOffset = false,
  onRegenerate,
}: Props) {
  const tr = useTranslation();
  const isBusy = status === "submitted" || status === "streaming";
  const lastMessage = messages[messages.length - 1];
  const showSpinner = isBusy && lastMessage?.role === "user";
  const streamingMessageId =
    status === "streaming" && lastMessage?.role === "assistant"
      ? lastMessage.id
      : null;
  const step = useChatStore((s) => s.agentMeta.step);
  const hitStepCap = useChatStore((s) => s.agentMeta.hitStepCap);
  const compactionNotice = useChatStore((s) => s.agentMeta.compactionNotice);
  const patchAgentMeta = useChatStore((s) => s.patchAgentMeta);
  const showContinue =
    !isBusy && hitStepCap && lastMessage?.role === "assistant";
  const groupTurns = useMemo(() => {
    let previous: ConversationTurn[] = [];
    return (value: UIMessage[]) => {
      previous = groupConversationTurns(value, previous);
      return previous;
    };
  }, []);
  const turns = useMemo(() => groupTurns(messages), [groupTurns, messages]);
  const lastTurnId = turns[turns.length - 1]?.id;
  const workbenchLayout = bottomDock !== undefined;

  const onApproval = useCallback(
    (id: string, approved: boolean) =>
      respondToToolApproval(id, approved, addToolApprovalResponse),
    [addToolApprovalResponse],
  );

  const renderTurn = useCallback(
    (turn: ConversationTurn) => (
      <RenderedTurn
        turn={turn}
        onApproval={onApproval}
        streamingMessageId={turn.id === lastTurnId ? streamingMessageId : null}
        busy={turn.id === lastTurnId && isBusy}
        panelOffset={panelOffset}
        workbenchLayout={workbenchLayout}
        onRegenerate={
          turn.id === lastTurnId && !isBusy ? onRegenerate : undefined
        }
      />
    ),
    [
      onApproval,
      lastTurnId,
      streamingMessageId,
      isBusy,
      panelOffset,
      workbenchLayout,
      onRegenerate,
    ],
  );

  return (
    <ConversationDisclosure>
      <Conversation className="min-h-0" initial="instant" resize="instant">
        <ConversationContent
          className="min-h-full gap-0 p-0"
          scrollClassName="min-h-0 overscroll-contain [overflow-anchor:none]"
        >
          {messages.length ? (
            <ConversationTimeline turns={turns} renderTurn={renderTurn} />
          ) : (
            <div className="flex min-h-0 flex-1 items-center justify-center p-6">
              {emptyState ?? (
                <ConversationEmptyState
                  title={tr("Ask RCode anything")}
                  description={tr(
                    "Explain command output, fix errors, generate snippets, or run a task.",
                  )}
                />
              )}
            </div>
          )}
          <div
            className={cn(
              "mx-auto flex flex-col gap-3 px-4 pb-5 @md/conversation:px-6",
              CONVERSATION_COLUMN,
              panelOffset && CONVERSATION_PANEL_OFFSET,
            )}
          >
            {compactionNotice && (
              <CompactionNotice
                droppedCount={compactionNotice.droppedCount}
                onDismiss={() => patchAgentMeta({ compactionNotice: null })}
              />
            )}
            {showSpinner && (
              <div className="flex items-center gap-2 text-ui-sm text-muted-foreground">
                <Spinner />
                <span className="truncate">{step ?? tr("Thinking…")}</span>
              </div>
            )}
            {showContinue && (
              <ContinueRow
                onContinue={() => {
                  patchAgentMeta({ hitStepCap: false });
                  void sendMessage(
                    "Continue from where you stopped. Don't recap; just keep going.",
                  );
                }}
              />
            )}
            {error && (
              <div className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-ui-base text-destructive">
                <div className="font-medium">{tr("Request failed.")}</div>
                <div className="mt-0.5 leading-relaxed opacity-90">
                  {error.message}
                </div>
                <div className="mt-2 flex items-center gap-2">
                  {onRegenerate && (
                    <Button
                      variant="secondary"
                      size="xs"
                      onClick={() => void onRegenerate()}
                    >
                      {tr("Retry")}
                    </Button>
                  )}
                  <button
                    type="button"
                    onClick={clearError}
                    className="underline opacity-80 hover:opacity-100"
                  >
                    {tr("Dismiss")}
                  </button>
                </div>
              </div>
            )}
          </div>
          {bottomDock !== undefined && (
            <div
              className="pointer-events-none sticky bottom-0 z-10 mt-auto flex w-full justify-center"
              data-conversation-composer-dock
            >
              <div
                className={cn(
                  "pointer-events-auto relative px-4 pb-4",
                  CONVERSATION_COLUMN,
                  panelOffset && CONVERSATION_PANEL_OFFSET,
                )}
              >
                <ConversationScrollButton
                  className="bottom-auto -top-10"
                  aria-label={tr("Scroll to latest message")}
                />
                {bottomDock}
              </div>
            </div>
          )}
        </ConversationContent>
        {bottomDock === undefined && (
          <ConversationScrollButton
            aria-label={tr("Scroll to latest message")}
          />
        )}
      </Conversation>
    </ConversationDisclosure>
  );
}

const CompactionNotice = memo(function CompactionNotice({
  droppedCount,
  onDismiss,
}: {
  droppedCount: number;
  onDismiss: () => void;
}) {
  const tr = useTranslation();
  return (
    <div className="flex items-center gap-2 rounded-md border border-border/40 bg-muted/30 px-2.5 py-1.5 text-ui-sm text-muted-foreground">
      <span className="size-1.5 shrink-0 rounded-full bg-amber-500/80" />
      <span className="flex-1 truncate">
        {tr(
          droppedCount === 1
            ? "Context compacted: {count} older tool result elided to save tokens."
            : "Context compacted: {count} older tool results elided to save tokens.",
          { count: droppedCount },
        )}
      </span>
      <button
        type="button"
        onClick={onDismiss}
        className="text-ui-base underline opacity-70 hover:opacity-100"
      >
        {tr("Dismiss")}
      </button>
    </div>
  );
});

const ContinueRow = memo(function ContinueRow({
  onContinue,
}: {
  onContinue: () => void;
}) {
  const tr = useTranslation();
  return (
    <div className="flex items-center gap-2 rounded-md border border-border/50 bg-card/60 px-2.5 py-1.5 text-ui-sm">
      <span className="flex-1 text-muted-foreground">
        {tr("Hit the step limit. Continue to keep going.")}
      </span>
      <button
        type="button"
        onClick={onContinue}
        className="rounded-md border border-border/60 bg-background px-2 py-0.5 text-ui-base font-medium text-foreground transition-colors hover:bg-accent"
      >
        {tr("Continue")}
      </button>
    </div>
  );
});

const RenderedTurn = memo(function RenderedTurn({
  turn,
  onApproval,
  streamingMessageId,
  busy,
  panelOffset,
  workbenchLayout,
  onRegenerate,
}: {
  turn: ConversationTurn;
  onApproval: (id: string, approved: boolean) => void;
  streamingMessageId: string | null;
  busy: boolean;
  panelOffset: boolean;
  workbenchLayout: boolean;
  onRegenerate?: () => void | PromiseLike<void>;
}) {
  const groups = useMemo(() => presentConversationTurn(turn), [turn]);
  const responses = groups.filter((group) => group.kind === "response");
  const responseText = !busy
    ? responses
        .flatMap((group) => group.entries)
        .filter((entry) => entry.part.type === "text")
        .map((entry) => (entry.part as { text: string }).text)
        .join("\n\n")
    : "";
  let lastAssistant: UIMessage | undefined;
  for (let index = turn.messages.length - 1; index >= 0; index -= 1) {
    if (turn.messages[index].role === "assistant") {
      lastAssistant = turn.messages[index];
      break;
    }
  }
  return (
    <section
      data-conversation-turn={turn.id}
      className={cn(
        "relative mx-auto flex flex-col gap-5 px-4 pb-5 @md/conversation:px-6",
        workbenchLayout ? "pt-14" : "pt-5",
        CONVERSATION_COLUMN,
        panelOffset && CONVERSATION_PANEL_OFFSET,
      )}
    >
      {turn.messages
        .filter((message) => message.role === "user")
        .map((message) => (
          <UserMessage key={message.id} message={message} />
        ))}
      {groups.length > 0 && (
        <div
          className="group/assistant-turn flex min-w-0 flex-col gap-5"
          data-assistant-turn
        >
          {groups.map((group) =>
            group.kind === "work" ? (
              <WorkBlock
                key={group.key}
                id={group.key}
                entries={group.entries}
                busy={busy}
                onApproval={onApproval}
                streamingMessageId={streamingMessageId}
              />
            ) : (
              <Message from="assistant" key={group.key}>
                <MessageContent className="overflow-visible">
                  <AssistantEntries
                    entries={group.entries}
                    onApproval={onApproval}
                    streamingMessageId={streamingMessageId}
                  />
                </MessageContent>
              </Message>
            ),
          )}
          {responseText && (
            <ResponseActions
              text={responseText}
              metadata={lastAssistant?.metadata}
              onRegenerate={onRegenerate}
            />
          )}
        </div>
      )}
    </section>
  );
});

const UserMessage = memo(function UserMessage({
  message,
}: {
  message: UIMessage;
}) {
  const rawText = message.parts
    .filter(
      (part): part is { type: "text"; text: string } => part.type === "text",
    )
    .map((part) => part.text)
    .join("\n");
  const cmdMatch = rawText.match(RCODE_CMD_RE);
  const commandName = cmdMatch?.[1] ?? null;
  const stripped = stripUserContextBlocks(
    cmdMatch ? rawText.slice(cmdMatch[0].length) : rawText,
  );
  return (
    <Message
      from="user"
      className="max-w-full @min-[624px]/conversation:max-w-xl"
    >
      <MessageFiles parts={message.parts} />
      <MessageContent className="group-[.is-user]:rounded-xl group-[.is-user]:rounded-tr-xs group-[.is-user]:rounded-br-xl group-[.is-user]:border group-[.is-user]:border-border group-[.is-user]:bg-card group-[.is-user]:px-4 group-[.is-user]:py-3">
        {commandName && <CommandSnippet name={commandName} />}
        {stripped.chips.length > 0 && <ContextChips chips={stripped.chips} />}
        {stripped.text && (
          <p className="whitespace-pre-wrap wrap-break-word">{stripped.text}</p>
        )}
      </MessageContent>
    </Message>
  );
});

function MessageFiles({ parts }: { parts: UIMessage["parts"] }) {
  const files = [
    ...new Map(
      parts
        .filter((part) => part.type === "file")
        .map((part) => [`${part.url}:${part.filename ?? ""}`, part]),
    ).values(),
  ];
  if (!files.length) return null;
  return (
    <div className="flex max-w-full flex-wrap justify-end gap-2">
      {files.map((file) => {
        const url = attachmentPreviewUrl(file.url);
        return file.mediaType.startsWith("image/") && url ? (
          <a
            key={`${file.url}:${file.filename ?? ""}`}
            href={url}
            target="_blank"
            rel="noreferrer"
            className="overflow-hidden rounded-xl border border-border"
          >
            <img
              src={url}
              alt={file.filename ?? ""}
              className="max-h-48 max-w-full object-contain"
              loading="lazy"
            />
          </a>
        ) : (
          <span
            key={`${file.url}:${file.filename ?? ""}`}
            className="inline-flex max-w-full items-center gap-1.5 rounded-lg border border-border bg-card px-3 py-2 text-ui-sm"
          >
            <HugeiconsIcon icon={File01Icon} size={14} />
            <span className="truncate">{file.filename ?? file.mediaType}</span>
          </span>
        );
      })}
    </div>
  );
}

const WorkBlock = memo(function WorkBlock({
  id,
  entries,
  busy,
  onApproval,
  streamingMessageId,
}: {
  id: string;
  entries: ConversationEntry[];
  busy: boolean;
  onApproval: (id: string, approved: boolean) => void;
  streamingMessageId: string | null;
}) {
  const tr = useTranslation();
  const hasExpandedReasoning = useConversationHasExpanded(
    entries.map((entry) => `reasoning:${entry.key}`),
  );
  const [open, setOpen] = useConversationDisclosure(
    `work:${id}`,
    busy || hasExpandedReasoning,
  );
  const count = entries.filter((entry) => conversationTool(entry.part)).length;
  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="group/work not-prose"
      data-conversation-work
    >
      <CollapsibleTrigger
        className="flex w-full items-center gap-2 border-b border-border/50 pb-2 text-left text-ui-base text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        title={count > 0 ? tr("{count} tool calls", { count }) : undefined}
      >
        <span>{tr(busy ? "Working" : "Worked")}</span>
        <HugeiconsIcon
          icon={ArrowRight01Icon}
          size={11}
          className="shrink-0 transition-transform group-data-[state=open]/work:rotate-90"
        />
      </CollapsibleTrigger>
      <CollapsibleContent className="rcode-collapsible-content">
        <div className="pt-5">
          <AssistantEntries
            entries={entries}
            onApproval={onApproval}
            streamingMessageId={streamingMessageId}
          />
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});

function AssistantEntries({
  entries,
  onApproval,
  streamingMessageId,
}: {
  entries: ConversationEntry[];
  onApproval: (id: string, approved: boolean) => void;
  streamingMessageId: string | null;
}) {
  const groups = useMemo(() => {
    const result: {
      key: string;
      entries: ConversationEntry[];
      exploration: boolean;
    }[] = [];
    for (const entry of entries) {
      const tool = conversationTool(entry.part);
      const exploration =
        !!tool &&
        ["read_file", "list_directory", "grep", "glob"].includes(tool.name) &&
        conversationWorkState(tool) !== "failed" &&
        tool.state !== "approval-requested";
      const last = result[result.length - 1];
      if (exploration && last?.exploration) last.entries.push(entry);
      else result.push({ key: entry.key, entries: [entry], exploration });
    }
    return result;
  }, [entries]);
  return (
    <div className="flex min-w-0 flex-col gap-4">
      {groups.map((group) =>
        group.exploration && group.entries.length > 1 ? (
          <ExplorationGroup
            key={group.key}
            entries={group.entries}
            onApproval={onApproval}
          />
        ) : (
          group.entries.map((entry) => (
            <RenderedPart
              key={entry.key}
              entryKey={entry.key}
              part={entry.part as AnyPart}
              onApproval={onApproval}
              streaming={
                entry.messageId === streamingMessageId &&
                (entry.part.type === "text" ||
                  entry.part.type === "reasoning") &&
                entry.part.state === "streaming"
              }
            />
          ))
        ),
      )}
    </div>
  );
}

const ExplorationGroup = memo(function ExplorationGroup({
  entries,
  onApproval,
}: {
  entries: ConversationEntry[];
  onApproval: (id: string, approved: boolean) => void;
}) {
  const tr = useTranslation();
  const [open, setOpen] = useConversationDisclosure(
    `exploration:${entries[0].key}`,
  );
  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="group/exploration not-prose"
    >
      <CollapsibleTrigger className="group/exploration-trigger inline-flex max-w-full items-center gap-2 self-start text-left text-ui-base leading-5 text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring">
        <HugeiconsIcon icon={File01Icon} size={16} />
        <span>{tr("Explored {count} items", { count: entries.length })}</span>
        <HugeiconsIcon
          icon={ArrowRight01Icon}
          size={11}
          className="opacity-0 transition-transform group-hover/exploration-trigger:opacity-100 group-focus-visible/exploration-trigger:opacity-100 group-data-[state=open]/exploration:rotate-90 group-data-[state=open]/exploration:opacity-100 [@media(hover:none)]:opacity-100"
        />
      </CollapsibleTrigger>
      <CollapsibleContent className="rcode-collapsible-content">
        <div className="ml-2 mt-2 flex flex-col gap-2 border-l border-border pl-3.5">
          {entries.map((entry) => (
            <RenderedTool
              key={entry.key}
              part={entry.part as AnyToolPart}
              entryKey={entry.key}
              onApproval={onApproval}
            />
          ))}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});

const ThinkingPart = memo(function ThinkingPart({
  id,
  text,
  streaming,
}: {
  id: string;
  text: string;
  streaming: boolean;
}) {
  const tr = useTranslation();
  const [open, setOpen] = useConversationDisclosure(`reasoning:${id}`);
  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="group/thinking not-prose"
      data-conversation-reasoning
    >
      <CollapsibleTrigger className="flex max-w-full items-center gap-1.5 rounded-md py-1 text-ui-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring">
        {streaming && <Spinner className="size-3" />}
        <span>{tr(streaming ? "Thinking" : "Reasoned")}</span>
        <HugeiconsIcon
          icon={ArrowRight01Icon}
          size={11}
          className="transition-transform group-data-[state=open]/thinking:rotate-90"
        />
      </CollapsibleTrigger>
      <CollapsibleContent className="rcode-collapsible-content">
        <div className="mt-2 whitespace-pre-wrap wrap-break-word text-ui-base leading-relaxed text-muted-foreground">
          {text}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});

const ResponseActions = memo(function ResponseActions({
  text,
  metadata,
  onRegenerate,
}: {
  text: string;
  metadata?: unknown;
  onRegenerate?: () => void | PromiseLike<void>;
}) {
  const tr = useTranslation();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const value = metadata as { createdAt?: string | number } | undefined;
  const date =
    value?.createdAt !== undefined ? new Date(value.createdAt) : undefined;
  const timestamp =
    date && Number.isFinite(date.getTime())
      ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      : undefined;
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
    } catch {
      toast.error(tr("Could not copy message"));
    }
  };
  return (
    <MessageActions
      className="min-h-7 text-muted-foreground opacity-0 transition-opacity group-hover/assistant-turn:opacity-100 focus-within:opacity-100 [@media(hover:none)]:opacity-100 motion-reduce:transition-none"
      data-conversation-reply-actions
    >
      <MessageAction
        tooltip={tr(copied ? "Copied" : "Copy response")}
        className="size-7"
        onClick={() => void copy()}
      >
        <HugeiconsIcon icon={copied ? Tick01Icon : Copy01Icon} size={14} />
      </MessageAction>
      {onRegenerate && (
        <MessageAction
          tooltip={tr("Retry response")}
          className="size-7"
          onClick={() => void onRegenerate()}
        >
          <HugeiconsIcon icon={RefreshIcon} size={14} />
        </MessageAction>
      )}
      {timestamp && (
        <span className="ml-1 text-ui-caption tabular-nums">{timestamp}</span>
      )}
    </MessageActions>
  );
});

const aiStreamdownComponents = {
  a: (props: MarkdownLinkProps) => (
    <MarkdownLink {...props} onSettled={useChatStore.getState().focusInput} />
  ),
  code: MarkdownCode,
};

function AiMessageResponse(props: Omit<MessageResponseProps, "components">) {
  return <MessageResponse {...props} components={aiStreamdownComponents} />;
}

const RenderedPart = memo(function RenderedPart({
  part,
  entryKey,
  onApproval,
  streaming,
}: {
  part: AnyPart;
  entryKey: string;
  onApproval: (id: string, approved: boolean) => void;
  streaming: boolean;
}) {
  if (part.type === "text") {
    return (
      <AiMessageResponse streaming={streaming}>
        {(part as unknown as { text: string }).text}
      </AiMessageResponse>
    );
  }

  if (part.type === "data-rcode-diagnostic") {
    const data = (part as unknown as { data?: { message?: unknown } | null })
      .data;
    const message = typeof data?.message === "string" ? data.message : null;
    if (!message) return null;
    return (
      <p role="status" className="text-ui-sm text-muted-foreground">
        {message}
      </p>
    );
  }

  if (part.type === "reasoning") {
    return (
      <ThinkingPart
        id={entryKey}
        streaming={streaming}
        text={(part as unknown as { text: string }).text}
      />
    );
  }

  if (part.type === "file") return <MessageFiles parts={[part]} />;

  if (
    part.type === "dynamic-tool" ||
    (typeof part.type === "string" && part.type.startsWith("tool-"))
  ) {
    return (
      <RenderedTool
        part={part as unknown as AnyToolPart}
        entryKey={entryKey}
        onApproval={onApproval}
      />
    );
  }

  return null;
});

const RenderedTool = memo(function RenderedTool({
  part,
  entryKey,
  onApproval,
}: {
  part: AnyToolPart;
  entryKey: string;
  onApproval: (id: string, approved: boolean) => void;
}) {
  const tool = conversationTool(part);
  const toolName = tool?.name ?? part.type;
  const displayInput = tool
    ? {
        ...tool.input,
        path: tool.input.path ?? tool.input.file_path,
        ...(toolName === "run_subagent"
          ? {
              agent:
                tool.input.description ?? tool.input.agent ?? tool.input.type,
            }
          : {}),
      }
    : part.input;

  if (part.state === "approval-requested") {
    return (
      <AiToolApproval
        part={part as Extract<ToolUIPart, { state: "approval-requested" }>}
        toolName={toolName}
        onRespond={(approved) => onApproval(part.approval.id, approved)}
      />
    );
  }

  return tool ? (
    <ConversationToolRow
      tool={{ ...tool, input: displayInput as Record<string, unknown> }}
      entryKey={entryKey}
    />
  ) : null;
});
