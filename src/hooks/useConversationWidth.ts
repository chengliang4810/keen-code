import { useLayoutEffect, useRef } from "react";

/**
 * 按 Harness ConversationRoot 的实际列宽计算正文宽度：clamp(680px, 64%, 920px)。
 * 使用 ResizeObserver 而非 CSS size container，避免改变后代 fixed 浮层的定位基准。
 * 仅尺寸变化时写入 CSS 变量；不触发 React 重渲染，卸载时释放观察器。
 */
export function useConversationWidth() {
  const ref = useRef<HTMLElement>(null);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const update = (width: number) => {
      element.style.setProperty("--conversation-column-width", `${width}px`);
    };
    update(element.getBoundingClientRect().width);
    const observer = new ResizeObserver(([entry]) => {
      if (entry) update(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return ref;
}
