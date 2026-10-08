import {
  drawEffortTrackFrame,
  readEffortPaletteFromElement,
  trackFrameIntervalMs,
  type EffortTrackKind,
} from "@/modules/ai/lib/effortTrack";
import type { EffortColor } from "@/modules/settings/effortColor";
import { useEffect, useRef } from "react";

export function useEffortTrack(
  kind: EffortTrackKind,
  active: boolean,
  color: EffortColor,
) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: The color attribute changes the inherited CSS palette read by this effect.
  useEffect(() => {
    if (!active || kind === "plain") return;
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;

    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    const palette = readEffortPaletteFromElement(canvas);
    const startedAt = performance.now();
    const interval = trackFrameIntervalMs(kind);
    let width = canvas.clientWidth;
    let height = canvas.clientHeight;
    let frame = 0;
    let lastPaint = 0;

    const draw = (now: number) => {
      const scale = Math.min(2, window.devicePixelRatio || 1);
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
        time: motion.matches ? 0.1 : (now - startedAt) / 1000,
        palette,
      });
    };

    const paint = (now: number) => {
      frame = 0;
      if (document.hidden || width <= 0 || height <= 0) return;
      if (now - lastPaint >= interval) {
        lastPaint = now;
        draw(now);
      }
      frame = requestAnimationFrame(paint);
    };

    const sync = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      lastPaint = 0;
      if (document.hidden || width <= 0 || height <= 0) return;
      if (motion.matches) draw(startedAt);
      else frame = requestAnimationFrame(paint);
    };

    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      width = entry.contentRect.width;
      height = entry.contentRect.height;
      sync();
    });
    observer.observe(canvas);
    document.addEventListener("visibilitychange", sync);
    window.addEventListener("resize", sync);
    motion.addEventListener("change", sync);
    sync();

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      document.removeEventListener("visibilitychange", sync);
      window.removeEventListener("resize", sync);
      motion.removeEventListener("change", sync);
      canvas.width = 1;
      canvas.height = 1;
    };
  }, [kind, active, color]);

  return canvasRef;
}
