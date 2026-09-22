import { useEffect, useMemo, useState, type ReactNode } from "react";
import { MobileRemoteShell, type MobileRemoteSessionSummary } from "./MobileRemoteShell";
import { WebLoginPanel, type WebLoginStatus } from "./WebLoginPanel";
import {
  getInjectedHostTransportAdapter,
  resolveHostMode,
  type HostMode,
  type HostTransportAdapter,
} from "./hostMode";
import { useVisualViewportLayout } from "@/hooks/useVisualViewportLayout";
import { useHostRemoteSession } from "./useHostRemoteSession";
import "./host-mode.css";

export interface HostStartupShellProps {
  children: ReactNode;
  hostMode?: HostMode;
  transport?: HostTransportAdapter | null;
}

/** Desktop 保持原 App 树；Web 认证后复用完整工作台，移动端保持受限远程壳。 */
export function HostStartupShell({
  children,
  hostMode = resolveHostMode(),
  transport = getInjectedHostTransportAdapter(),
}: HostStartupShellProps) {
  if (hostMode === "desktop") return <>{children}</>;
  return (
    <RemoteStartupShell hostMode={hostMode} transport={transport}>
      {children}
    </RemoteStartupShell>
  );
}

function RemoteStartupShell({
  hostMode,
  transport,
  children,
}: {
  hostMode: Exclude<HostMode, "desktop">;
  transport: HostTransportAdapter | null;
  children: ReactNode;
}) {
  // Remote 根壳独立同步 visual viewport，软键盘收起/展开时不依赖桌面 App。
  useVisualViewportLayout();
  const [status, setStatus] = useState<WebLoginStatus>("signed-out");
  const [token, setToken] = useState("");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void Promise.resolve(transport?.snapshot?.()).then((snapshot) => {
      if (active && snapshot?.connection === "connected") setStatus("signed-in");
    }).catch(() => {});
    return () => { active = false; };
  }, [transport]);

  const submit = async () => {
    if (!transport?.authenticate) {
      setErrorMessage("Web Host transport adapter 尚未注入。");
      setStatus("error");
      return;
    }
    setStatus("submitting");
    setErrorMessage(null);
    try {
      const result = await transport.authenticate(token.trim());
      if (!result.authenticated) {
        setErrorMessage(result.error ?? null);
        setStatus("error");
        return;
      }
      setToken("");
      setStatus("signed-in");
    } catch (error) {
      setErrorMessage(error instanceof Error ? error.message : String(error));
      setStatus("error");
    }
  };

  if (status === "signed-in") {
    return (
      <HostAuthenticatedContent hostMode={hostMode} transport={transport}>
        {children}
      </HostAuthenticatedContent>
    );
  }

  // SSR/嵌入测试没有 transport 时仍给移动端明确的不可用状态，不挂载桌面 App。
  if (hostMode === "mobile-remote" && !transport) {
    return (
      <main className="host-startup-shell host-startup-shell--remote" data-host-mode={hostMode}>
        <MobileRemoteShell
          hostMode={hostMode}
          connection="unauthorized"
          onReconnect={() => {}}
        />
      </main>
    );
  }

  return (
    <main className="host-startup-shell host-startup-shell--login" data-host-mode={hostMode}>
      <WebLoginPanel
        hostMode={hostMode}
        status={status}
        token={token}
        onTokenChange={setToken}
        onSubmit={submit}
        errorMessage={errorMessage}
        labels={hostMode === "mobile-remote" ? {
          title: "连接 KeenCode Remote",
          description: "使用本机 Web Host Token 访问已授权会话。",
        } : undefined}
      />
    </main>
  );
}

/**
 * 登录后的宿主分流：桌面 Web 使用完整 App，移动远程仍使用轻量移动壳。
 * Web 与 Desktop 共用 ACP runtime，不能再把 Web 降级为移动端投影。
 */
export function HostAuthenticatedContent({
  hostMode,
  transport,
  children,
}: {
  hostMode: Exclude<HostMode, "desktop">;
  transport: HostTransportAdapter | null;
  children: ReactNode;
}) {
  if (hostMode === "web") {
    return (
      <main
        className="host-startup-shell host-startup-shell--web"
        data-host-mode="web"
        data-testid="web-workspace-shell"
      >
        {children}
      </main>
    );
  }
  return <RemoteWorkspace hostMode={hostMode} transport={transport} />;
}

function RemoteWorkspace({
  hostMode,
  transport,
}: {
  hostMode: Exclude<HostMode, "desktop">;
  transport: HostTransportAdapter | null;
}) {
  const remote = useHostRemoteSession(transport);
  const sessionSummaries = useMemo<MobileRemoteSessionSummary[]>(() => remote.sessions.map((item) => {
    const current = item.id === remote.selectedSessionId ? remote.current : null;
    const status = remote.pendingAsk && item.id === remote.selectedSessionId
      ? "waiting"
      : current?.status ?? "idle";
    const updated = item.updatedAt ? new Date(item.updatedAt) : null;
    const summary = updated && !Number.isNaN(updated.getTime())
      ? `更新于 ${updated.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}`
      : "远程会话";
    return {
      id: item.id,
      title: item.title?.trim() || "未命名会话",
      summary,
      status,
    };
  }), [remote.current, remote.pendingAsk, remote.selectedSessionId, remote.sessions]);

  const selected = sessionSummaries.find((item) => item.id === remote.selectedSessionId) ?? null;
  return (
    <main className="host-startup-shell host-startup-shell--remote" data-host-mode={hostMode}>
      <MobileRemoteShell
        hostMode={hostMode}
        connection={remote.connection}
        sessions={sessionSummaries}
        session={selected}
        selectedSessionId={remote.selectedSessionId}
        messages={remote.current?.messages ?? []}
        activities={remote.current?.activities ?? []}
        pendingAsk={remote.pendingAsk}
        asking={Boolean(remote.pendingAsk)}
        restoring={remote.restoring}
        sending={remote.sending}
        errorMessage={remote.error}
        onReconnect={remote.reconnect}
        onSelectSession={remote.selectSession}
        onSend={remote.send}
        onUploadAttachment={remote.uploadAttachment}
        onStop={remote.stop}
        onRetry={remote.retry}
        onAnswer={remote.answer}
        onCancelAnswer={remote.cancelAnswer}
      />
    </main>
  );
}
