import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import * as api from "@/lib/api";
import {
  loadUnreadTerminalResultsSafe,
  type UnreadTerminalResult,
} from "@/lib/sessionCompletion";

/** 后台形成终态但尚未由用户打开查看的 Session 结果集合。 */
export type UnreadTerminalResults = Map<string, UnreadTerminalResult>;

/**
 * 持有后台终态未读结果，并把数量投影到 macOS Dock 图标角标。
 *
 * 侧栏未读标记与 Dock 角标共用同一份状态，避免两处口径分叉。
 * 数量归零时推送 0，由后端移除角标；浏览器预览下静默跳过。
 */
export function useUnreadTerminalResults(appBooting: boolean): {
  unreadTerminalResults: UnreadTerminalResults;
  setUnreadTerminalResults: Dispatch<SetStateAction<UnreadTerminalResults>>;
} {
  const [unreadTerminalResults, setUnreadTerminalResults] = useState<
    UnreadTerminalResults
  >(() => loadUnreadTerminalResultsSafe(localStorage));
  /** 最近一次成功推送的数量，避免重复写入原生角标。 */
  const appliedBadgeRef = useRef<number | null>(null);

  useEffect(() => {
    // 启动页结束前不推送，避免品牌启动阶段出现角标。
    if (appBooting || !api.isTauri()) return;
    const count = unreadTerminalResults.size;
    if (appliedBadgeRef.current === count) return;
    appliedBadgeRef.current = count;
    void api.traySetBadge(count).catch(() => {
      // 推送失败时允许下一次变更重试，不打断工作台。
      appliedBadgeRef.current = null;
    });
  }, [appBooting, unreadTerminalResults]);

  return { unreadTerminalResults, setUnreadTerminalResults };
}
