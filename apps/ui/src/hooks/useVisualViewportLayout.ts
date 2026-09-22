import { useEffect } from "react";

/**
 * 把移动浏览器的 visual viewport 高度暴露给壳层。
 * 软键盘打开时布局视口可能仍保持原高度，直接使用 100% 会让底部
 * Composer 被键盘盖住；仅同步 CSS 变量，不触碰会话或输入状态。
 */
export function useVisualViewportLayout(): void {
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;

    const root = document.documentElement;
    let frame = 0;
    const sync = () => {
      frame = 0;
      const height = Math.round(viewport.height);
      if (height > 0) {
        root.style.setProperty("--visual-viewport-height", `${height}px`);
      }
    };
    const schedule = () => {
      if (frame) return;
      frame = window.requestAnimationFrame(sync);
    };

    sync();
    viewport.addEventListener("resize", schedule);
    viewport.addEventListener("scroll", schedule);
    window.addEventListener("resize", schedule);

    return () => {
      viewport.removeEventListener("resize", schedule);
      viewport.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
      if (frame) window.cancelAnimationFrame(frame);
      root.style.removeProperty("--visual-viewport-height");
    };
  }, []);
}
