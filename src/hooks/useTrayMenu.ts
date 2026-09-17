import { useEffect, useMemo, useRef } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import type { Project, SessionRow } from "@/features/app/models";

/** 托盘菜单里最多展示的最近会话数量。 */
const TRAY_SESSION_LIMIT = 5;

export interface UseTrayMenuOptions {
  locale: Locale;
  sessions: readonly SessionRow[];
  projects: readonly Project[];
  /** 启动页结束前不推送菜单，避免默认语言覆盖后端按持久化语言生成的兜底菜单。 */
  appBooting: boolean;
  /** 托盘菜单请求新建对话。 */
  newChat: (project?: Project | null) => void | Promise<void>;
  /** 托盘菜单请求打开指定会话。 */
  openSession: (session: SessionRow, project?: Project | null) => Promise<void>;
}

/**
 * 把当前会话投影为系统托盘菜单，并把菜单选择回投到既有导航入口。
 *
 * 后端托盘命令在非 Tauri 环境不存在，浏览器预览下静默跳过。
 */
export function useTrayMenu({
  locale,
  sessions,
  projects,
  appBooting,
  newChat,
  openSession,
}: UseTrayMenuOptions): void {
  const tr = useMemo(() => createT(locale), [locale]);
  /** 最近一次成功推送的菜单投影，避免会话刷新时反复重建原生菜单。 */
  const appliedRef = useRef<string | null>(null);

  const menu = useMemo<api.TrayMenuPayload>(() => {
    const recent = [...sessions]
      .filter((item) => !item.archived)
      .sort((left, right) => {
        const diff = Date.parse(right.updatedAt) - Date.parse(left.updatedAt);
        return Number.isFinite(diff) && diff !== 0
          ? diff
          : left.id.localeCompare(right.id);
      })
      .slice(0, TRAY_SESSION_LIMIT);
    return {
      labels: {
        newChat: tr("sidebar.newSession"),
        show: tr("tray.show"),
        quit: tr("tray.quit"),
      },
      sessions: recent.map((item) => ({
        id: item.id,
        title: item.title || tr("session.untitled"),
      })),
    };
  }, [sessions, tr]);

  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    const encoded = JSON.stringify(menu);
    if (appliedRef.current === encoded) return;
    appliedRef.current = encoded;
    void api.traySetMenu(menu).catch(() => {
      // 推送失败时允许下一次变更重试，不打断工作台。
      appliedRef.current = null;
    });
  }, [appBooting, menu]);

  const handlersRef = useRef({ newChat, openSession, sessions, projects });
  handlersRef.current = { newChat, openSession, sessions, projects };

  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      const unlistenNewChat = await listen("app://tray-new-chat", () => {
        void handlersRef.current.newChat();
      });
      const unlistenOpenSession = await listen<{ sessionId: string }>(
        "app://tray-open-session",
        (event) => {
          const current = handlersRef.current;
          const row = current.sessions.find(
            (item) => item.id === event.payload.sessionId,
          );
          if (!row) return;
          const project =
            current.projects.find((item) => item.id === row.projectId) ?? null;
          void current.openSession(row, project);
        },
      );
      if (disposed) {
        unlistenNewChat();
        unlistenOpenSession();
        return;
      }
      unlisten = () => {
        unlistenNewChat();
        unlistenOpenSession();
      };
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
