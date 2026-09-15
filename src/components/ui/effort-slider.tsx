/**
 * 思考强度滑块：胶囊轨道、档位点、玻璃旋钮与 Canvas 四态动效。
 *
 * 行为层复用 shadcn/Radix Slider（键盘步进、指针拖动、role="slider" 语义都在
 * 它那边）；视觉层按 AGENTS.md 在 `styles/effort-slider.css` 自绘——shadcn 的
 * 1.5px 细轨样式无法承载 Droppy Code 式的 34px 胶囊轨道与溢出旋钮。
 *
 * 几何：inset 取旋钮半径（21px），Radix 对 thumb 的边界偏移公式
 * （`calc(p% + thumbInBoundsOffset)`）与 droppy 的 `inset + p*(W-2*inset)` 等
 * 价，因此档位点、填充与 thumb 三者共用同一公式。填充延伸到旋钮外缘
 * （`x + inset`），不使用 Radix 的 Range 元素（Range 止于 thumb 中心）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as SliderPrimitive from "@radix-ui/react-slider";

import {
  drawEffortTrackFrame,
  readEffortPaletteFromElement,
  resolveEffortTrackKind,
  trackFrameIntervalMs,
} from "@/lib/effortTrack";
import { cn } from "@/lib/utils";

export interface EffortSliderProps {
  /** 档位数量；小于 2 时滑块禁用（上游对单档模型不渲染滑块）。 */
  count: number;
  /** 当前档位下标，受控。 */
  index: number;
  onIndexChange: (index: number) => void;
  /** 快速模式：来自 Ultra 开关，决定 fast/fusion 两态。 */
  fast: boolean;
  /** 无障碍名称，取当前档位的展示名。 */
  label: string;
  /** 落在 thumb 上，供面板内 Label 的 htmlFor 关联。 */
  id?: string;
}

/** 与 `--effort-slider-knob` 保持一致的旋钮直径；inset 即其半径。 */
const KNOB_SIZE = 36;
const INSET = KNOB_SIZE / 2;

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
  const effectRef = useRef<HTMLCanvasElement>(null);
  const [dragging, setDragging] = useState(false);
  const [settling, setSettling] = useState(false);

  const isMax = count > 1 && index === count - 1;
  const kind = resolveEffortTrackKind(isMax, fast);
  const disabled = count < 2;
  const step = count > 1 ? 1 / (count - 1) : 0;

  /** 档位下标对应的 thumb 中心 x（相对根元素）。 */
  const stopX = useCallback(
    (at: number, width: number) => INSET + at * step * (width - INSET * 2),
    [step],
  );

  /** 填充延伸到旋钮外缘：宽度 = x + inset（上游 `x + inset`）。 */
  const layoutFill = useCallback(
    (x: number) => {
      const fill = fillRef.current;
      if (!fill) return;
      fill.style.width = `${x + INSET}px`;
    },
    [],
  );

  // 档位或尺寸变化时重排填充与档位点；ResizeObserver 覆盖面板开合的宽度变化。
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const stops = root.querySelectorAll<HTMLElement>(".effort-slider__stop");
    const layout = () => {
      const width = root.getBoundingClientRect().width;
      for (const stop of stops) {
        stop.style.left = `${stopX(Number(stop.dataset.stop ?? "0"), width)}px`;
      }
      layoutFill(stopX(index, width));
    };
    layout();
    const observer = new ResizeObserver(layout);
    observer.observe(root);
    return () => observer.disconnect();
  }, [count, index, stopX, layoutFill]);

  // 档位切换时的下沉动效（触觉近似），240ms 后移除类名。
  useEffect(() => {
    setSettling(true);
    const timer = window.setTimeout(() => setSettling(false), 240);
    return () => window.clearTimeout(timer);
  }, [index]);

  // 画布动效：plain 不挂载（渲染端短路），其余按外观节流；reduced-motion 停画。
  useEffect(() => {
    if (kind === "plain") return;
    const canvas = effectRef.current;
    const root = rootRef.current;
    if (!canvas || !root) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduceMotion =
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduceMotion) return;

    const palette = readEffortPaletteFromElement(root);
    const startedAt = performance.now();
    let raf = 0;
    let last = 0;
    const interval = trackFrameIntervalMs(kind);

    const paint = (now: number) => {
      raf = requestAnimationFrame(paint);
      if (now - last < interval) return;
      last = now;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      const scale = window.devicePixelRatio || 1;
      const pixelWidth = Math.max(1, Math.round(width * scale));
      const pixelHeight = Math.max(1, Math.round(height * scale));
      if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
        canvas.width = pixelWidth;
        canvas.height = pixelHeight;
      }
      ctx.setTransform(scale, 0, 0, scale, 0, 0);
      ctx.clearRect(0, 0, width, height);
      drawEffortTrackFrame(ctx, {
        kind,
        width,
        height,
        time: (now - startedAt) / 1000,
        palette,
      });
    };
    raf = requestAnimationFrame(paint);
    return () => cancelAnimationFrame(raf);
  }, [kind]);

  // 拖动进行中让填充实时跟随指针：Radix 只在指针移动时提交值，视觉上直接跟手。
  useEffect(() => {
    if (!dragging) return;
    const root = rootRef.current;
    if (!root) return;
    const onMove = (event: PointerEvent) => {
      layoutFill(event.clientX - root.getBoundingClientRect().left);
    };
    const onUp = () => setDragging(false);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [dragging, layoutFill]);

  const stopIndexes = useMemo(
    () => Array.from({ length: count }, (_, at) => at),
    [count],
  );

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
      <SliderPrimitive.Root
        className="effort-slider__root"
        value={[index]}
        onValueChange={([next]) => {
          if (typeof next === "number" && next !== index) onIndexChange(next);
        }}
        min={0}
        max={Math.max(0, count - 1)}
        step={1}
        disabled={disabled}
      >
        <SliderPrimitive.Track className="effort-slider__track">
          <div className="effort-slider__clip" aria-hidden>
            <div ref={fillRef} className="effort-slider__fill">
              {kind !== "plain" ? (
                <canvas ref={effectRef} className="effort-slider__effect" aria-hidden />
              ) : null}
            </div>
            {stopIndexes.map((at) => (
              <span
                key={at}
                data-stop={at}
                className={cn(
                  "effort-slider__stop",
                  at <= index && "effort-slider__stop--passed",
                )}
                aria-hidden
              />
            ))}
          </div>
        </SliderPrimitive.Track>
        <SliderPrimitive.Thumb
          id={id}
          className={cn("effort-slider__thumb", settling && "is-settling")}
          aria-label={label}
          onPointerDown={() => setDragging(true)}
        />
      </SliderPrimitive.Root>
    </span>
  );
}
