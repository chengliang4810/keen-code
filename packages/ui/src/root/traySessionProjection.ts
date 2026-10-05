import {
  desktopMenuMessageIds,
  getDesktopMenuMessage,
  type ExitRequestedPayload,
  type Locale,
} from "@zcode/shared";

export interface TraySessionProjectionItem {
  taskId: string;
  title: string;
  workspacePath: string;
  workspaceIdentity?: string;
  unreadAt?: number;
}

export interface TrayMenuProjection {
  labels: {
    newChat: string;
    show: string;
    quit: string;
  };
  sessions: Array<{
    id: string;
    title: string;
  }>;
}

/**
 * 退出事件只接受 Rust 查询到的正整数活动会话数；无效载荷不能打开确认框，
 * 避免 renderer 把错误事件误当成已确认的退出流程。
 */
export function isValidExitRequestedPayload(payload: unknown): payload is ExitRequestedPayload {
  if (typeof payload !== "object" || payload === null) {
    return false;
  }
  const activeCount = (payload as { activeCount?: unknown }).activeCount;
  return typeof activeCount === "number" && Number.isInteger(activeCount) && activeCount > 0;
}

/**
 * 托盘只接收当前任务列表的短投影；会话正文、workspace 归属和未读 CAS
 * 仍由调用方保留，Rust 菜单本身不成为第二事实源。
 */
export function buildTrayMenuProjection(
  locale: Locale,
  items: readonly TraySessionProjectionItem[],
): TrayMenuProjection {
  const seen = new Set<string>();
  const sessions = items.flatMap((item) => {
    const id = item.taskId.trim();
    if (!id || seen.has(id)) {
      return [];
    }
    seen.add(id);
    return [{ id, title: item.title }];
  });

  return {
    labels: {
      newChat: getDesktopMenuMessage(locale, desktopMenuMessageIds.trayNewChat),
      show: getDesktopMenuMessage(locale, desktopMenuMessageIds.trayShow),
      quit: getDesktopMenuMessage(locale, desktopMenuMessageIds.trayQuitApp),
    },
    sessions,
  };
}

/** 根据稳定 taskId 找到完整 workspace 目标；找不到时保持 stale 事件失败关闭。 */
export function findTraySession(
  items: readonly TraySessionProjectionItem[],
  sessionId: string,
): TraySessionProjectionItem | null {
  const normalizedId = sessionId.trim();
  if (!normalizedId) {
    return null;
  }
  return items.find((item) => item.taskId === normalizedId) ?? null;
}
