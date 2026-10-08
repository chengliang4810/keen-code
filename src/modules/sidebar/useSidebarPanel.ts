import { uiState } from "@/lib/uiState";
import {
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import type { PanelImperativeHandle } from "react-resizable-panels";
import type { SidebarViewId } from "./types";
import { SIDEBAR_WIDTH_STORAGE_KEY, useSidebarWidth } from "./sidebarWidth";

export {
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_MAX_WIDTH,
} from "./sidebarWidth";
const SIDEBAR_VIEW_STORAGE_KEY = "rcode.sidebar.view";
const SIDEBAR_COLLAPSED_STORAGE_KEY = "rcode.sidebar.collapsed";

export function shouldPersistSidebarWidth(
  width: number,
  isUserInteraction: boolean,
): boolean {
  return isUserInteraction && width > 0;
}

function readSidebarView(): SidebarViewId {
  try {
    const stored = uiState.getItem(SIDEBAR_VIEW_STORAGE_KEY);
    if (
      stored === "tasks" ||
      stored === "explorer" ||
      stored === "source-control"
    )
      return stored;
  } catch {
    // ignore
  }
  return "tasks";
}

function readSidebarCollapsed(): boolean {
  try {
    return uiState.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

type FocusableExplorer = {
  focus: () => void;
  isFocused: () => boolean;
};

export function useSidebarPanel(
  explorerRef: RefObject<FocusableExplorer | null>,
) {
  const sidebarRef = useRef<PanelImperativeHandle | null>(null);
  const sidebarWidthRef = useRef(useSidebarWidth.getState().width);
  const sidebarWidthWriteTimerRef = useRef(0);
  const explorerReturnFocusRef = useRef<HTMLElement | null>(null);
  const [sidebarView, setSidebarViewState] =
    useState<SidebarViewId>(readSidebarView);
  const [initialSidebarCollapsed] = useState(readSidebarCollapsed);
  const collapsedRef = useRef(initialSidebarCollapsed);

  const persistSidebarView = useCallback((view: SidebarViewId) => {
    setSidebarViewState(view);
    try {
      uiState.setItem(SIDEBAR_VIEW_STORAGE_KEY, view);
    } catch {
      // storage may fail in private mode
    }
  }, []);

  const persistSidebarCollapsed = useCallback((collapsed: boolean) => {
    if (collapsedRef.current === collapsed) return;
    collapsedRef.current = collapsed;
    try {
      uiState.setItem(
        SIDEBAR_COLLAPSED_STORAGE_KEY,
        collapsed ? "1" : "0",
      );
    } catch {
      // storage may fail in private mode
    }
  }, []);

  const toggleSidebar = useCallback(() => {
    const p = sidebarRef.current;
    if (!p) return;
    if (p.getSize().asPercentage <= 0) p.resize(`${sidebarWidthRef.current}px`);
    else p.collapse();
  }, []);

  const cycleSidebarView = useCallback(
    (view: SidebarViewId) => {
      const panel = sidebarRef.current;
      const collapsed = panel ? panel.getSize().asPercentage <= 0 : false;
      if (collapsed) {
        if (panel) panel.resize(`${sidebarWidthRef.current}px`);
        if (view !== sidebarView) persistSidebarView(view);
        return;
      }
      if (view === sidebarView) {
        panel?.collapse();
        return;
      }
      persistSidebarView(view);
    },
    [persistSidebarView, sidebarView],
  );

  const openSidebarView = useCallback(
    (view: SidebarViewId) => {
      const panel = sidebarRef.current;
      if (panel && panel.getSize().asPercentage <= 0) {
        panel.resize(`${sidebarWidthRef.current}px`);
      }
      if (view !== sidebarView) persistSidebarView(view);
    },
    [persistSidebarView, sidebarView],
  );

  const persistSidebarWidth = useCallback(
    (next: number, isUserInteraction: boolean) => {
      if (!shouldPersistSidebarWidth(next, isUserInteraction)) return;
      sidebarWidthRef.current = next;
      if (sidebarWidthWriteTimerRef.current) {
        window.clearTimeout(sidebarWidthWriteTimerRef.current);
      }
      sidebarWidthWriteTimerRef.current = window.setTimeout(() => {
        sidebarWidthWriteTimerRef.current = 0;
        try {
          uiState.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(next));
        } catch {
          // ignore
        }
      }, 200);
    },
    [],
  );

  useEffect(() => {
    return () => {
      if (sidebarWidthWriteTimerRef.current) {
        window.clearTimeout(sidebarWidthWriteTimerRef.current);
      }
    };
  }, []);

  const toggleExplorerFocus = useCallback(() => {
    const explorer = explorerRef.current;
    const panel = sidebarRef.current;
    const collapsed = panel ? panel.getSize().asPercentage <= 0 : false;
    if (sidebarView !== "explorer" || collapsed) {
      if (panel && collapsed) panel.resize(`${sidebarWidthRef.current}px`);
      if (sidebarView !== "explorer") persistSidebarView("explorer");
      const active = document.activeElement;
      explorerReturnFocusRef.current =
        active instanceof HTMLElement && active !== document.body
          ? active
          : null;
      requestAnimationFrame(() => explorerRef.current?.focus());
      return;
    }
    if (!explorer) return;
    if (explorer.isFocused()) {
      const target = explorerReturnFocusRef.current;
      explorerReturnFocusRef.current = null;
      if (target && document.body.contains(target)) {
        target.focus();
      } else {
        (document.activeElement as HTMLElement | null)?.blur?.();
      }
      return;
    }
    const active = document.activeElement;
    explorerReturnFocusRef.current =
      active instanceof HTMLElement && active !== document.body ? active : null;
    explorer.focus();
  }, [explorerRef, persistSidebarView, sidebarView]);

  return {
    sidebarRef,
    sidebarWidthRef,
    reportSidebarWidth: useSidebarWidth.getState().reportWidth,
    sidebarView,
    initialSidebarCollapsed,
    persistSidebarView,
    persistSidebarCollapsed,
    toggleSidebar,
    cycleSidebarView,
    openSidebarView,
    persistSidebarWidth,
    toggleExplorerFocus,
  };
}
