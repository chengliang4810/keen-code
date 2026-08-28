/**
 * 用户跟随最新内容时保持视口吸底；通过滚轮、触摸或滚动条向上浏览时解除吸底。
 *
 * 用户脱离后不会仅因仍处于接近底部范围就重新吸回；只有再次向下到达底部、
 * 显式强制吸底或切换会话时才恢复。程序写入的滚动会被监听器识别并忽略，
 * 防止尺寸变化和流式内容与用户手势相互争抢。
 */

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import {
  STICK_ESCAPE_MIN_DELTA_PX,
  STICK_ESCAPE_WHEEL_DELTA,
  STICK_TO_BOTTOM_THRESHOLD_PX,
  bottomScrollTop,
  isHardBottom,
  isHeightDeltaNoise,
  isMeaningfulScrollUp,
  isNearBottom,
  nextStickPinState,
  shouldReleaseStickOnScrollUp,
  takeProgrammaticStickScroll,
} from "@/lib/stickToBottom";

export type UseStickToBottomOptions = {
  /** 会话身份变化时重新吸底，例如切换 Session 或首条消息变化。 */
  conversationKey?: string | number | null;
  /** 变化时强制重新吸底，例如用户刚发送消息。 */
  forceStickKey?: string | number | null;
  /** 重新吸底所用的接近底部距离，单位为像素，默认 100。 */
  thresholdPx?: number;
  /** 是否启用吸底行为。 */
  enabled?: boolean;
};

export type UseStickToBottomResult = {
  /** 会话滚动视口引用。 */
  viewportRef: RefObject<HTMLDivElement | null>;
  /** 可选的内容列引用，用于更准确地观察内容尺寸。 */
  contentRef: RefObject<HTMLDivElement | null>;
  /** 主动滚动到底部并恢复吸底。 */
  scrollToBottom: (behavior?: ScrollBehavior) => void;
  /** 非响应式的自动跟随状态引用。 */
  isPinnedRef: RefObject<boolean>;
  /** 用户已向上浏览且内容溢出时，是否显示“回到底部”控件。 */
  showBack: boolean;
};

/** 创建会话视口的吸底、脱离和重新跟随控制器。 */
export function useStickToBottom(
  options: UseStickToBottomOptions = {},
): UseStickToBottomResult {
  const {
    conversationKey = null,
    forceStickKey = null,
    thresholdPx = STICK_TO_BOTTOM_THRESHOLD_PX,
    enabled = true,
  } = options;

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  /** 是否自动跟随流式内容增长。 */
  const isPinnedRef = useRef(true);
  /**
   * 用户是否主动离开底部；向下回到底部或强制吸底前一直保持 true，
   * 防止仅因仍位于接近底部范围而重新吸回。
   */
  const escapedRef = useRef(false);
  /**
   * 最后一次明确用户手势是否朝向最新内容；向上滚动时清除。
   * 与绝对底部判定结合后，即使最终事件没有正增量也能重新吸底。
   */
  const userIntentDownRef = useRef(false);
  const lastScrollTopRef = useRef(0);
  /** 最近一次程序写入的 scrollTop，用于忽略对应的合成滚动事件。 */
  const ignoreScrollTopRef = useRef<number | undefined>(undefined);
  /** 内容尺寸变化处理中保存的高度差，用于处理与滚动事件的竞争。 */
  const resizeDifferenceRef = useRef(0);
  const thresholdRef = useRef(thresholdPx);
  thresholdRef.current = thresholdPx;
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;

  const [showBack, setShowBack] = useState(false);

  /** 根据当前溢出和吸底状态同步“回到底部”按钮。 */
  const syncShowBack = useCallback(() => {
    const el = viewportRef.current;
    if (!el) {
      setShowBack(false);
      return;
    }
    const overflow = el.scrollHeight > el.clientHeight + 40;
    setShowBack(!isPinnedRef.current && overflow);
  }, []);

  /** 立即写入滚动位置，并标记对应的程序滚动事件。 */
  const applyScrollTop = useCallback((top: number) => {
    const el = viewportRef.current;
    if (!el) return;
    if (Math.abs(el.scrollTop - top) < 1) {
      ignoreScrollTopRef.current = el.scrollTop;
      return;
    }
    // 即使 CSS 设置了平滑滚动，也强制本次赋值立即生效。
    const prev = el.style.scrollBehavior;
    el.style.scrollBehavior = "auto";
    el.scrollTop = top;
    ignoreScrollTopRef.current = el.scrollTop;
    lastScrollTopRef.current = el.scrollTop;
    if (prev) el.style.scrollBehavior = prev;
    else el.style.removeProperty("scroll-behavior");
  }, []);

  /** 滚动到底部并恢复自动跟随。 */
  const scrollToBottom = useCallback(
    (behavior: ScrollBehavior = "instant") => {
      const el = viewportRef.current;
      if (!el) return;
      escapedRef.current = false;
      isPinnedRef.current = true;
      userIntentDownRef.current = false;
      const top = bottomScrollTop(el.scrollHeight, el.clientHeight);
      if (behavior === "smooth" && typeof el.scrollTo === "function") {
        // 平滑滚动仅用于显式“回到底部”按钮，并忽略过程中的中间滚动事件。
        ignoreScrollTopRef.current = top;
        el.scrollTo({ top, behavior: "smooth" });
        // 先恢复吸底状态，到达接近底部范围后再清除返回按钮。
        const start = performance.now();
        const tick = () => {
          if (!viewportRef.current) return;
          const near = isNearBottom(
            viewportRef.current.scrollTop,
            viewportRef.current.scrollHeight,
            viewportRef.current.clientHeight,
            thresholdRef.current,
          );
          if (near || performance.now() - start > 600) {
            applyScrollTop(
              bottomScrollTop(
                viewportRef.current.scrollHeight,
                viewportRef.current.clientHeight,
              ),
            );
            isPinnedRef.current = true;
            escapedRef.current = false;
            syncShowBack();
            return;
          }
          requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
      } else {
        applyScrollTop(top);
      }
      syncShowBack();
    },
    [applyScrollTop, syncShowBack],
  );

  /** 仅在仍处于吸底状态时跟随到最新内容。 */
  const followIfPinned = useCallback(() => {
    if (!isPinnedRef.current || !enabledRef.current) return;
    const el = viewportRef.current;
    if (!el) return;
    applyScrollTop(bottomScrollTop(el.scrollHeight, el.clientHeight));
  }, [applyScrollTop]);

  // 使用被动原生监听统一处理滚轮、触摸、滚动和尺寸变化。
  useEffect(() => {
    if (!enabled) return;
    const el = viewportRef.current;
    if (!el) return;

    const handleScroll = () => {
      const scrollTop = el.scrollTop;
      let lastScrollTop = lastScrollTopRef.current;
      const ignore =
        ignoreScrollTopRef.current ?? takeProgrammaticStickScroll(el);
      lastScrollTopRef.current = scrollTop;
      ignoreScrollTopRef.current = undefined;

      // 虚拟列表和吸底控制器的主动滚动不是用户离开底部。
      if (ignore != null && Math.abs(ignore - scrollTop) < 1) {
        syncShowBack();
        return;
      }

      // 程序跟随可能与用户向上滚动交织在同一个事件中。
      if (ignore != null && ignore > scrollTop) {
        lastScrollTop = ignore;
      }

      const maxTop = bottomScrollTop(el.scrollHeight, el.clientHeight);
      const meaningfulUp = isMeaningfulScrollUp(scrollTop, lastScrollTop);
      const shouldEscape = shouldReleaseStickOnScrollUp({
        pinned: isPinnedRef.current,
        scrollTop,
        previousScrollTop: lastScrollTop,
        scrollHeight: el.scrollHeight,
        clientHeight: el.clientHeight,
      });
      const meaningfulDown =
        scrollTop - lastScrollTop >= STICK_ESCAPE_MIN_DELTA_PX;

      // 底部锁定时忽略微小抖动、弹性滚动和虚假占位，仅明确向上拖动才解除。
      if (isPinnedRef.current && !escapedRef.current && !shouldEscape) {
        if (Math.abs(scrollTop - maxTop) > 0.5) {
          applyScrollTop(maxTop);
        }
        return;
      }

      // 检测到明确向上浏览时立即脱离，避免同一手势内的流式增长把用户拉回底部。
      if (shouldEscape) {
        userIntentDownRef.current = false;
        isPinnedRef.current = false;
        escapedRef.current = true;
        syncShowBack();
        return;
      }

      if (meaningfulUp) userIntentDownRef.current = false;
      if (meaningfulDown) userIntentDownRef.current = true;

      // 抑制 ResizeObserver 与滚动竞争产生的模糊事件，但保留明确向下或到达底部的手势。
      // @see https://github.com/WICG/resize-observer/issues/25
      window.setTimeout(() => {
        if (ignore != null && scrollTop === ignore) return;

        // 当前帧仍未脱离时，在布局完成后继续钳制到底部。
        if (isPinnedRef.current && !escapedRef.current) {
          const top = bottomScrollTop(el.scrollHeight, el.clientHeight);
          if (Math.abs(el.scrollTop - top) > 0.5) applyScrollTop(top);
          syncShowBack();
          return;
        }

        const near = isNearBottom(
          el.scrollTop,
          el.scrollHeight,
          el.clientHeight,
          thresholdRef.current,
        );
        const hard = isHardBottom(
          el.scrollTop,
          el.scrollHeight,
          el.clientHeight,
        );
        const intentDown = userIntentDownRef.current;
        const scrollingUp = meaningfulUp;
        const scrollingDown = meaningfulDown;

        if (
          resizeDifferenceRef.current !== 0 &&
          !scrollingDown &&
          !(hard && intentDown)
        ) {
          return;
        }

        const next = nextStickPinState(
          {
            pinned: isPinnedRef.current,
            escaped: escapedRef.current,
          },
          {
            scrollingUp,
            scrollingDown,
            nearBottom: near,
            hardBottom: hard && !scrollingUp,
            userIntentDown: intentDown && !scrollingUp,
          },
        );
        isPinnedRef.current = next.pinned;
        escapedRef.current = next.escaped;
        if (next.pinned) {
          userIntentDownRef.current = false;
          applyScrollTop(
            bottomScrollTop(el.scrollHeight, el.clientHeight),
          );
        }
        syncShowBack();
      }, 1);
    };

    const handleWheel = (e: WheelEvent) => {
      // 底部锁定时忽略触控板和弹性滚动产生的小幅滚轮变化。
      if (
        isPinnedRef.current &&
        !escapedRef.current &&
        Math.abs(e.deltaY) < STICK_ESCAPE_WHEEL_DELTA
      ) {
        return;
      }
      // deltaY < 0 表示浏览历史，仅在手势足够明确时脱离吸底。
      if (
        e.deltaY <= -STICK_ESCAPE_WHEEL_DELTA &&
        el.scrollHeight > el.clientHeight
      ) {
        userIntentDownRef.current = false;
        if (isPinnedRef.current) {
          escapedRef.current = true;
          isPinnedRef.current = false;
          syncShowBack();
        }
        return;
      }
      // deltaY > 0 表示接近最新内容；记录意图以支持最大 scrollTop 处重新吸底。
      if (e.deltaY >= STICK_ESCAPE_WHEEL_DELTA) {
        userIntentDownRef.current = true;
        if (escapedRef.current) {
          requestAnimationFrame(() => {
            if (!viewportRef.current) return;
            const v = viewportRef.current;
            if (
              isNearBottom(
                v.scrollTop,
                v.scrollHeight,
                v.clientHeight,
                thresholdRef.current,
              )
            ) {
              escapedRef.current = false;
              isPinnedRef.current = true;
              userIntentDownRef.current = false;
              applyScrollTop(
                bottomScrollTop(v.scrollHeight, v.clientHeight),
              );
              syncShowBack();
            }
          });
        }
      }
    };

    let touchY: number | null = null;
    const onTouchStart = (e: TouchEvent) => {
      touchY = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY;
      if (touchY == null || y == null) return;
      const dy = y - touchY;
      // 手指向下表示内容向上；只有明确拖动才解除吸底，过滤底部轻触。
      if (dy > STICK_ESCAPE_MIN_DELTA_PX) {
        userIntentDownRef.current = false;
        if (isPinnedRef.current) {
          escapedRef.current = true;
          isPinnedRef.current = false;
          syncShowBack();
        }
      } else if (dy < -STICK_ESCAPE_MIN_DELTA_PX) {
        // 手指向上表示内容向下接近最新消息。
        userIntentDownRef.current = true;
      }
      touchY = y;
    };
    const onTouchEnd = () => {
      touchY = null;
      // 向最新内容快速滑动结束后，若停在底部附近则重新吸底。
      if (userIntentDownRef.current && escapedRef.current) {
        requestAnimationFrame(() => {
          if (!viewportRef.current) return;
          const v = viewportRef.current;
          if (
            isNearBottom(
              v.scrollTop,
              v.scrollHeight,
              v.clientHeight,
              thresholdRef.current,
            )
          ) {
            escapedRef.current = false;
            isPinnedRef.current = true;
            userIntentDownRef.current = false;
            syncShowBack();
          }
        });
      }
    };

    el.addEventListener("scroll", handleScroll, { passive: true });
    el.addEventListener("wheel", handleWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("touchend", onTouchEnd, { passive: true });
    el.addEventListener("touchcancel", onTouchEnd, { passive: true });

    lastScrollTopRef.current = el.scrollTop;

    return () => {
      el.removeEventListener("scroll", handleScroll);
      el.removeEventListener("wheel", handleWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("touchend", onTouchEnd);
      el.removeEventListener("touchcancel", onTouchEnd);
    };
  }, [enabled, conversationKey, syncShowBack, applyScrollTop]);

  // 切换会话时重新吸底并跳到底部。
  useEffect(() => {
    if (!enabled) return;
    escapedRef.current = false;
    isPinnedRef.current = true;
    userIntentDownRef.current = false;
    const id = requestAnimationFrame(() => scrollToBottom("instant"));
    return () => cancelAnimationFrame(id);
  }, [conversationKey, enabled, scrollToBottom]);

  // 用户发送消息或回合开始运行时强制跟随；双 rAF 等待新消息行高完成首帧布局。
  useEffect(() => {
    if (!enabled || forceStickKey == null || forceStickKey === "") return;
    escapedRef.current = false;
    isPinnedRef.current = true;
    userIntentDownRef.current = false;
    let raf2 = 0;
    const raf1 = requestAnimationFrame(() => {
      scrollToBottom("instant");
      raf2 = requestAnimationFrame(() => scrollToBottom("instant"));
    });
    return () => {
      cancelAnimationFrame(raf1);
      if (raf2) cancelAnimationFrame(raf2);
    };
  }, [forceStickKey, enabled, scrollToBottom]);

  // 吸底期间持续处理内容增长和收缩。
  useEffect(() => {
    if (!enabled) return;
    const el = viewportRef.current;
    if (!el) return;

    let previousHeight: number | undefined;
    let raf = 0;

    const onHeightChange = (height: number) => {
      const difference = height - (previousHeight ?? height);
      // 小幅重排不进入完整尺寸修正，但吸底时仍需跟随，避免流式增量累积后掉队。
      if (previousHeight != null && isHeightDeltaNoise(difference)) {
        followIfPinned();
        previousHeight = height;
        return;
      }
      resizeDifferenceRef.current = difference;

      // 内容收缩后浏览器可能留下超出最大值的 scrollTop，此处以程序方式钳制。
      const maxTop = bottomScrollTop(el.scrollHeight, el.clientHeight);
      if (el.scrollTop > maxTop + 1) {
        applyScrollTop(maxTop);
      }

      if (
        difference < 0 &&
        !escapedRef.current &&
        isNearBottom(
          el.scrollTop,
          el.scrollHeight,
          el.clientHeight,
          thresholdRef.current,
        )
      ) {
        // 仍在跟随时发生 Markdown 收缩则保持吸底；用户已脱离时不重新启用。
        isPinnedRef.current = true;
      }

      // 内容或视口尺寸变化仅在吸底状态下跟随；脱离后不补偿全部高度差，
      // 防止底部的流式增长把正在阅读历史的用户拉下去。
      followIfPinned();

      previousHeight = height;
      requestAnimationFrame(() => {
        window.setTimeout(() => {
          if (resizeDifferenceRef.current === difference) {
            resizeDifferenceRef.current = 0;
          }
        }, 1);
      });
    };

    const measureContentHeight = () => {
      const content = contentRef.current ?? el.firstElementChild;
      if (content instanceof HTMLElement) return content.offsetHeight;
      return el.scrollHeight;
    };

    const ro = new ResizeObserver(() => {
      // 将多节点通知合并到一帧，并从 DOM 内容列读取真实内容高度。
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        onHeightChange(measureContentHeight());
      });
    });

    const content = contentRef.current ?? el.firstElementChild;
    if (content) ro.observe(content);
    // 窗口或工作台外框导致的视口尺寸变化也需要重新跟随。
    ro.observe(el);

    return () => {
      if (raf) cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, [enabled, conversationKey, applyScrollTop, followIfPinned]);

  return {
    viewportRef,
    contentRef,
    scrollToBottom,
    isPinnedRef,
    showBack,
  };
}
