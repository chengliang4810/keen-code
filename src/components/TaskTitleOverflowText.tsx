import {
  useEffect,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type ElementType,
} from "react";

import { cn } from "@/lib/utils";

const marqueeGapPx = 24;
const marqueePixelsPerSecond = 40;
const minimumMarqueeDurationSeconds = 6;
const marqueePauseSeconds = 2;
const marqueeHoverDelayMs = 1_000;
const marqueeMaskFadeInSeconds = 0.15;
const rightFadeMask =
  "linear-gradient(to right, black 0, black calc(100% - 1.5rem), transparent 100%)";
const bothEdgesFadeMask =
  "linear-gradient(to right, transparent 0, black 1.5rem, black calc(100% - 1.5rem), transparent 100%)";

function createMaskKeyframe(maskImage: string, offset: number): Keyframe {
  return { maskImage, offset, webkitMaskImage: maskImage };
}

type TaskTitleOverflowTextProps = ComponentPropsWithoutRef<"span"> & {
  as?: "p" | "span";
};

/** 长会话标题先渐隐，持续悬停一秒后以固定速度循环展示完整内容。 */
export function TaskTitleOverflowText({
  as = "span",
  children,
  className,
  title: _nativeTitle,
  ...props
}: TaskTitleOverflowTextProps) {
  const Component = as as ElementType;
  const textRef = useRef<HTMLElement | null>(null);
  const originalTextRef = useRef<HTMLSpanElement | null>(null);
  const marqueeTrackRef = useRef<HTMLSpanElement | null>(null);
  const [overflowState, setOverflowState] = useState({
    distance: 0,
    duration: minimumMarqueeDurationSeconds,
    isOverflowing: false,
  });
  const { distance, duration, isOverflowing } = overflowState;

  useEffect(() => {
    const textElement = textRef.current;
    const originalTextElement = originalTextRef.current;
    if (!textElement || !originalTextElement) return;

    const updateOverflow = () => {
      const contentWidth = originalTextElement.scrollWidth;
      const nextIsOverflowing = contentWidth > textElement.clientWidth;
      const nextDistance = nextIsOverflowing ? contentWidth + marqueeGapPx : 0;
      const nextDuration = nextIsOverflowing
        ? Math.max(minimumMarqueeDurationSeconds, nextDistance / marqueePixelsPerSecond)
        : minimumMarqueeDurationSeconds;
      setOverflowState((current) =>
        current.isOverflowing === nextIsOverflowing &&
        current.distance === nextDistance &&
        current.duration === nextDuration
          ? current
          : {
              distance: nextDistance,
              duration: nextDuration,
              isOverflowing: nextIsOverflowing,
            },
      );
    };

    updateOverflow();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateOverflow);
      return () => window.removeEventListener("resize", updateOverflow);
    }

    const resizeObserver = new ResizeObserver(updateOverflow);
    resizeObserver.observe(textElement);
    resizeObserver.observe(originalTextElement);
    return () => resizeObserver.disconnect();
  }, [children]);

  useEffect(() => {
    const textElement = textRef.current;
    const marqueeTrack = marqueeTrackRef.current;
    if (!isOverflowing || !textElement || !marqueeTrack || typeof marqueeTrack.animate !== "function") {
      return;
    }

    const reducedMotionQuery = window.matchMedia?.("(prefers-reduced-motion: reduce)") ?? null;
    const hoverTarget = textElement.closest<HTMLElement>(".tree-l3") ?? textElement;
    let animations: Animation[] = [];
    let startTimer: number | null = null;

    const stopMarquee = () => {
      if (startTimer !== null) {
        window.clearTimeout(startTimer);
        startTimer = null;
      }
      animations.forEach((animation) => animation.cancel());
      animations = [];
    };
    const startMarquee = () => {
      if (reducedMotionQuery?.matches) return;
      stopMarquee();
      const totalDuration = duration + marqueePauseSeconds;
      const movementEndOffset = duration / totalDuration;
      const maskFadeInEndOffset = marqueeMaskFadeInSeconds / totalDuration;
      const originalTailArrivalOffset =
        (duration * ((distance - marqueeGapPx) / distance)) / totalDuration;
      const animationOptions: KeyframeAnimationOptions = {
        duration: totalDuration * 1_000,
        easing: "linear",
        iterations: Infinity,
      };
      const trackAnimation = marqueeTrack.animate(
        [
          { offset: 0, transform: "translate3d(0, 0, 0)" },
          { offset: movementEndOffset, transform: `translate3d(-${distance}px, 0, 0)` },
          { offset: 1, transform: `translate3d(-${distance}px, 0, 0)` },
        ],
        animationOptions,
      );
      const maskAnimation = textElement.animate(
        [
          createMaskKeyframe(rightFadeMask, 0),
          createMaskKeyframe(bothEdgesFadeMask, maskFadeInEndOffset),
          createMaskKeyframe(bothEdgesFadeMask, originalTailArrivalOffset),
          createMaskKeyframe(rightFadeMask, originalTailArrivalOffset),
          createMaskKeyframe(rightFadeMask, 1),
        ],
        animationOptions,
      );
      animations = [trackAnimation, maskAnimation];
    };
    const scheduleMarquee = () => {
      if (reducedMotionQuery?.matches) return;
      stopMarquee();
      startTimer = window.setTimeout(() => {
        startTimer = null;
        startMarquee();
      }, marqueeHoverDelayMs);
    };
    const handleMotionPreferenceChange = (event: MediaQueryListEvent) => {
      if (event.matches) stopMarquee();
    };

    hoverTarget.addEventListener("mouseenter", scheduleMarquee);
    hoverTarget.addEventListener("mouseleave", stopMarquee);
    reducedMotionQuery?.addEventListener("change", handleMotionPreferenceChange);
    return () => {
      hoverTarget.removeEventListener("mouseenter", scheduleMarquee);
      hoverTarget.removeEventListener("mouseleave", stopMarquee);
      reducedMotionQuery?.removeEventListener("change", handleMotionPreferenceChange);
      stopMarquee();
    };
  }, [distance, duration, isOverflowing]);

  return (
    <Component
      ref={textRef}
      className={cn(
        "task-title-overflow-text",
        isOverflowing && "task-title-overflow-text--overflowing task-title-marquee",
        className,
      )}
      {...props}
    >
      <span ref={marqueeTrackRef} className="task-title-marquee-track">
        <span ref={originalTextRef} data-task-title-copy="original">
          {children}
        </span>
        {isOverflowing ? (
          <span aria-hidden="true" data-task-title-copy="duplicate">
            {children}
          </span>
        ) : null}
      </span>
    </Component>
  );
}
