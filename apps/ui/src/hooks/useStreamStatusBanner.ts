/** 顶部流状态横幅：本地错误卡、流卡顿与重试进度的共享状态。 */

import { useEffect, useMemo, useRef, useState, type Dispatch, type RefObject, type SetStateAction } from "react";
import { presentErrorBanner, type ErrorBannerView } from "@/lib/session";
import type { Locale } from "@/i18n";

/** 流卡顿诊断的展示状态（I06）；null 表示已关闭或未卡顿。 */
export interface StreamStallState {
  sessionId?: string;
  stallSeconds: number;
  tier?: string;
  sawModelOutput?: boolean;
  sawToolActivity?: boolean;
}

/** Provider 自动重试进度（session://retry）。 */
export interface RetryStatusState {
  attempt: number;
  maxAttempts: number;
  delayMs: number;
  reason: string;
}

/** `useStreamStatusBanner` 的输入依赖。 */
export interface UseStreamStatusBannerOptions {
  /** 本地化语言；错误横幅文案随语言重算。 */
  locale: Locale;
  /** 当前是否处于流式输出；驱动无障碍播报的起止边界。 */
  streaming: boolean;
  /** 流开始的播报文案。 */
  a11yStreaming: string;
  /** 流结束的播报文案。 */
  a11yDone: string;
}

/** `useStreamStatusBanner` 返回的横幅状态。 */
export interface StreamStatusBannerState {
  /** 无障碍播报文本；起止各播报一次，结束文案短暂保留后清空。 */
  streamA11yNote: string;
  /** 无法归属到回合的本地错误文本。 */
  localError: string | null;
  /** 更新本地错误的稳定 setter。 */
  setLocalError: Dispatch<SetStateAction<string | null>>;
  /** 错误技术详情折叠态；可见错误变化时自动折叠。 */
  errorDetailOpen: boolean;
  /** 更新详情折叠态的稳定 setter。 */
  setErrorDetailOpen: Dispatch<SetStateAction<boolean>>;
  /** 流卡顿诊断；由运行时事件路径更新。 */
  streamStall: StreamStallState | null;
  /** 更新流卡顿诊断的稳定 setter。 */
  setStreamStall: Dispatch<SetStateAction<StreamStallState | null>>;
  /** Provider 自动重试进度；成功、停止或失败后清除。 */
  retryStatus: RetryStatusState | null;
  /** 更新重试进度的稳定 setter。 */
  setRetryStatus: Dispatch<SetStateAction<RetryStatusState | null>>;
  /** 由本地错误派生的顶部错误卡视图。 */
  errorBanner: ErrorBannerView | null;
  /** 内部：跨渲染记忆上一次流状态的引用。 */
  wasStreamingRef: RefObject<boolean>;
}

/** 管理顶部错误卡、流卡顿、重试进度与无障碍播报。 */
export function useStreamStatusBanner(
  options: UseStreamStatusBannerOptions,
): StreamStatusBannerState {
  const { locale, streaming, a11yStreaming, a11yDone } = options;
  /** Polite SR announce for stream start/stop (not every token). */
  const [streamA11yNote, setStreamA11yNote] = useState("");
  const wasStreamingRef = useRef(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const [errorDetailOpen, setErrorDetailOpen] = useState(false);
  const [streamStall, setStreamStall] = useState<StreamStallState | null>(
    null,
  );
  const [retryStatus, setRetryStatus] = useState<RetryStatusState | null>(
    null,
  );
  // Agent 回合错误只进入对话气泡；顶部错误卡仅承载无法归属到回合的本地错误。
  const errorBanner = useMemo(
    () => presentErrorBanner(null, localError, locale),
    [localError, locale],
  );
  // Collapse technical dump whenever the visible error changes.
  useEffect(() => {
    setErrorDetailOpen(false);
  }, [errorBanner?.code, errorBanner?.summary, errorBanner?.detail]);
  // T15: announce stream start/end once (avoid token-level noise).
  useEffect(() => {
    if (streaming && !wasStreamingRef.current) {
      setStreamA11yNote(a11yStreaming);
    } else if (!streaming && wasStreamingRef.current) {
      setStreamA11yNote(a11yDone);
      const timer = window.setTimeout(() => setStreamA11yNote(""), 2500);
      wasStreamingRef.current = streaming;
      return () => window.clearTimeout(timer);
    }
    wasStreamingRef.current = streaming;
  }, [streaming, a11yStreaming, a11yDone]);

  return {
    streamA11yNote,
    localError,
    setLocalError,
    errorDetailOpen,
    setErrorDetailOpen,
    streamStall,
    setStreamStall,
    retryStatus,
    setRetryStatus,
    errorBanner,
    wasStreamingRef,
  };
}
