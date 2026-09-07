import { invoke as nativeInvoke } from "@tauri-apps/api/core";
import { isTauri } from "./tauri";

/** 前端异常日志允许写入后端的最大字符数。 */
const FRONTEND_ERROR_MAX_CHARS = 12_000;

/** 将任意抛出值转换成包含堆栈的有限长度文本。 */
export function formatFrontendError(value: unknown): string {
  let text: string;
  if (value instanceof Error) {
    text = `${value.name}: ${value.message}${value.stack ? `\n${value.stack}` : ""}`;
  } else if (typeof value === "string") {
    text = value;
  } else {
    try {
      const serialized = JSON.stringify(value);
      text = serialized ?? String(value);
    } catch {
      try {
        text = String(value);
      } catch {
        text = "[unprintable value]";
      }
    }
  }
  return text.slice(0, FRONTEND_ERROR_MAX_CHARS);
}

/** 尽力把前端异常写入统一诊断日志；日志失败不得再次抛错。 */
export function reportFrontendError(component: string, value: unknown): void {
  if (!isTauri()) return;
  // 直接使用原生入口，避免诊断写入失败再次触发 IPC 错误上报。
  void nativeInvoke("diagnostics_record", { component, message: formatFrontendError(value) }).catch(() => {});
}

/** 注册浏览器全局同步异常与未处理 Promise 拒绝监听。 */
export function installFrontendErrorHandlers(): () => void {
  /** 记录未被业务代码捕获的同步异常。 */
  const onError = (event: ErrorEvent) => {
    const location = event.filename
      ? `\nsource=${event.filename}:${event.lineno}:${event.colno}`
      : "";
    reportFrontendError(
      "frontend.window_error",
      `${formatFrontendError(event.error ?? event.message ?? `Resource failed: ${(event.target as Element | null)?.tagName ?? "unknown"}`)}${location}`,
    );
  };
  /** 记录未被业务代码处理的异步拒绝。 */
  const onUnhandledRejection = (event: PromiseRejectionEvent) => {
    reportFrontendError("frontend.unhandled_rejection", event.reason);
  };
  const originalError = console.error;
  const loggedError: typeof console.error = (...args) => {
    originalError.apply(console, args);
    reportFrontendError("frontend.console_error", args.map(formatFrontendError).join("\n"));
  };
  console.error = loggedError;
  window.addEventListener("error", onError, { capture: true });
  window.addEventListener("unhandledrejection", onUnhandledRejection);
  return () => {
    if (console.error === loggedError) console.error = originalError;
    window.removeEventListener("error", onError, { capture: true });
    window.removeEventListener("unhandledrejection", onUnhandledRejection);
  };
}
