import { reportFrontendError } from "./frontendDiagnostics";

/** 判断当前界面是否运行在 Tauri WebView 中。 */
export function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

/** 调用当前桌面后端注册的 Tauri 命令。 */
export async function invoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isTauri()) throw new Error(`Tauri required: ${command}`);
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  try {
    return await tauriInvoke<T>(command, args);
  } catch (error) {
    if (command !== "diagnostics_record") reportFrontendError(`frontend.ipc.${command}`, error);
    throw error;
  }
}
