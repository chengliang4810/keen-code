import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Slider } from "@appica/ui-react/slider";

import {
  drawEffortTrackFrame,
  readEffortPaletteFromElement,
  resolveEffortTrackKind,
  trackFrameIntervalMs,
} from "@/lib/effortTrack";
import { cn } from "@/lib/utils";

export interface EffortSliderProps {
  count: number;
  index: number;
  onIndexChange: (index: number) => void;
  fast: boolean;
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

/** Appica Slider 负责行为和无障碍；覆盖层恢复 KeenCode 的四态专用视觉。 */
export function EffortSlider({
  count,
  index,
  onIndexChange,
  fast,
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
  const kind = resolveEffortTrackKind(isMax, fast);
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
  }, [count, index, layoutVisual, stopX]);

  useEffect(() => {
    setSettling(true);
    const timer = window.setTimeout(() => setSettling(false), 240);
    return () => window.clearTimeout(timer);
  }, [index]);

  useEffect(() => {
    if (kind === "plain") return;
    const canvas = effectRef.current;
    const root = rootRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !root || !context) return;
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;

    const palette = readEffortPaletteFromElement(root);
    const startedAt = performance.now();
    const interval = trackFrameIntervalMs(kind);
    let frame = 0;
    let lastPaint = 0;
    const paint = (now: number) => {
      frame = requestAnimationFrame(paint);
      if (now - lastPaint < interval) return;
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
        time: (now - startedAt) / 1000,
        palette,
      });
    };
    frame = requestAnimationFrame(paint);
    return () => cancelAnimationFrame(frame);
  }, [kind]);

  useEffect(() => {
    if (!dragging) return;
    const root = rootRef.current;
    if (!root) return;
    const move = (event: PointerEvent) => {
      const bounds = root.getBoundingClientRect();
      layoutVisual(Math.min(bounds.width - INSET, Math.max(INSET, event.clientX - bounds.left)));
    };
    const stop = () => setDragging(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop, { once: true });
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
  }, [dragging, layoutVisual]);

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
