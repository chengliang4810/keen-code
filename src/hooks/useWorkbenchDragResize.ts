import {
  useCallback,
  useEffect,
  useRef,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { LayoutPrefs } from "@/lib/layout";
import {
  ASIDE_WIDTH_MIN,
  SIDEBAR_WIDTH_MIN,
  clampAsideWidth,
  clampSidebarWidth,
  saveLayout,
  shouldCollapsePane,
} from "@/lib/layout";
import {
  hitDragZoneFromRects,
  toClientDragPoint,
  type DragZone,
} from "@/lib/dragZone";

type StateSetter<T> = Dispatch<SetStateAction<T>>;

export type WorkbenchPlatform = "mac" | "win" | "other";

export type WorkbenchTranslator = (key: MessageKey, vars?: Vars) => string;

export interface UseWorkbenchDragResizeOptions {
  isTauri: () => boolean;
  platform: WorkbenchPlatform;
  addProjectOpen: boolean;
  addProjectDropRef: RefObject<HTMLElement | null>;
  setDragZone: StateSetter<DragZone>;
  selectAddProjectSourceFromPaths: (paths: string[]) => void | Promise<void>;
  addAttachmentsFromPaths: (paths: string[]) => void | Promise<void>;
  setLocalError: StateSetter<string | null>;
  translate: WorkbenchTranslator;
  sidebarRef: RefObject<HTMLElement | null>;
  asideRef: RefObject<HTMLElement | null>;
  layout: LayoutPrefs;
  setLayout: StateSetter<LayoutPrefs>;
  resizingSidebar: boolean;
  setResizingSidebar: StateSetter<boolean>;
  resizingAside: boolean;
  setResizingAside: StateSetter<boolean>;
}

/** 管理桌面拖放和左右工作区面板的高频尺寸调整。 */
export function useWorkbenchDragResize({
  isTauri,
  platform,
  addProjectOpen,
  addProjectDropRef,
  setDragZone,
  selectAddProjectSourceFromPaths,
  addAttachmentsFromPaths,
  setLocalError,
  translate,
  sidebarRef,
  asideRef,
  layout,
  setLayout,
  resizingSidebar,
  setResizingSidebar,
  resizingAside,
  setResizingAside,
}: UseWorkbenchDragResizeOptions): void {
  const dragPathsRef = useRef<string[]>([]);
  const hitDragZone = useCallback(
    (clientX: number, clientY: number): DragZone =>
      hitDragZoneFromRects(
        clientX,
        clientY,
        addProjectDropRef.current?.getBoundingClientRect() ?? null,
        addProjectOpen,
      ),
    [addProjectDropRef, addProjectOpen],
  );

  // Tauri OS file drag-drop (full absolute paths).
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const webview = getCurrentWebview();
        const win = getCurrentWindow();
        const factor = await win.scaleFactor();

        const stopListening = await webview.onDragDropEvent((event) => {
          if (cancelled) return;
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "drop") {
            if ("paths" in payload && payload.paths?.length) {
              dragPathsRef.current = payload.paths;
            }
          }
          if (payload.type === "leave") {
            setDragZone(null);
            dragPathsRef.current = [];
            return;
          }
          if (payload.type === "enter" || payload.type === "over") {
            const { x, y } = toClientDragPoint(
              payload.position,
              factor,
              platform,
            );
            setDragZone(hitDragZone(x, y));
            return;
          }
          if (payload.type === "drop") {
            const { x, y } = toClientDragPoint(
              payload.position,
              factor,
              platform,
            );
            const zone = hitDragZone(x, y);
            const paths = payload.paths?.length
              ? payload.paths
              : dragPathsRef.current;
            setDragZone(null);
            dragPathsRef.current = [];
            if (!paths.length) {
              setLocalError(translate("attach.droppedNone"));
              return;
            }
            if (zone === "project") {
              void selectAddProjectSourceFromPaths(paths);
            } else if (zone === "main") {
              void addAttachmentsFromPaths(paths);
            }
          }
        });
        if (cancelled) stopListening();
        else unlisten = stopListening;
      } catch {
        /* webview API unavailable */
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [
    addAttachmentsFromPaths,
    hitDragZone,
    isTauri,
    platform,
    selectAddProjectSourceFromPaths,
    setDragZone,
    setLocalError,
    translate,
  ]);

  // HTML5 fallback: some image drags only expose File list in the webview.
  useEffect(() => {
    const onDragOver = (event: DragEvent) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
    };
    const onDrop = (event: DragEvent) => {
      if (!event.dataTransfer?.files?.length) return;
      const files = Array.from(event.dataTransfer.files);
      const paths = files
        .map((file) => (file as File & { path?: string }).path || "")
        .filter(Boolean);
      const zone = hitDragZone(event.clientX, event.clientY);
      if (!paths.length) return;
      event.preventDefault();
      event.stopPropagation();
      if (zone === "project") void selectAddProjectSourceFromPaths(paths);
      else if (zone === "main") void addAttachmentsFromPaths(paths);
    };
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("drop", onDrop);
    };
  }, [addAttachmentsFromPaths, hitDragZone, selectAddProjectSourceFromPaths]);

  useEffect(() => {
    if (!resizingSidebar) return;
    let frame = 0;
    let pendingWidth = layout.sidebarWidth;
    const paint = () => {
      frame = 0;
      const pane = sidebarRef.current;
      if (!pane) return;
      const width = `${pendingWidth}px`;
      pane.style.width = width;
      pane.style.minWidth = width;
      pane.style.maxWidth = width;
    };
    const onMove = (event: PointerEvent) => {
      const collapsed = shouldCollapsePane(event.clientX, SIDEBAR_WIDTH_MIN);
      pendingWidth = clampSidebarWidth(event.clientX);
      if (collapsed) {
        if (frame) cancelAnimationFrame(frame);
        setLayout((current) => {
          const next = {
            ...current,
            sidebarWidth: pendingWidth,
            sidebarCollapsed: true,
          };
          saveLayout(localStorage, next);
          return next;
        });
        setResizingSidebar(false);
      } else if (!frame) {
        frame = requestAnimationFrame(paint);
      }
    };
    const onUp = () => {
      if (frame) cancelAnimationFrame(frame);
      paint();
      setResizingSidebar(false);
      setLayout((current) => {
        const next = { ...current, sidebarWidth: pendingWidth };
        saveLayout(localStorage, next);
        return next;
      });
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [layout.sidebarWidth, resizingSidebar, setLayout, setResizingSidebar, sidebarRef]);

  useEffect(() => {
    if (!resizingAside) return;
    let frame = 0;
    let pendingWidth = layout.asideWidth;
    const paint = () => {
      frame = 0;
      const pane = asideRef.current;
      if (!pane) return;
      const width = `${pendingWidth}px`;
      pane.style.width = width;
      pane.style.minWidth = width;
      pane.style.maxWidth = width;
    };
    const onMove = (event: PointerEvent) => {
      const rawWidth = window.innerWidth - event.clientX;
      const collapsed = shouldCollapsePane(rawWidth, ASIDE_WIDTH_MIN);
      pendingWidth = clampAsideWidth(
        rawWidth,
        window.innerWidth -
          (layout.sidebarCollapsed ? 0 : layout.sidebarWidth),
      );
      if (collapsed) {
        if (frame) cancelAnimationFrame(frame);
        setLayout((current) => {
          const next = {
            ...current,
            asideWidth: pendingWidth,
            asideCollapsed: true,
          };
          saveLayout(localStorage, next);
          return next;
        });
        setResizingAside(false);
      } else if (!frame) {
        frame = requestAnimationFrame(paint);
      }
    };
    const onUp = () => {
      if (frame) cancelAnimationFrame(frame);
      paint();
      setResizingAside(false);
      setLayout((current) => {
        const next = { ...current, asideWidth: pendingWidth };
        saveLayout(localStorage, next);
        return next;
      });
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [
    asideRef,
    layout.asideWidth,
    layout.sidebarCollapsed,
    layout.sidebarWidth,
    resizingAside,
    setLayout,
    setResizingAside,
  ]);

}
