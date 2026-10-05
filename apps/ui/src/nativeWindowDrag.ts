import { invoke } from "@tauri-apps/api/core";

const START_WINDOW_DRAG_COMMAND = "desktop_start_window_drag";
const TOGGLE_MAXIMIZE_COMMAND = "desktop_execute_command";
const DRAG_REGION_CLASS = "[app-region:drag]";
const NO_DRAG_REGION_CLASS = "[app-region:no-drag]";

/** DOM 解析后的窗口拖拽命中结果，保持策略可在无浏览器环境中单独测试。 */
export interface NativeWindowDragTargetState {
  hasDragRegion: boolean;
  hasNoDragRegion: boolean;
  isInteractive: boolean;
}

/** 只有源码明确提供的拖拽区才可触发原生窗口移动。 */
export function shouldStartNativeWindowDrag(target: NativeWindowDragTargetState): boolean {
  return target.hasDragRegion && !target.hasNoDragRegion && !target.isInteractive;
}

function isTauriWindow(): boolean {
  return typeof window !== "undefined" &&
    Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

function computedAppRegion(element: Element): string | undefined {
  if (typeof window === "undefined") return undefined;
  const style = window.getComputedStyle(element);
  const region =
    style.getPropertyValue("app-region").trim() ||
    style.getPropertyValue("-webkit-app-region").trim();
  return region === "drag" || region === "no-drag" ? region : undefined;
}

function hasInteractiveRole(element: Element): boolean {
  const tagName = element.tagName.toLowerCase();
  if (["a", "button", "input", "option", "select", "textarea", "summary"].includes(tagName)) {
    return true;
  }
  if (
    element.getAttribute("contenteditable") !== null &&
    element.getAttribute("contenteditable") !== "false"
  ) {
    return true;
  }
  if (element.getAttribute("draggable") === "true") return true;
  if (element.getAttribute("role") === "button" || element.getAttribute("role") === "link") {
    return true;
  }
  // 侧栏 Tab 的 no-drag 规则由源 CSS 的嵌套选择器表达，DOM 本身没有独立 class。
  return element.hasAttribute("data-side-pane-tab-id") || element.hasAttribute("data-no-window-drag");
}

function readTargetState(target: EventTarget | null): NativeWindowDragTargetState {
  if (typeof Element === "undefined" || !(target instanceof Element)) {
    return { hasDragRegion: false, hasNoDragRegion: false, isInteractive: false };
  }

  let current: Element | null = target;
  let hasDragRegion = false;
  let hasNoDragRegion = false;
  let isInteractive = false;
  while (current) {
    if (current.classList.contains(DRAG_REGION_CLASS) || computedAppRegion(current) === "drag") {
      hasDragRegion = true;
    }
    if (
      current.classList.contains(NO_DRAG_REGION_CLASS) ||
      computedAppRegion(current) === "no-drag"
    ) {
      hasNoDragRegion = true;
    }
    if (hasInteractiveRole(current)) isInteractive = true;
    current = current.parentElement;
  }
  return { hasDragRegion, hasNoDragRegion, isInteractive };
}

function isPrimaryMouseEvent(event: PointerEvent | MouseEvent): boolean {
  if (event.button !== 0 || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
    return false;
  }
  return !("isPrimary" in event) || event.isPrimary;
}

function reportWindowDragFailure(error: unknown): void {
  // 拖拽失败不应阻断页面交互，但保留带组件前缀的低频诊断，便于原生验收定位。
  console.warn("[native-window-drag] 原生窗口操作失败", error);
}

/** 安装一次性的 renderer 事件代理；返回值用于页面刷新和卸载时移除监听。 */
export function installNativeWindowDrag(): () => void {
  if (!isTauriWindow()) return () => {};

  const onPointerDown = (event: PointerEvent) => {
    if (event.pointerType !== "mouse" || !isPrimaryMouseEvent(event)) return;
    const target = readTargetState(event.target);
    if (!shouldStartNativeWindowDrag(target)) return;
    void invoke<void>(START_WINDOW_DRAG_COMMAND, { button: event.button }).catch(
      reportWindowDragFailure,
    );
  };

  const onDoubleClick = (event: MouseEvent) => {
    if (!isPrimaryMouseEvent(event)) return;
    if (!shouldStartNativeWindowDrag(readTargetState(event.target))) return;
    event.preventDefault();
    void invoke<void>(TOGGLE_MAXIMIZE_COMMAND, { command: "toggleMaximizeWindow" }).catch(
      reportWindowDragFailure,
    );
  };

  // 捕获阶段能看到按钮内部的实际 target，从而在 no-drag 边界处提前退出。
  document.addEventListener("pointerdown", onPointerDown, true);
  document.addEventListener("dblclick", onDoubleClick, true);
  return () => {
    document.removeEventListener("pointerdown", onPointerDown, true);
    document.removeEventListener("dblclick", onDoubleClick, true);
  };
}
