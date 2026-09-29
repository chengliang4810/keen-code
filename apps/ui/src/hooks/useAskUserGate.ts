/** 根会话 AskUser 追问门的状态与按会话暂存账本。 */

import {
  useCallback,
  useRef,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import type { AskUserPayload } from "@/lib/session";

/** `useAskUserGate` 返回的追问门状态。 */
export interface AskUserGateState {
  /** 当前展示的追问；同一时刻最多一个。 */
  askUser: AskUserPayload | null;
  /** 更新当前追问的稳定 setter，供运行时事件与 UI 关闭路径共用。 */
  setAskUser: Dispatch<SetStateAction<AskUserPayload | null>>;
  /** 包裹追问对话框的容器；滚动与焦点恢复用。 */
  askUserWrapRef: RefObject<HTMLDivElement | null>;
  /** 后台会话未回答问题的暂存账本（sessionId → payload）。 */
  pendingAskUserBySessionRef: RefObject<Map<string, AskUserPayload>>;
  /** 仍有未回答问题的会话集合；驱动侧栏状态点。 */
  pendingAskUserSessionIds: Set<string>;
  /** 触发侧栏重渲染的稳定 setter。 */
  setPendingAskUserSessionIds: Dispatch<SetStateAction<Set<string>>>;
  /** 清除指定会话的未回答问题；rpcId 不同时保留后来到达的新问题。 */
  clearPendingAskUser: (sessionId?: string | null, rpcId?: string | number) => void;
  /** 只挂载一次的事件监听读取最新清理函数用的稳定引用。 */
  clearPendingAskUserRef: RefObject<
    (sessionId?: string | null, rpcId?: string | number) => void
  >;
}

/** 管理 AskUser 追问门与按会话的未回答问题账本。 */
export function useAskUserGate(): AskUserGateState {
  const [askUser, setAskUser] = useState<AskUserPayload | null>(null);
  const askUserWrapRef = useRef<HTMLDivElement>(null);
  /**
   * 后台任务也可以在用户查看其他任务时提出问题。这里按 Session 暂存未回答问题，
   * 切回任务时恢复显示，回答或本轮结束后删除。
   */
  const pendingAskUserBySessionRef = useRef<Map<string, AskUserPayload>>(
    new Map(),
  );
  const [pendingAskUserSessionIds, setPendingAskUserSessionIds] = useState<
    Set<string>
  >(new Set());
  const clearPendingAskUser = useCallback(
    (sessionId?: string | null, rpcId?: string | number) => {
      if (!sessionId) return;
      const pending = pendingAskUserBySessionRef.current.get(sessionId);
      if (rpcId != null && pending?.rpcId !== rpcId) return;
      pendingAskUserBySessionRef.current.delete(sessionId);
      setPendingAskUserSessionIds((previous) => {
        if (!previous.has(sessionId)) return previous;
        const next = new Set(previous);
        next.delete(sessionId);
        return next;
      });
    },
    [],
  );
  /** 为只挂载一次的事件监听保存最新问题清理函数。 */
  const clearPendingAskUserRef = useRef(clearPendingAskUser);
  clearPendingAskUserRef.current = clearPendingAskUser;

  return {
    askUser,
    setAskUser,
    askUserWrapRef,
    pendingAskUserBySessionRef,
    pendingAskUserSessionIds,
    setPendingAskUserSessionIds,
    clearPendingAskUser,
    clearPendingAskUserRef,
  };
}
