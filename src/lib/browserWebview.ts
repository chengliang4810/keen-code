/**
 * 内置浏览器子 WebView 的命令入口。
 *
 * 同一标签的命令必须串行：React 严格模式会先卸载再挂载，若关闭与创建并发，
 * 迟到的关闭会销毁刚创建的 WebView，页面会变成空白。
 */

import * as api from "@/lib/api";
import type { BrowserRect } from "@/lib/api";
import { syncNativeThemeSurfaces } from "./nativeTheme";

/** 每个标签的命令尾链；链上命令保证按调用顺序执行。 */
const queues = new Map<string, Promise<unknown>>();

/** 把命令追加到标签队列，前一个命令失败也继续执行。 */
export function enqueueBrowserOp<T>(tabId: string, op: () => Promise<T>): Promise<T> {
  const previous = queues.get(tabId) ?? Promise.resolve();
  const next = previous.then(op, op);
  const tail = next.then(
    () => undefined,
    () => undefined,
  );
  queues.set(tabId, tail);
  void tail.then(() => {
    // 队列空闲后移除表项，避免长期保留已关闭标签。
    if (queues.get(tabId) === tail) queues.delete(tabId);
  });
  return next;
}

export function openBrowserWebview(tabId: string, url: string, rect: BrowserRect) {
  return enqueueBrowserOp(tabId, async () => {
    await api.browserOpen(tabId, url, rect);
    // 子 WebView 创建后才出现在 Tauri 的列表中；在显示前消费当前主题底色。
    await syncNativeThemeSurfaces();
  });
}

export function setBrowserBounds(tabId: string, rect: BrowserRect) {
  return enqueueBrowserOp(tabId, () => api.browserBounds(tabId, rect));
}

export function showBrowserWebview(tabId: string) {
  return enqueueBrowserOp(tabId, () => api.browserShow(tabId));
}

export function hideBrowserWebview(tabId: string) {
  return enqueueBrowserOp(tabId, () => api.browserHide(tabId));
}

export function closeBrowserWebview(tabId: string) {
  return enqueueBrowserOp(tabId, () => api.browserClose(tabId));
}

export function navigateBrowserWebview(tabId: string, url: string) {
  return enqueueBrowserOp(tabId, () => api.browserNavigate(tabId, url));
}

export function reloadBrowserWebview(tabId: string) {
  return enqueueBrowserOp(tabId, () => api.browserReload(tabId));
}

export function browserHistory(tabId: string, direction: "back" | "forward") {
  return enqueueBrowserOp(tabId, () => api.browserHistory(tabId, direction));
}
