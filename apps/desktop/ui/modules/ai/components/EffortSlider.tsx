import { cn } from "@/lib/utils";
import {
  effortPointerPosition,
  effortStopPosition,
} from "@/modules/ai/lib/effortSlider";
import { resolveFixedEffortTrackKind } from "@/modules/ai/lib/effortTrack";
import { useEffortTrack } from "@/modules/ai/lib/useEffortTrack";
import type { EffortColor } from "@/modules/settings/effortColor";
import { Slider } from "radix-ui";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { PointerEvent } from "react";
import "@/modules/ai/components/effort-slider.css";

export interface EffortSliderProps {
  color: EffortColor;
  count: number;
  index: number;
  onIndexChange: (index: number) => void;
  label: string;
  valueText: string;
  disabled?: boolean;
  active?: boolean;
}

export default function EffortSlider({
  color,
  count,
  index,
  onIndexChange,
  label,
  valueText,
  disabled = false,
  active = true,
}: EffortSliderProps) {
  const rootRef = useRef<HTMLSpanElement>(null);
  const fillRef = useRef<HTMLSpanElement>(null);
  const thumbRef = useRef<HTMLSpanElement>(null);
  const pointerRef = useRef<number | null>(null);
  const [dragging, setDragging] = useState(false);
  const [pointerInput, setPointerInput] = useState(false);
  const kind =
    index < 0
      ? "plain"
      : resolveFixedEffortTrackKind(count > 1 && index === count - 1);
  const canvasRef = useEffortTrack(kind, active && !disabled, color);
  const inert = disabled || count < 1;
  const stops = Array.from({ length: count }, (_, stop) => stop);

  const positionVisual = useCallback((x: number) => {
    if (fillRef.current) fillRef.current.style.width = `${x + 18}px`;
    if (thumbRef.current) thumbRef.current.style.left = `${x}px`;
  }, []);

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const layout = () => {
      const width = root.offsetWidth;
      root
        .querySelectorAll<HTMLElement>("[data-effort-stop]")
        .forEach((stop) => {
          stop.style.left = `${effortStopPosition(Number(stop.dataset.effortStop), count, width)}px`;
        });
      if (!dragging) positionVisual(effortStopPosition(index, count, width));
    };
    layout();
    const observer = new ResizeObserver(layout);
    observer.observe(root);
    return () => observer.disconnect();
  }, [count, index, dragging, positionVisual]);

  useEffect(() => {
    if (!active) return;
    const frame = requestAnimationFrame(() => {
      const root = rootRef.current;
      const panel = root?.closest("[data-slot=popover-content]");
      if (panel && document.activeElement === panel)
        root
          ?.querySelector<HTMLElement>("[role=slider]")
          ?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [active]);

  const trackPointer = (event: PointerEvent<HTMLSpanElement>) => {
    const root = rootRef.current;
    if (!root) return;
    const bounds = root.getBoundingClientRect();
    positionVisual(
      effortPointerPosition(
        event.clientX,
        bounds.left,
        bounds.width,
        root.offsetWidth,
      ),
    );
  };

  const finishDrag = (event: PointerEvent<HTMLSpanElement>) => {
    if (pointerRef.current !== event.pointerId) return;
    pointerRef.current = null;
    setDragging(false);
  };

  if (count < 1) return null;

  return (
    <span
      ref={rootRef}
      className={cn(
        "reasoning-effort-slider",
        `reasoning-effort-slider--${kind}`,
      )}
      data-dragging={dragging || undefined}
      data-unselected={index < 0 || undefined}
      data-disabled={inert || undefined}
      data-pointer={pointerInput || undefined}
    >
      <span className="reasoning-effort-slider__track" aria-hidden="true">
        <span className="reasoning-effort-slider__clip">
          <span ref={fillRef} className="reasoning-effort-slider__fill">
            {kind !== "plain" && (
              <canvas
                ref={canvasRef}
                className="reasoning-effort-slider__effect"
              />
            )}
          </span>
          {stops.map((at) => (
            <span
              key={at}
              data-effort-stop={at}
              className="reasoning-effort-slider__stop"
              data-passed={at <= index || undefined}
            />
          ))}
        </span>
      </span>
      <span
        ref={thumbRef}
        className="reasoning-effort-slider__thumb"
        aria-hidden="true"
      />
      <Slider.Root
        className="reasoning-effort-slider__input"
        value={[Math.max(0, index)]}
        min={0}
        max={Math.max(1, count - 1)}
        step={1}
        disabled={inert}
        onValueChange={([next]) => {
          if (count === 1) onIndexChange(0);
          else if (next !== undefined && next !== index && next < count)
            onIndexChange(next);
        }}
        onPointerDownCapture={(event) => {
          if (inert || event.button !== 0 || !event.isPrimary) return;
          pointerRef.current = event.pointerId;
          setPointerInput(true);
          setDragging(true);
          trackPointer(event);
        }}
        onPointerMove={(event) => {
          if (event.pointerId === pointerRef.current) trackPointer(event);
        }}
        onPointerUpCapture={(event) => {
          if (pointerRef.current !== event.pointerId) return;
          if (index < 0) onIndexChange(0);
          finishDrag(event);
        }}
        onPointerCancelCapture={finishDrag}
        onLostPointerCapture={finishDrag}
        onKeyDownCapture={(event) => {
          setPointerInput(false);
          if (inert || index >= 0) return;
          if (
            ![
              "Home",
              "End",
              "ArrowLeft",
              "ArrowRight",
              "ArrowUp",
              "ArrowDown",
              "PageUp",
              "PageDown",
              "Enter",
              " ",
            ].includes(event.key)
          )
            return;
          event.preventDefault();
          onIndexChange(event.key === "End" ? count - 1 : 0);
        }}
      >
        <Slider.Track className="reasoning-effort-slider__input-track" />
        <Slider.Thumb
          className="reasoning-effort-slider__input-thumb"
          aria-label={label}
          aria-valuetext={valueText}
        />
      </Slider.Root>
    </span>
  );
}
