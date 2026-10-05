import { useEffect } from "react";
import type { IFileWatcherService } from "@zcode/services";
import { logger } from "@/logger.js";

const WATCH_DEBOUNCE_MS = 300;

/** workspacePath 可能是 Windows 路径；沿用它自己的分隔符拼子目录，别把 `/` 混进 `\\` 路径。 */
function savedWorkflowsDirectoryPath(workspacePath: string): string {
  const separator = workspacePath.includes("\\") && !workspacePath.includes("/") ? "\\" : "/";
  const trimmed = workspacePath.replace(/[\\/]+$/u, "");
  return `${trimmed}${separator}.zcode${separator}workflows`;
}

/**
 * 目录监听：对话里 SaveWorkflow 落盘后中枢自动更新。
 * 后端 list 返回的目录不存在时，受限 watcher 会按需创建；未拿到 list 目录前不猜测本地路径。
 * 非递归：只看这一层（Linux 上递归 fs.watch 有既知问题）。服务实例变化（远程重连）时
 * effect 依赖变化会拆掉旧 watcher 重建，旧 host 的 id 不会泄漏。
 *
 * 项目组与全局组都优先传协议 list 返回的 `directory`。`workspacePath` 仅供远端实现仍以工作区
 * 路径作为 watcher 输入时使用；本地项目绝不能回退到用户工作区 `.zcode/workflows`。
 */
export function useSavedWorkflowsDirectoryWatch({
  fileWatcherService,
  workspacePath,
  directory,
  enabled,
  refresh,
}: {
  fileWatcherService: IFileWatcherService;
  workspacePath?: string | null | undefined;
  directory?: string | null | undefined;
  enabled: boolean;
  refresh: (options: { bypassCache?: boolean }) => Promise<void>;
}): void {
  const resolvedDirectory =
    directory ?? (workspacePath ? savedWorkflowsDirectoryPath(workspacePath) : null);
  useEffect(() => {
    if (!enabled || !resolvedDirectory) return;
    let disposed = false;
    let watchId: string | null = null;
    let subscription: { dispose: () => void } | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const directoryPath = resolvedDirectory;
    void fileWatcherService
      .watch({ path: directoryPath })
      .then(({ id }) => {
        if (disposed) {
          void fileWatcherService.unwatch({ id });
          return;
        }
        watchId = id;
        subscription = fileWatcherService.onDynamicChange(id)(() => {
          if (timer) clearTimeout(timer);
          timer = setTimeout(() => {
            timer = null;
            void refresh({ bypassCache: true });
          }, WATCH_DEBOUNCE_MS);
        });
      })
      .catch((error: unknown) => {
        logger.warn("[SavedWorkflows] 监听已保存工作流目录失败", {
          path: directoryPath,
          error: error instanceof Error ? error.message : String(error),
        });
      });
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      subscription?.dispose();
      if (watchId) void fileWatcherService.unwatch({ id: watchId });
    };
  }, [enabled, fileWatcherService, refresh, resolvedDirectory]);
}
