import { useEffect, useState, type RefObject } from "react";

/**
 * 检测主界面浮层是否真的盖住原生子 WebView。
 *
 * 子 WebView 由系统原生层绘制，永远在主 WebView 之上，任何 DOM 浮层都无法盖住它，
 * 因此被遮挡时必须让位。
 *
 * 只按几何遮挡判断，不看"是否存在浮层"：左侧输入框的斜杠菜单虽然也是浮层，但与右侧
 * 面板不相交，不该让网页闪白。
 */

/** 采样点相对宿主矩形的比例位置；中心加四角，覆盖部分遮挡。 */
const SAMPLE_POINTS: ReadonlyArray<readonly [number, number]> = [
  [0.5, 0.5],
  [0.1, 0.1],
  [0.9, 0.1],
  [0.1, 0.9],
  [0.9, 0.9],
];

/**
 * 宿主矩形是否被其它元素遮挡。
 *
 * `elementFromPoint` 按真实层叠顺序命中，原生子 WebView 不参与 DOM 命中测试，
 * 所以只要采样点命中的不是宿主自身，就说明有 DOM 元素压在浏览器之上。
 */
function isOccluded(host: HTMLElement): boolean {
  const rect = host.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return false;
  return SAMPLE_POINTS.some(([fx, fy]) => {
    const x = rect.left + rect.width * fx;
    const y = rect.top + rect.height * fy;
    if (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight) {
      return false;
    }
    const hit = document.elementFromPoint(x, y);
    return hit !== null && !host.contains(hit);
  });
}

/**
 * 原生子 WebView 是否被主界面浮层遮挡。
 *
 * `enabled` 为 false 时不做检测，避免隐藏状态下持续观察 DOM。
 */
export function useCoveringOverlay(
  hostRef: RefObject<HTMLElement | null>,
  enabled: boolean,
): boolean {
  const [occluded, setOccluded] = useState(false);

  useEffect(() => {
    if (!enabled || typeof document === "undefined") {
      setOccluded(false);
      return;
    }
    const check = () => {
      const host = hostRef.current;
      setOccluded(host ? isOccluded(host) : false);
    };
    check();
    // 对话流式渲染会持续改动 DOM，用帧合并避免每个变更都做一次命中测试。
    let frame = 0;
    const schedule = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        check();
      });
    };
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("resize", schedule);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener("resize", schedule);
    };
  }, [enabled, hostRef]);

  return occluded;
}
