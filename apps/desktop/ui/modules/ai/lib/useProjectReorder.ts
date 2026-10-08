import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { createProjectDragGesture } from "@/modules/ai/lib/projectDragGesture";
import type {
  ProjectDropTarget,
  ProjectDropEdge,
} from "@/modules/ai/lib/sidebarNavigation";

type Drop = { target: ProjectDropTarget; edge: ProjectDropEdge };
type Options = {
  disabled: boolean;
  onMove: (
    source: ProjectDropTarget,
    target: ProjectDropTarget,
    edge: ProjectDropEdge,
  ) => void;
};

export function useProjectReorder(options: Options) {
  const rootRef = useRef<HTMLDivElement>(null);
  const ghostElement = useRef<HTMLDivElement | null>(null);
  const position = useRef({ x: 0, y: 0 });
  const cleanup = useRef<(() => void) | null>(null);
  const suppressClick = useRef<HTMLButtonElement | null>(null);
  const opts = useRef(options);
  const [dragging, setDragging] = useState<{
    source: ProjectDropTarget;
    label: string;
  } | null>(null);
  const [drop, setDrop] = useState<Drop | null>(null);
  useLayoutEffect(() => {
    opts.current = options;
  });
  useEffect(() => {
    if (options.disabled) cleanup.current?.();
  }, [options.disabled]);
  useEffect(
    () => () => {
      cleanup.current?.();
    },
    [],
  );

  const placeGhost = (point: { x: number; y: number }) => {
    position.current = point;
    if (ghostElement.current) {
      ghostElement.current.style.left = `${point.x + 12}px`;
      ghostElement.current.style.top = `${point.y + 8}px`;
    }
  };
  const ghostRef = useCallback((element: HTMLDivElement | null) => {
    ghostElement.current = element;
    if (element) {
      element.style.left = `${position.current.x + 12}px`;
      element.style.top = `${position.current.y + 8}px`;
    }
  }, []);

  const onPointerDown = (
    event: ReactPointerEvent<HTMLButtonElement>,
    source: ProjectDropTarget,
    label: string,
  ) => {
    if (options.disabled || event.button !== 0 || !event.isPrimary) return;
    cleanup.current?.();
    suppressClick.current = null;
    const element = event.currentTarget;
    const pointerId = event.pointerId;
    const start = { x: event.clientX, y: event.clientY };
    position.current = start;
    let active = false;
    let currentDrop: Drop | null = null;
    let frame: number | null = null;
    let previousUserSelect = "";
    let previousCursor = "";

    const updateDrop = () => {
      const { x, y } = position.current;
      let hit = document
        .elementFromPoint(x, y)
        ?.closest<HTMLElement>("[data-sidebar-drop-id]");
      if (
        source.kind === "project" &&
        hit?.dataset.sidebarDropKind === "task" &&
        hit.dataset.sidebarDropSection !== "pinned"
      ) {
        hit =
          hit.closest<HTMLElement>('[data-sidebar-drop-kind="project"]') ??
          undefined;
      }
      const id = hit?.dataset.sidebarDropId;
      const kind = hit?.dataset.sidebarDropKind;
      const section = hit?.dataset.sidebarDropSection;
      let next: Drop | null = null;
      if (
        hit &&
        rootRef.current?.contains(hit) &&
        id &&
        (kind === "project" || kind === "task") &&
        section === source.section &&
        (source.kind === "project"
          ? section === "pinned" || kind === "project"
          : kind === "task" && hit.dataset.sidebarDropOwner === source.owner) &&
        !(kind === source.kind && id === source.id)
      ) {
        const bounds = hit.getBoundingClientRect();
        next = {
          target: {
            kind,
            id,
            section,
            ...(source.kind === "task"
              ? { owner: hit.dataset.sidebarDropOwner }
              : {}),
          },
          edge: y < bounds.top + bounds.height / 2 ? "before" : "after",
        };
      }
      if (
        next?.target.id !== currentDrop?.target.id ||
        next?.target.kind !== currentDrop?.target.kind ||
        next?.edge !== currentDrop?.edge
      ) {
        currentDrop = next;
        setDrop(next);
      }
    };
    // 只有指针贴近可滚动区域边缘时持续滚动，离开边缘即停止帧循环。
    const scrollAtEdge = () => {
      frame = null;
      const viewport = rootRef.current?.querySelector<HTMLElement>(
        '[data-slot="scroll-area-viewport"]',
      );
      if (!active || !viewport) return;
      const bounds = viewport.getBoundingClientRect();
      const { x, y } = position.current;
      if (
        x < bounds.left ||
        x > bounds.right ||
        y < bounds.top ||
        y > bounds.bottom
      )
        return;
      const amount = y < bounds.top + 24 ? -8 : y > bounds.bottom - 24 ? 8 : 0;
      const previous = viewport.scrollTop;
      if (amount) viewport.scrollTop += amount;
      if (viewport.scrollTop !== previous) {
        updateDrop();
        frame = requestAnimationFrame(scrollAtEdge);
      }
    };
    const gesture = createProjectDragGesture(start, {
      activate: () => {
        if (!element.isConnected || opts.current.disabled) {
          finish(false);
          return;
        }
        active = true;
        previousUserSelect = document.body.style.userSelect;
        previousCursor = document.body.style.cursor;
        document.body.style.userSelect = "none";
        document.body.style.cursor = "grabbing";
        element.setPointerCapture(pointerId);
        setDragging({ source, label });
        placeGhost(start);
      },
      move: (point) => {
        placeGhost(point);
        updateDrop();
        if (frame === null) frame = requestAnimationFrame(scrollAtEdge);
      },
      finish: (commit) => {
        if (commit && currentDrop && !opts.current.disabled)
          opts.current.onMove(source, currentDrop.target, currentDrop.edge);
      },
    });
    const move = (event: PointerEvent) => {
      if (event.pointerId !== pointerId) return;
      gesture.move({ x: event.clientX, y: event.clientY });
      if (active) event.preventDefault();
    };
    const finish = (commit: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("blur", blur);
      window.removeEventListener("keydown", keydown);
      element.removeEventListener("lostpointercapture", blur);
      cleanup.current = null;
      const shouldSuppress = gesture.end(commit);
      if (frame !== null) cancelAnimationFrame(frame);
      if (active) {
        document.body.style.userSelect = previousUserSelect;
        document.body.style.cursor = previousCursor;
        if (element.hasPointerCapture(pointerId))
          element.releasePointerCapture(pointerId);
      }
      active = false;
      setDragging(null);
      setDrop(null);
      if (shouldSuppress) {
        // Escape 取消后鼠标可能稍后才释放，必须拦截该按钮的释放点击。
        suppressClick.current = element;
      }
    };
    const up = (event: PointerEvent) => {
      if (event.pointerId !== pointerId) return;
      if (active) {
        position.current = { x: event.clientX, y: event.clientY };
        updateDrop();
      }
      finish(true);
    };
    const cancel = (event: PointerEvent) => {
      if (event.pointerId === pointerId) finish(false);
    };
    const blur = () => finish(false);
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        finish(false);
      }
    };
    cleanup.current = () => finish(false);
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("blur", blur);
    window.addEventListener("keydown", keydown);
    element.addEventListener("lostpointercapture", blur);
  };
  const onClickCapture = (event: MouseEvent) => {
    if (
      !suppressClick.current ||
      event.detail === 0 ||
      !(event.target instanceof Node) ||
      !suppressClick.current.contains(event.target)
    )
      return;
    suppressClick.current = null;
    event.preventDefault();
    event.stopPropagation();
  };
  return { rootRef, ghostRef, dragging, drop, onPointerDown, onClickCapture };
}
