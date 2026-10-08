import { uiState } from "@/lib/uiState";
import { create } from "zustand";

export const SIDEBAR_DEFAULT_WIDTH = 260;
export const SIDEBAR_MIN_WIDTH = 220;
export const SIDEBAR_MAX_WIDTH = 480;
export const SIDEBAR_WIDTH_STORAGE_KEY = "rcode.sidebar.width";

function readSidebarWidth(): number {
  try {
    const stored = uiState.getItem(SIDEBAR_WIDTH_STORAGE_KEY);
    const parsed = stored ? Number.parseInt(stored, 10) : NaN;
    return Number.isFinite(parsed)
      ? Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, parsed))
      : SIDEBAR_DEFAULT_WIDTH;
  } catch {
    return SIDEBAR_DEFAULT_WIDTH;
  }
}

/** 共享未缩放的实际布局宽度，收起时保留最近一次展开值。 */
export const useSidebarWidth = create<{
  width: number;
  reportWidth: (width: number) => void;
}>((set, get) => ({
  width: readSidebarWidth(),
  reportWidth: (width) => {
    if (!Number.isFinite(width) || width <= 0 || width === get().width) return;
    set({ width });
  },
}));
