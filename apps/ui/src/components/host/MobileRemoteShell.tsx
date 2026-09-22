import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Badge } from "@appica/ui-react/badge";
import { Textarea } from "@/components/ui/textarea";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AskUserModal } from "@/components/AskUserModal";
import { Button } from "@/components/ui/button";
import {
  IconArrowLeft,
  IconChevronDown,
  IconDesktop,
  IconList,
  IconPaperclip,
  IconStop,
  IconRefresh,
  IconSend,
  IconClose,
} from "@/components/icons";
import { useStickToBottom } from "@/hooks/useStickToBottom";
import type { AskUserPayload } from "@/lib/session";
import type { HostMode, HostUploadedAttachment } from "./hostMode";
import "./host-mode.css";

export type MobileRemoteConnection =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "offline"
  | "unauthorized";

export type MobileRemoteSessionStatus = "idle" | "running" | "waiting" | "failed";

export interface MobileRemoteSessionSummary {
  id?: string;
  title: string;
  summary: string;
  status: MobileRemoteSessionStatus;
  updatedLabel?: string;
}

export interface MobileRemoteMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
  occurredAtMs: number;
  turnId: string | null;
}

export interface MobileRemoteActivity {
  id: string;
  kind: "tool" | "diff" | "agent";
  title: string;
  detail?: string | null;
  status: string;
}

export interface MobileRemoteLabels {
  title: string;
  sessions: string;
  closeSessions: string;
  reconnect: string;
  connecting: string;
  connected: string;
  reconnecting: string;
  offline: string;
  unauthorized: string;
  sessionRunning: string;
  sessionIdle: string;
  sessionWaiting: string;
  sessionFailed: string;
  waitingForAnswer: string;
  emptySession: string;
  emptyMessages: string;
  unsupportedHost: string;
  composerPlaceholder: string;
  send: string;
  stop: string;
  retry: string;
  restoring: string;
  attach: string;
  uploading: string;
  removeAttachment: string;
  backToBottom: string;
}

export const defaultMobileRemoteLabels: MobileRemoteLabels = {
  title: "KeenCode Remote",
  sessions: "会话",
  closeSessions: "返回会话",
  reconnect: "重新连接",
  connecting: "正在连接",
  connected: "已连接",
  reconnecting: "正在重连",
  offline: "离线",
  unauthorized: "需要授权",
  sessionRunning: "运行中",
  sessionIdle: "空闲",
  sessionWaiting: "等待输入",
  sessionFailed: "失败",
  waitingForAnswer: "会话正在等待你的回答。",
  emptySession: "暂无远程会话",
  emptyMessages: "这个会话还没有消息",
  unsupportedHost: "移动远程界面仅用于 Web 与移动远程宿主。",
  composerPlaceholder: "输入消息",
  send: "发送",
  stop: "停止",
  retry: "重试",
  restoring: "正在恢复会话…",
  attach: "添加附件",
  uploading: "正在上传附件…",
  removeAttachment: "移除附件",
  backToBottom: "回到底部",
};

export interface MobileRemoteShellProps {
  hostMode: HostMode;
  connection: MobileRemoteConnection;
  session?: MobileRemoteSessionSummary | null;
  sessions?: MobileRemoteSessionSummary[];
  selectedSessionId?: string | null;
  messages?: MobileRemoteMessage[];
  activities?: MobileRemoteActivity[];
  asking?: boolean;
  pendingAsk?: AskUserPayload | null;
  restoring?: boolean;
  sending?: boolean;
  errorMessage?: string | null;
  onReconnect: () => void | Promise<void>;
  onSelectSession?: (sessionId: string) => void | Promise<void>;
  onSend?: (text: string, attachments?: HostUploadedAttachment[]) => void | Promise<void>;
  onUploadAttachment?: (file: File) => Promise<HostUploadedAttachment>;
  onStop?: () => void | Promise<void>;
  onRetry?: () => void | Promise<void>;
  onAnswer?: (answers: Record<string, string | string[]>) => void | Promise<void>;
  onCancelAnswer?: () => void | Promise<void>;
  children?: ReactNode;
  composer?: ReactNode;
  labels?: Partial<MobileRemoteLabels>;
}

function connectionLabel(state: MobileRemoteConnection, labels: MobileRemoteLabels): string {
  return labels[state];
}

function connectionVariant(
  state: MobileRemoteConnection,
): "success" | "info" | "warning" | "error" | "soft" {
  if (state === "connected") return "success";
  if (state === "connecting" || state === "reconnecting") return "info";
  if (state === "offline") return "warning";
  if (state === "unauthorized") return "error";
  return "soft";
}

function sessionLabel(state: MobileRemoteSessionStatus, labels: MobileRemoteLabels): string {
  if (state === "running") return labels.sessionRunning;
  if (state === "waiting") return labels.sessionWaiting;
  if (state === "failed") return labels.sessionFailed;
  return labels.sessionIdle;
}

function sessionVariant(state: MobileRemoteSessionStatus): "error" | "info" | "warning" | "soft" {
  if (state === "failed") return "error";
  if (state === "running") return "info";
  if (state === "waiting") return "warning";
  return "soft";
}

export function MobileRemoteShell({
  hostMode,
  connection,
  session = null,
  sessions = session ? [session] : [],
  selectedSessionId = session?.id ?? null,
  messages = [],
  activities = [],
  asking = false,
  pendingAsk = null,
  restoring = false,
  sending = false,
  errorMessage,
  onReconnect,
  onSelectSession,
  onSend,
  onUploadAttachment,
  onStop,
  onRetry,
  onAnswer,
  onCancelAnswer,
  children,
  composer,
  labels: labelOverrides,
}: MobileRemoteShellProps) {
  const labels = { ...defaultMobileRemoteLabels, ...labelOverrides };
  const [draft, setDraft] = useState("");
  const [showSessions, setShowSessions] = useState(false);
  const [attachments, setAttachments] = useState<HostUploadedAttachment[]>([]);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const isRemote = hostMode === "mobile-remote" || hostMode === "web";
  const selected = sessions.find((item) => item.id === selectedSessionId) ?? session;
  const busy = selected?.status === "running" || sending;
  const {
    viewportRef: messagesViewportRef,
    contentRef: messagesContentRef,
    scrollToBottom,
    isPinnedRef,
    showBack,
  } = useStickToBottom({
    conversationKey: selectedSessionId ?? session?.id ?? "empty",
    contentReadyKey: `${messages.length}:${activities.length}:${pendingAsk ? "ask" : ""}`,
    enabled: isRemote,
  });

  useEffect(() => {
    if (!isRemote) return;
    // 流式内容只在用户仍处于吸底状态时跟随；上滑后由 hook 保留阅读位置。
    const frame = requestAnimationFrame(() => {
      if (isPinnedRef.current) scrollToBottom("instant");
    });
    return () => cancelAnimationFrame(frame);
  }, [activities, children, isPinnedRef, isRemote, messages, pendingAsk, scrollToBottom]);

  useEffect(() => {
    if (!showSessions) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setShowSessions(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [showSessions]);

  if (!isRemote) {
    return (
      <section className="host-panel host-panel--unsupported" data-host-mode={hostMode} data-testid="mobile-remote-shell">
        <div className="host-panel__icon" aria-hidden="true"><IconDesktop size={20} /></div>
        <p>{labels.unsupportedHost}</p>
      </section>
    );
  }

  const submit = () => {
    const value = draft.trim();
    if ((!value && attachments.length === 0) || !onSend || busy || uploading || connection !== "connected") return;
    setDraft("");
    const uploaded = attachments;
    setAttachments([]);
    void onSend(value, uploaded);
  };

  const uploadFiles = async (files: FileList | null) => {
    if (!files?.length || !onUploadAttachment || uploading) return;
    setUploading(true);
    setUploadError(null);
    try {
      const uploaded: HostUploadedAttachment[] = [];
      for (const file of Array.from(files)) uploaded.push(await onUploadAttachment(file));
      setAttachments((current) => [...current, ...uploaded]);
    } catch (error) {
      setUploadError(error instanceof Error ? error.message : "附件上传失败。");
    } finally {
      setUploading(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  return (
    <section
      className={`mobile-remote-shell${showSessions ? " mobile-remote-shell--sessions-open" : ""}`}
      data-host-mode={hostMode}
      data-connection={connection}
      data-testid="mobile-remote-shell"
    >
      <header className="mobile-remote-shell__header">
        <Button
          type="button"
          variant="ghost"
          size="icon-md"
          className="mobile-remote-shell__sessions-toggle"
          aria-label={showSessions ? labels.closeSessions : labels.sessions}
          onClick={() => setShowSessions((value) => !value)}
        >
          {showSessions ? <IconArrowLeft size={19} /> : <IconList size={19} />}
        </Button>
        <div className="mobile-remote-shell__heading">
          <strong>{selected?.title || labels.title}</strong>
          <Badge size="md" variant={connectionVariant(connection)}>
            {connectionLabel(connection, labels)}
          </Badge>
        </div>
        {connection !== "connected" ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-md"
            aria-label={labels.reconnect}
            onClick={() => void onReconnect()}
            disabled={connection === "connecting" || connection === "reconnecting"}
          >
            <IconRefresh size={18} />
          </Button>
        ) : <span className="mobile-remote-shell__header-spacer" />}
      </header>

      {showSessions ? (
        <Button
          type="button"
          variant="ghost"
          size="md"
          className="mobile-remote-shell__session-backdrop"
          aria-label={labels.closeSessions}
          data-testid="mobile-remote-session-backdrop"
          onClick={() => setShowSessions(false)}
        />
      ) : null}

      <aside className="mobile-remote-shell__sidebar" aria-label={labels.sessions}>
        <div className="mobile-remote-shell__brand">
          <div className="host-panel__icon" aria-hidden="true"><IconDesktop size={19} /></div>
          <strong>{labels.title}</strong>
        </div>
        <div className="mobile-remote-shell__session-list">
          {sessions.length === 0 ? (
            <div className="mobile-remote-shell__empty">{labels.emptySession}</div>
          ) : sessions.map((item, index) => {
            const id = item.id ?? `session-${index}`;
            const active = id === selectedSessionId || (!selectedSessionId && item === selected);
            return (
              <Button
                key={id}
                type="button"
                variant={active ? "soft" : "ghost"}
                className="mobile-remote-shell__session-item"
                aria-current={active ? "page" : undefined}
                onClick={() => {
                  setShowSessions(false);
                  if (item.id) void onSelectSession?.(item.id);
                }}
              >
                <span className="mobile-remote-shell__session-copy">
                  <strong>{item.title}</strong>
                  <span>{item.summary}</span>
                </span>
                <Badge size="md" variant={sessionVariant(item.status)}>
                  {sessionLabel(item.status, labels)}
                </Badge>
              </Button>
            );
          })}
        </div>
      </aside>

      <main className="mobile-remote-shell__conversation">
        {errorMessage || uploadError ? (
          <Alert variant="error" role="alert"><AlertDescription>{uploadError ?? errorMessage}</AlertDescription></Alert>
        ) : null}
        {restoring ? (
          <div className="mobile-remote-shell__restoring" role="status">{labels.restoring}</div>
        ) : null}
        {asking || pendingAsk ? (
          <Alert variant="warning" role="status"><AlertDescription>{labels.waitingForAnswer}</AlertDescription></Alert>
        ) : null}

        <div className="mobile-remote-shell__messages-wrap">
          <div ref={messagesViewportRef} className="mobile-remote-shell__messages" aria-live="polite">
            <div ref={messagesContentRef} className="mobile-remote-shell__messages-content">
              {!selected ? (
                <div className="mobile-remote-shell__empty">{labels.emptySession}</div>
              ) : messages.length === 0 && !children ? (
                <div className="mobile-remote-shell__empty">{labels.emptyMessages}</div>
              ) : null}
              {messages.map((message) => (
                <article key={message.id} className={`mobile-remote-message mobile-remote-message--${message.role}`}>
                  <p>{message.text}</p>
                </article>
              ))}
              {activities.length ? (
                <section className="mobile-remote-activities" aria-label="任务活动">
                  {activities.map((activity) => (
                    <article key={activity.id} className="mobile-remote-activity" data-kind={activity.kind}>
                      <div className="mobile-remote-activity__heading">
                        <strong>{activity.title}</strong>
                        <Badge size="md" variant={activity.status === "failed" ? "error" : activity.status === "completed" ? "success" : "info"}>
                          {activity.status}
                        </Badge>
                      </div>
                      {activity.detail ? <pre>{activity.detail}</pre> : null}
                    </article>
                  ))}
                </section>
              ) : null}
              {children}
              {pendingAsk && onAnswer && onCancelAnswer ? (
                <AskUserModal
                  payload={pendingAsk}
                  labels={{
                    title: labels.waitingForAnswer,
                    submit: "提交",
                    next: "下一题",
                    cancel: "取消",
                    otherPlaceholder: "输入回答",
                    freeTextHint: "输入其他回答",
                    multiHint: "可选择多个选项",
                    close: "关闭",
                  }}
                  onSubmit={onAnswer}
                  onCancel={onCancelAnswer}
                />
              ) : null}
            </div>
          </div>
          {showBack ? (
            <Button
              type="button"
              variant="outline"
              size="md"
              className="mobile-remote-shell__back-to-bottom"
              data-testid="mobile-remote-back-to-bottom"
              onClick={() => scrollToBottom("smooth")}
            >
              <IconChevronDown size={16} />
              <span>{labels.backToBottom}</span>
            </Button>
          ) : null}
        </div>

        {composer ?? (selected && onSend ? (
          <form
            className="mobile-remote-shell__composer"
            onSubmit={(event) => { event.preventDefault(); submit(); }}
          >
            {attachments.length ? (
              <div className="mobile-remote-attachments" aria-label="附件">
                {attachments.map((attachment) => (
                  <div className="mobile-remote-attachment" key={attachment.resourceId}>
                    {attachment.contentType.startsWith("image/") ? (
                      <img src={attachment.previewUrl} alt={attachment.fileName} />
                    ) : <IconPaperclip size={18} aria-hidden="true" />}
                    <span><strong>{attachment.fileName}</strong><small>{attachment.contentType} · {attachment.size} B</small></span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-md"
                      aria-label={`${labels.removeAttachment} ${attachment.fileName}`}
                      onClick={() => setAttachments((current) => current.filter((item) => item.resourceId !== attachment.resourceId))}
                    ><IconClose size={16} /></Button>
                  </div>
                ))}
              </div>
            ) : null}
            <Textarea
              inputSize="md"
              rows={2}
              value={draft}
              disabled={connection !== "connected" || restoring || Boolean(pendingAsk)}
              placeholder={labels.composerPlaceholder}
              aria-label={labels.composerPlaceholder}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  submit();
                }
              }}
            />
            <div className="mobile-remote-shell__composer-actions">
              {onUploadAttachment ? (
                <>
                  {/* 浏览器文件选择器必须由原生 input 承载，外观和交互入口仍使用统一 Button。 */}
                  <input
                    ref={fileInputRef}
                    className="mobile-remote-file-input"
                    type="file"
                    multiple
                    tabIndex={-1}
                    aria-hidden="true"
                    onChange={(event) => void uploadFiles(event.currentTarget.files)}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-md"
                    aria-label={uploading ? labels.uploading : labels.attach}
                    title={uploading ? labels.uploading : labels.attach}
                    disabled={uploading || busy || connection !== "connected" || restoring}
                    onClick={() => fileInputRef.current?.click()}
                  ><IconPaperclip size={18} /></Button>
                </>
              ) : null}
              {busy && onStop ? (
                <Button type="button" variant="outline" onClick={() => void onStop()}>
                  <IconStop size={17} /><span>{labels.stop}</span>
                </Button>
              ) : selected.status === "failed" && onRetry ? (
                <Button type="button" variant="outline" onClick={() => void onRetry()}>
                  <IconRefresh size={17} /><span>{labels.retry}</span>
                </Button>
              ) : null}
              <Button
                type="submit"
                variant="primary"
                size="icon-md"
                aria-label={labels.send}
                disabled={(!draft.trim() && attachments.length === 0) || busy || uploading || connection !== "connected" || restoring || Boolean(pendingAsk)}
              >
                <IconSend size={18} />
              </Button>
            </div>
          </form>
        ) : null)}
      </main>
    </section>
  );
}
