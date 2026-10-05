import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

function scheduleMicrotask(callback: () => void): void {
  // 合成 scroll 可能在 ConversationTimeline 的布局提交后立即触发；把关闭
  // 推到当前原生事件结束后，避免在同一提交链里同步推进选区 React 状态。
  if (typeof globalThis.queueMicrotask === "function") {
    globalThis.queueMicrotask(callback);
    return;
  }
  void Promise.resolve().then(callback);
}

function isConversationSelectionReferenceRecord(
  value: Record<string, unknown>,
): boolean {
  if (typeof value.contentType !== "string" || typeof value.text !== "string") {
    return false;
  }
  if (value.contentType === "markdown") {
    return typeof value.sourceKey === "string" && typeof value.sourceTitle === "string";
  }
  return (
    (value.contentType === "user" ||
      value.contentType === "assistant" ||
      value.contentType === "reasoning" ||
      value.contentType === "tool") &&
    typeof value.sourceSessionId === "string" &&
    Number.isSafeInteger(value.sourceRowId)
  );
}

function equalTextSelectionValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (
    left === null ||
    right === null ||
    typeof left !== "object" ||
    typeof right !== "object"
  ) {
    return false;
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) {
      return false;
    }
    return left.every((value, index) => equalTextSelectionValue(value, right[index]));
  }
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  // 仅对具备完整来源字段的 ConversationSelectionReference 忽略随机 id；
  // 普通嵌套对象的 id 仍是可观察状态，不能被通用递归吞掉。
  const ignoreReferenceId =
    isConversationSelectionReferenceRecord(leftRecord) &&
    isConversationSelectionReferenceRecord(rightRecord);
  const keys = new Set([...Object.keys(leftRecord), ...Object.keys(rightRecord)]);
  for (const key of keys) {
    if (ignoreReferenceId && key === "id") continue;
    if (
      !Object.prototype.hasOwnProperty.call(leftRecord, key) ||
      !Object.prototype.hasOwnProperty.call(rightRecord, key) ||
      !equalTextSelectionValue(leftRecord[key], rightRecord[key])
    ) {
      return false;
    }
  }
  return true;
}

/**
 * 选区检查器每次读取 Range 都会创建新的位置对象；只要字段没有变化，
 * 合成 scroll/selectionchange 事件就不应把同一份 UI 投影再次推进 React 更新。
 */
export function areTextSelectionValuesEqual<T>(left: T, right: T): boolean {
  return equalTextSelectionValue(left, right);
}

/** 统一鼠标、键盘和触控选区的监听；作用域变化后旧选区不能路由到新任务。 */
export function useTextSelection<T>({
  rootRef,
  enabled,
  inspect,
  scopeKey,
  observeSelectionChange = false,
}: {
  rootRef: RefObject<HTMLDivElement | null>;
  enabled: boolean;
  inspect: () => T | null;
  scopeKey: unknown;
  observeSelectionChange?: boolean;
}) {
  const [snapshot, setSnapshot] = useState<{ scopeKey: unknown; value: T } | null>(null);
  const frameRef = useRef(0);
  const mountedRef = useRef(false);
  const deferredCloseRevisionRef = useRef(0);
  const commitSnapshot = useCallback(
    (next: { scopeKey: unknown; value: T } | null) => {
      setSnapshot((current) => {
        if (next === null) return current === null ? current : null;
        if (
          current !== null &&
          Object.is(current.scopeKey, next.scopeKey) &&
          areTextSelectionValuesEqual(current.value, next.value)
        ) {
          return current;
        }
        return next;
      });
    },
    [],
  );
  const close = useCallback(() => {
    window.cancelAnimationFrame(frameRef.current);
    deferredCloseRevisionRef.current += 1;
    commitSnapshot(null);
  }, [commitSnapshot]);
  const closeAfterEvent = useCallback(() => {
    window.cancelAnimationFrame(frameRef.current);
    const revision = deferredCloseRevisionRef.current + 1;
    deferredCloseRevisionRef.current = revision;
    scheduleMicrotask(() => {
      if (!mountedRef.current || deferredCloseRevisionRef.current !== revision) return;
      commitSnapshot(null);
    });
  }, [commitSnapshot]);
  useEffect(() => {
    close();
    const root = rootRef.current;
    if (!root || !enabled) return;
    mountedRef.current = true;
    const schedule = () => {
      window.cancelAnimationFrame(frameRef.current);
      frameRef.current = window.requestAnimationFrame(() => {
        const value = inspect();
        commitSnapshot(value ? { scopeKey, value } : null);
      });
    };
    const onKey = (event: KeyboardEvent) =>
      event.key === "Escape" ? close() : schedule();
    const onSelection = () => {
      if (window.getSelection()?.isCollapsed) closeAfterEvent();
      else if (observeSelectionChange) schedule();
    };
    root.addEventListener("mouseup", schedule);
    root.addEventListener("touchend", schedule, { passive: true });
    document.addEventListener("keyup", onKey);
    document.addEventListener("selectionchange", onSelection);
    root.addEventListener("scroll", closeAfterEvent, { passive: true, capture: true });
    window.addEventListener("resize", closeAfterEvent);
    return () => {
      mountedRef.current = false;
      deferredCloseRevisionRef.current += 1;
      window.cancelAnimationFrame(frameRef.current);
      root.removeEventListener("mouseup", schedule);
      root.removeEventListener("touchend", schedule);
      document.removeEventListener("keyup", onKey);
      document.removeEventListener("selectionchange", onSelection);
      root.removeEventListener("scroll", closeAfterEvent, true);
      window.removeEventListener("resize", closeAfterEvent);
    };
  }, [
    close,
    closeAfterEvent,
    commitSnapshot,
    enabled,
    inspect,
    observeSelectionChange,
    rootRef,
    scopeKey,
  ]);
  return {
    state: enabled && snapshot && snapshot.scopeKey === scopeKey ? snapshot.value : null,
    close,
  };
}
