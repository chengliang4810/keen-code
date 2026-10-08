import { useEffect, useRef, useState, type RefObject } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { createProjectDirectoryDropTarget } from "@/modules/ai/lib/projectDirectory";

/** 仅表单存在时监听系统目录拖拽，异步注册晚于卸载时也立即释放监听。 */
export function useProjectDirectoryDrop(
  target: RefObject<HTMLElement | null>,
  disabled: boolean,
  onDrop: (paths: string[]) => void,
) {
  const [hovering, setHovering] = useState(false);
  const latest = useRef({ disabled, onDrop });
  latest.current = { disabled, onDrop };
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const handle = createProjectDirectoryDropTarget({
      contains: (x, y) => {
        const rect = target.current?.getBoundingClientRect();
        return (
          !latest.current.disabled &&
          !!rect &&
          x >= rect.left &&
          x < rect.right &&
          y >= rect.top &&
          y < rect.bottom
        );
      },
      pixelRatio: () => window.devicePixelRatio,
      onHover: (active) => {
        if (!disposed) setHovering(active);
      },
      onDrop: (paths) => {
        if (!disposed) latest.current.onDrop(paths);
      },
    });
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (!disposed) handle(event.payload);
      })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((error) =>
        console.error("[rcode] project directory drop listen failed:", error),
      );
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [target]);
  return hovering && !disabled;
}
