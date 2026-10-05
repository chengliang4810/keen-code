import { useCallback, useEffect, useRef, useState } from "react";
import type { WorkspaceFileEntry } from "@zcode/shared";
import { useWorkspaceServices } from "@/hooks/useWorkspaceServices.js";

/** Rust 持有完整索引和 fuzzy top-K；组件只持有当前有界结果，不启动文件搜索 Worker。 */
export function useWorkspaceFileSearchIndex({
  workspacePath, workspaceIdentity, workspaceRemoteSessionId, enabled, query,
}: {
  workspacePath: string;
  workspaceIdentity?: string;
  workspaceRemoteSessionId?: string;
  enabled: boolean;
  query: string;
}) {
  const { fileService } = useWorkspaceServices(workspacePath, workspaceRemoteSessionId, workspaceIdentity);
  const [entries, setEntries] = useState<WorkspaceFileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const lastRefresh = useRef(0);
  const refresh = useCallback(() => setRefreshVersion((version) => version + 1), []);

  useEffect(() => {
    let cancelled = false;
    setEntries([]);
    setLoaded(false);
    setError(null);
    setLoading(enabled);
    if (!enabled) return;
    const forceRefresh = lastRefresh.current !== refreshVersion;
    lastRefresh.current = refreshVersion;
    // 快速连续键入合并为一次请求；工作区切换/卸载取消计时器并丢弃迟到结果。
    const timer = setTimeout(() => {
      void fileService.searchWorkspaceFiles({ rootPath: workspacePath, workspaceIdentity, query, limit: 1000, requireQuery: true, refresh: forceRefresh })
        .then((result) => { if (!cancelled) { setEntries(result); setLoaded(true); } })
        .catch((failure) => { if (!cancelled) setError(failure instanceof Error ? failure : new Error(String(failure))); })
        .finally(() => { if (!cancelled) setLoading(false); });
    }, 120);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [enabled, fileService, query, refreshVersion, workspaceIdentity, workspacePath, workspaceRemoteSessionId]);
  return { entries, loading, loaded, error, refresh };
}
