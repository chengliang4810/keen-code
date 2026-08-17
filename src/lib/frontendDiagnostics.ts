import { diagnosticsRecord } from "@/lib/acp/api";

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
  void diagnosticsRecord(component, formatFrontendError(value)).catch(() => {});
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
      `${formatFrontendError(event.error ?? event.message)}${location}`,
    );
  };
  /** 记录未被业务代码处理的异步拒绝。 */
  const onUnhandledRejection = (event: PromiseRejectionEvent) => {
    reportFrontendError("frontend.unhandled_rejection", event.reason);
  };
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onUnhandledRejection);
  return () => {
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onUnhandledRejection);
  };
}
