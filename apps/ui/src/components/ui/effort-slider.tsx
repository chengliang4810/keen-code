import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Slider } from "@appica/ui-react/slider";

import {
  drawEffortTrackFrame,
  readEffortPaletteFromElement,
  resolveFixedEffortTrackKind,
  trackFrameIntervalMs,
} from "@/lib/effortTrack";
import { cn } from "@/lib/utils";

export interface EffortSliderProps {
  count: number;
  index: number;
  onIndexChange: (index: number) => void;
  label: string;
  id?: string;
}

const KNOB_SIZE = 36;
const INSET = KNOB_SIZE / 2;
const SLIDER_MIN = 0;
const SLIDER_MAX = 100;

/** 将模型支持的离散推理档位均匀映射到 0-100。 */
export function effortSliderStep(count: number): number {
  return count > 1 ? (SLIDER_MAX - SLIDER_MIN) / (count - 1) : SLIDER_MAX;
}

/** 将 Slider 的 0-100 值吸附回模型支持的推理档位索引。 */
export function effortIndexFromSliderValue(value: number, count: number): number {
  if (count < 2) return 0;
  return Math.min(count - 1, Math.max(0, Math.round(value / effortSliderStep(count))));
}

/** Appica Slider 负责行为和无障碍；覆盖层按档位呈现固定的快速视觉效果。 */
export function EffortSlider({
  count,
  index,
  onIndexChange,
  label,
  id,
}: EffortSliderProps) {
  const rootRef = useRef<HTMLSpanElement>(null);
  const fillRef = useRef<HTMLDivElement>(null);
  const thumbRef = useRef<HTMLSpanElement>(null);
  const effectRef = useRef<HTMLCanvasElement>(null);
  const [dragging, setDragging] = useState(false);
  const [settling, setSettling] = useState(false);

  const isMax = count > 1 && index === count - 1;
  const kind = resolveFixedEffortTrackKind(isMax);
  const disabled = count < 2;
  const visualStep = count > 1 ? 1 / (count - 1) : 0;
  const sliderStep = effortSliderStep(count);
  const sliderValue = count > 1 ? index * sliderStep : SLIDER_MIN;
  const stopIndexes = useMemo(
    () => Array.from({ length: count }, (_, at) => at),
    [count],
  );
  const stopX = useCallback(
    (at: number, width: number) => INSET + at * visualStep * (width - INSET * 2),
    [visualStep],
  );
  const layoutVisual = useCallback((x: number) => {
    if (fillRef.current) fillRef.current.style.width = `${x + INSET}px`;
    if (thumbRef.current) thumbRef.current.style.left = `${x}px`;
  }, []);

  useEffect(() => {
    // 拖拽中由 pointermove 直接驱动位置，不与指针争抢；松手后回到这里按档位吸附。
    if (dragging) return;
    const root = rootRef.current;
    if (!root) return;
    const layout = () => {
      const width = root.getBoundingClientRect().width;
      root.querySelectorAll<HTMLElement>(".effort-slider__stop").forEach((stop) => {
        stop.style.left = `${stopX(Number(stop.dataset.stop ?? "0"), width)}px`;
      });
      layoutVisual(stopX(index, width));
    };
    layout();
    const observer = new ResizeObserver(layout);
    observer.observe(root);
    return () => observer.disconnect();
  }, [count, index, dragging, layoutVisual, stopX]);

  useEffect(() => {
    // 松手（或键盘换档）后播放回落动画；拖拽期间旋钮保持放大，不定格。
    if (dragging) {
      setSettling(false);
      return;
    }
    setSettling(true);
    const timer = window.setTimeout(() => setSettling(false), 240);
    return () => window.clearTimeout(timer);
  }, [index, dragging]);

  useEffect(() => {
    if (kind === "plain") return;
    const canvas = effectRef.current;
    const root = rootRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !root || !context) return;
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

    let palette = readEffortPaletteFromElement(root);
    const startedAt = performance.now();
    const interval = trackFrameIntervalMs(kind);
    let frame = 0;
    let lastPaint = 0;
    const paint = (now: number) => {
      frame = 0;
      if (document.hidden) return;
      if (!reducedMotion) frame = requestAnimationFrame(paint);
      if (!reducedMotion && now - lastPaint < interval) return;
      lastPaint = now;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      const scale = window.devicePixelRatio || 1;
      const pixelWidth = Math.max(1, Math.round(width * scale));
      const pixelHeight = Math.max(1, Math.round(height * scale));
      if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
        canvas.width = pixelWidth;
        canvas.height = pixelHeight;
      }
      context.setTransform(scale, 0, 0, scale, 0, 0);
      context.clearRect(0, 0, width, height);
      drawEffortTrackFrame(context, {
        kind,
        width,
        height,
        time: reducedMotion ? 0.1 : (now - startedAt) / 1000,
        palette,
      });
    };
    const syncVisibility = () => {
      if (document.hidden) {
        cancelAnimationFrame(frame);
        frame = 0;
      } else if (!frame) {
        if (reducedMotion) paint(startedAt);
        else frame = requestAnimationFrame(paint);
      }
    };
    document.addEventListener("visibilitychange", syncVisibility);
    syncVisibility();
    // 设置页切换思考强度色预设时令牌变化，画布需要重读调色板并立即重绘。
    const paletteObserver = new MutationObserver(() => {
      palette = readEffortPaletteFromElement(root);
      syncVisibility();
    });
    paletteObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-effort-color"],
    });
    const observer = reducedMotion ? new ResizeObserver(syncVisibility) : null;
    if (observer) observer.observe(root);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("visibilitychange", syncVisibility);
      paletteObserver.disconnect();
      observer?.disconnect();
    };
  }, [kind]);

  useEffect(() => {
    if (!dragging) return;
    const root = rootRef.current;
    if (!root) return;
    let lastNearest = -1;
    const move = (event: PointerEvent) => {
      const bounds = root.getBoundingClientRect();
      const x = Math.min(bounds.width - INSET, Math.max(INSET, event.clientX - bounds.left));
      layoutVisual(x);
      // 拖拽中档位跟随最近停靠点（对照上游 DragGesture 的 nearestStop），
      // 松手吸附与标题都以这里的最新档位为准。
      if (count > 1) {
        const step = (bounds.width - INSET * 2) / (count - 1);
        const nearest = Math.min(count - 1, Math.max(0, Math.round((x - INSET) / step)));
        if (nearest !== lastNearest) {
          lastNearest = nearest;
          onIndexChange(nearest);
        }
      }
    };
    const stop = () => setDragging(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop, { once: true });
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
  }, [dragging, count, layoutVisual, onIndexChange]);

  if (count < 1) return null;

  return (
    <span
      ref={rootRef}
      className={cn(
        "effort-slider",
        `effort-slider--${kind}`,
        dragging && "effort-slider--dragging",
      )}
    >
      <span className="effort-slider__track" aria-hidden>
        <span className="effort-slider__clip">
          {/* 画布只覆盖填充，动效不越过旋钮（对照上游 TrackEffect 挂在填充胶囊上）。 */}
          <span ref={fillRef} className="effort-slider__fill">
            {kind === "plain" ? null : (
              <canvas ref={effectRef} className="effort-slider__effect" />
            )}
          </span>
          {stopIndexes.map((at) => (
            <span
              key={at}
              data-stop={at}
              className={cn(
                "effort-slider__stop",
                at <= index && "effort-slider__stop--passed",
              )}
            />
          ))}
        </span>
      </span>
      <span
        ref={thumbRef}
        className={cn("effort-slider__thumb", settling && "is-settling")}
        aria-hidden
      />
      <Slider
        id={id}
        className="effort-slider__input"
        value={sliderValue}
        onPointerDown={() => setDragging(true)}
        onValueChange={(next) => {
          if (typeof next !== "number") return;
          const nextIndex = effortIndexFromSliderValue(next, count);
          if (nextIndex !== index) onIndexChange(nextIndex);
        }}
        min={SLIDER_MIN}
        max={SLIDER_MAX}
        step={sliderStep}
        disabled={disabled}
        thumbAriaLabel={label}
        tooltipVisibility="never"
      />
    </span>
  );
}
