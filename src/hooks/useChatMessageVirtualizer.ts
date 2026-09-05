/**
 * ConversationThread 使用的可变高度消息窗口。
 * 吸底时始终挂载尾部；用户脱离吸底后按 scrollTop 截取窗口，并在重测行高时
 * 修正视口上方变化带来的阅读位置偏移。
 *
 * 防跳动策略：
 * - 使用调用方提供的内容感知高度估算，避免总滚动高度严重偏低；
 * - 忽略亚像素重测和不稳定的小幅收缩；
 * - 仅当变化行整体位于视口上方时修正 scrollTop；
 * - 为每行建立 ResizeObserver，使图片和视频加载后能更新行高；
 * - 合并密集重测，防止窗口在多次布局间振荡。
 *
 * 长会话性能策略：
 * - 每个动画帧最多处理一次滚动重算；
 * - 行高或消息数未变化时复用累计偏移；
 * - 使用自适应扩展距离；
 * - 脱离吸底时限制远端强制索引的扩展范围。
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import {
  CHAT_DEFAULT_ROW_ESTIMATE_PX,
  CHAT_VIRTUALIZE_THRESHOLD,
  computeChatVirtualWindow,
  cumulativeOffsets,
  resolveChatOverscanPx,
  scrollTopAfterHeightChange,
  shouldCommitRowHeight,
  type ChatVirtualWindow,
} from "@/lib/chatVirtualList";
import { markProgrammaticStickScroll } from "@/lib/stickToBottom";

export type UseChatMessageVirtualizerArgs = {
  /** 消息总数。 */
  itemCount: number;
  /** 获取指定消息的稳定键。 */
  getKey: (index: number) => string;
  /** 首次实测前根据内容估算行高。 */
  getEstimateHeight?: (index: number) => number;
  /** 会话滚动视口引用。 */
  viewportRef: RefObject<HTMLElement | null>;
  /** useStickToBottom 提供的非响应式吸底状态引用。 */
  isPinnedRef: RefObject<boolean>;
  /** 会话切换时用于重置行高缓存的稳定键。 */
  conversationKey?: string | number | null;
  /** 必须保持挂载的索引，例如查找命中项和流式回复。 */
  forceIndices?: readonly number[];
  /** 消息数低于该阈值时完整渲染，不启用占位。 */
  threshold?: number;
  /** 是否允许启用虚拟列表。 */
  enabled?: boolean;
};

export type UseChatMessageVirtualizerResult = {
  /** 当前是否正在使用虚拟窗口。 */
  virtualized: boolean;
  /** 当前挂载窗口的起始索引。 */
  start: number;
  /** 当前挂载窗口的结束索引，不包含该索引。 */
  end: number;
  /** 顶部未挂载内容的占位高度。 */
  paddingTop: number;
  /** 底部未挂载内容的占位高度。 */
  paddingBottom: number;
  /** 绑定到每个消息行外层，用于测量实际高度。 */
  measureRef: (index: number) => (el: HTMLElement | null) => void;
  /** 滚动后主动触发窗口重算；原生滚动监听也会调用。 */
  onViewportScroll: () => void;
};

/** 返回完整挂载状态，用于未启用虚拟列表时复用结果结构。 */
const full = (count: number): ChatVirtualWindow => ({
  start: 0,
  end: count,
  paddingTop: 0,
  paddingBottom: 0,
  totalHeight: 0,
});

/** 创建可变高度消息虚拟窗口，并维护测量缓存与阅读位置。 */
export function useChatMessageVirtualizer(
  args: UseChatMessageVirtualizerArgs,
): UseChatMessageVirtualizerResult {
  const {
    itemCount,
    getKey,
    getEstimateHeight,
    viewportRef,
    isPinnedRef,
    conversationKey = null,
    forceIndices = [],
    threshold = CHAT_VIRTUALIZE_THRESHOLD,
    enabled = true,
  } = args;

  const virtualized = enabled && itemCount >= threshold;
  /** 使用最新消息数完成会话切换重置，但不因消息数变化触发该 effect。 */
  const itemCountRef = useRef(itemCount);
  itemCountRef.current = itemCount;
  const heightsRef = useRef<Map<string, number>>(new Map());
  const getKeyRef = useRef(getKey);
  getKeyRef.current = getKey;
  const estimateRef = useRef(getEstimateHeight);
  estimateRef.current = getEstimateHeight;
  const forceRef = useRef(forceIndices);
  forceRef.current = forceIndices;
  const recomputeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** 行高修正写入的 scrollTop；吸底监听需要忽略对应的一次程序滚动。 */
  const ignoreScrollAdjustRef = useRef(false);
  /** 每个索引对应的 ResizeObserver，使媒体加载后继续更新行高。 */
  const rowObserversRef = useRef<Map<number, ResizeObserver>>(new Map());
  /** 将滚动触发的重算合并为每个动画帧一次。 */
  const scrollRafRef = useRef<number | null>(null);
  /**
   * 任一已提交行高变化时递增，用于失效累计偏移缓存。
   * 行高稳定时可避免每次滚动都执行 O(n) 重建。
   */
  const heightsVersionRef = useRef(0);
  const offsetsCacheRef = useRef<{
    /** 缓存对应的行高版本。 */
    version: number;
    /** 缓存对应的消息总数。 */
    count: number;
    /** 已计算的累计偏移。 */
    offsets: number[];
  } | null>(null);

  const [win, setWin] = useState<ChatVirtualWindow>(() => full(itemCount));

  // 切换会话时清空行高、偏移和观察器缓存。
  useEffect(() => {
    heightsRef.current.clear();
    heightsVersionRef.current = 0;
    offsetsCacheRef.current = null;
    for (const ro of rowObserversRef.current.values()) ro.disconnect();
    rowObserversRef.current.clear();
    setWin(full(itemCountRef.current));
  }, [conversationKey]);

  /** 获取指定索引的实测高度或内容感知估算高度。 */
  const getHeight = useCallback((index: number) => {
    const key = getKeyRef.current(index);
    const measured = heightsRef.current.get(key);
    if (measured != null) return measured;
    const est = estimateRef.current?.(index);
    // 允许内联或折叠工具行显式估算为 0，避免每个空工具行被错误补成 120px。
    if (est != null && Number.isFinite(est) && est >= 0) return est;
    return CHAT_DEFAULT_ROW_ESTIMATE_PX;
  }, []);

  /** 获取与当前行高版本对应的累计偏移缓存。 */
  const getOffsets = useCallback(() => {
    const version = heightsVersionRef.current;
    const cached = offsetsCacheRef.current;
    if (
      cached &&
      cached.version === version &&
      cached.count === itemCount
    ) {
      return cached.offsets;
    }
    const offsets = cumulativeOffsets(itemCount, getHeight);
    offsetsCacheRef.current = { version, count: itemCount, offsets };
    return offsets;
  }, [itemCount, getHeight]);

  /** 立即根据当前视口、行高和吸底状态重算窗口。 */
  const recomputeNow = useCallback(() => {
    if (!virtualized) {
      setWin((prev) => {
        const next = full(itemCount);
        return prev.start === next.start &&
          prev.end === next.end &&
          prev.paddingTop === 0 &&
          prev.paddingBottom === 0
          ? prev
          : next;
      });
      return;
    }
    const el = viewportRef.current;
    if (!el) {
      setWin(full(itemCount));
      return;
    }
    const pin = !!isPinnedRef.current;
    const offsets = getOffsets();
    const next = computeChatVirtualWindow({
      count: itemCount,
      getHeight,
      scrollTop: el.scrollTop,
      viewportHeight: el.clientHeight,
      overscanPx: resolveChatOverscanPx({
        viewportHeight: el.clientHeight,
        pinToBottom: pin,
      }),
      pinToBottom: pin,
      forceIndices: forceRef.current,
      offsets,
    });
    setWin((prev) => {
      if (
        prev.start === next.start &&
        prev.end === next.end &&
        prev.paddingTop === next.paddingTop &&
        prev.paddingBottom === next.paddingBottom &&
        prev.totalHeight === next.totalHeight
      ) {
        return prev;
      }
      // 吸底且索引范围不变时忽略亚像素占位抖动，避免底部闪烁和回弹。
      if (
        pin &&
        prev.start === next.start &&
        prev.end === next.end &&
        Math.abs(prev.paddingTop - next.paddingTop) < 3 &&
        Math.abs(prev.paddingBottom - next.paddingBottom) < 3 &&
        Math.abs(prev.totalHeight - next.totalHeight) < 6
      ) {
        return prev;
      }
      return next;
    });
  }, [virtualized, itemCount, viewportRef, isPinnedRef, getHeight, getOffsets]);

  /** 合并密集测量后延迟重算窗口。 */
  const recompute = useCallback(() => {
    // 合并高 Markdown 和表格重排产生的密集测量；吸底时适当延长合并窗口。
    if (recomputeTimerRef.current != null) {
      clearTimeout(recomputeTimerRef.current);
    }
    const delay = isPinnedRef.current ? 72 : 32;
    recomputeTimerRef.current = setTimeout(() => {
      recomputeTimerRef.current = null;
      recomputeNow();
    }, delay);
  }, [recomputeNow, isPinnedRef]);

  // 滚动触发重算，并通过 rAF 限制快速滚动时的更新频率。
  useEffect(() => {
    if (!virtualized) {
      setWin(full(itemCount));
      return;
    }
    const el = viewportRef.current;
    if (!el) return;
    const onScroll = () => {
      if (ignoreScrollAdjustRef.current) {
        ignoreScrollAdjustRef.current = false;
        return;
      }
      // 用户快速浏览历史时每帧最多更新一次窗口。
      if (scrollRafRef.current != null) return;
      scrollRafRef.current = requestAnimationFrame(() => {
        scrollRafRef.current = null;
        recomputeNow();
      });
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(() => recompute());
    ro.observe(el);
    recomputeNow();
    return () => {
      el.removeEventListener("scroll", onScroll);
      ro.disconnect();
      if (scrollRafRef.current != null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
      if (recomputeTimerRef.current != null) {
        clearTimeout(recomputeTimerRef.current);
        recomputeTimerRef.current = null;
      }
    };
  }, [virtualized, itemCount, viewportRef, recompute, recomputeNow, conversationKey]);

  // 已挂载消息流式增长或强制索引变化时立即重算。
  useLayoutEffect(() => {
    if (!virtualized) return;
    recomputeNow();
  }, [virtualized, itemCount, forceIndices, recomputeNow]);

  // 关闭虚拟列表时释放所有行观察器。
  useEffect(() => {
    if (virtualized) return;
    for (const ro of rowObserversRef.current.values()) ro.disconnect();
    rowObserversRef.current.clear();
  }, [virtualized]);

  /** 提交一行的实测高度，并在需要时修正阅读锚点。 */
  const commitRowHeight = useCallback(
    (index: number, el: HTMLElement) => {
      if (!virtualized) return;
      const key = getKeyRef.current(index);
      const nextH = Math.round(el.getBoundingClientRect().height);
      const prevH = heightsRef.current.get(key);
      if (!shouldCommitRowHeight(prevH, nextH)) return;

      const pin = !!isPinnedRef.current;
      const viewport = viewportRef.current;
      // 仅在已有实测行高的重测阶段修正滚动；首次从估算切到实测不做补偿，
      // 避免图表行在视口边缘因初次高度差过大而回弹。
      if (viewport && prevH != null && !pin) {
        const offsets = getOffsets();
        // 当前缓存仍是旧高度，因此使用旧累计偏移计算该行位置。
        const rowOffset = offsets[index] ?? 0;
        const delta = nextH - prevH;
        const adjusted = scrollTopAfterHeightChange({
          scrollTop: viewport.scrollTop,
          rowOffset,
          prevHeight: prevH,
          delta,
          pinToBottom: false,
        });
        if (Math.abs(adjusted - viewport.scrollTop) > 0.5) {
          ignoreScrollAdjustRef.current = true;
          viewport.scrollTop = adjusted;
        }
      }

      heightsRef.current.set(key, nextH);
      heightsVersionRef.current += 1;
      offsetsCacheRef.current = null;
      recompute();
      // 吸底时在行高提交后再次对齐真实底部，避免尾部短暂空白后回弹。
      if (pin && viewport) {
        requestAnimationFrame(() => {
          if (!isPinnedRef.current || !viewportRef.current) return;
          const v = viewportRef.current;
          const top = Math.max(0, v.scrollHeight - v.clientHeight);
          if (Math.abs(v.scrollTop - top) > 0.5) {
            ignoreScrollAdjustRef.current = true;
            markProgrammaticStickScroll(v, top);
            v.scrollTop = top;
          }
        });
      }
    },
    [virtualized, getOffsets, isPinnedRef, viewportRef, recompute],
  );

  /**
   * 按索引缓存稳定的 ref 回调。
   * 若每次渲染都返回新函数，React 会反复解绑和绑定 ref，造成观察器抖动与滚动卡顿。
   */
  const measureCallbackCacheRef = useRef<
    Map<number, (el: HTMLElement | null) => void>
  >(new Map());

  // 关闭虚拟列表或切换会话时清空回调缓存。
  useEffect(() => {
    measureCallbackCacheRef.current.clear();
  }, [conversationKey, virtualized]);

  const measureRef = useCallback(
    (index: number) => {
      const cached = measureCallbackCacheRef.current.get(index);
      if (cached) return cached;
      const cb = (el: HTMLElement | null) => {
        const prevRo = rowObserversRef.current.get(index);
        if (prevRo) {
          prevRo.disconnect();
          rowObserversRef.current.delete(index);
        }
        if (!el || !virtualized) return;

        // 挂载时立即测量，并持续观察后续媒体加载和布局增长。
        commitRowHeight(index, el);
        const ro = new ResizeObserver(() => {
          commitRowHeight(index, el);
        });
        ro.observe(el);
        rowObserversRef.current.set(index, ro);
      };
      measureCallbackCacheRef.current.set(index, cb);
      return cb;
    },
    [virtualized, commitRowHeight],
  );

  if (!virtualized) {
    return {
      virtualized: false,
      start: 0,
      end: itemCount,
      paddingTop: 0,
      paddingBottom: 0,
      measureRef,
      onViewportScroll: recomputeNow,
    };
  }

  return {
    virtualized: true,
    start: win.start,
    end: win.end,
    paddingTop: win.paddingTop,
    paddingBottom: win.paddingBottom,
    measureRef,
    onViewportScroll: recomputeNow,
  };
}
