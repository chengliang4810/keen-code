/** 资源查看器"变更"子域的 Git 工作区状态加载与派生。 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import * as api from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import type { Locale } from "@/i18n";
import {
  filterWorkspaceGitEntries,
  normalizeWorkspaceGitEntries,
  type WorkspaceGitFile,
} from "@/lib/workspaceGit";
import { countWorkspaceChangeFiles } from "@/lib/resourceViewerTree";

/** 工具状态连发后的强制读取防抖间隔。 */
const WORKSPACE_SYNC_DEBOUNCE_MS = 200;

/** `useWorkspaceChanges` 的输入依赖。 */
export interface UseWorkspaceChangesOptions {
  /** 当前项目根；为空时本子域整体归零。 */
  projectPath: string | null;
  /** 复用变更列表的搜索过滤词。 */
  query: string;
  /** 侧栏面板当前是否可见。 */
  paneActive: boolean;
  /** 当前是否处于"变更"模式；仅在可见时同步 Git 状态。 */
  changesActive: boolean;
  /** 工具状态修订号；变化触发防抖后的强制读取。 */
  syncRevision: number;
  /** 本地化错误所需语言。 */
  locale: Locale;
  /** 加载失败且已有旧快照时的错误上抛出口。 */
  onError: (message: string) => void;
}

/** `useWorkspaceChanges` 返回的 Git 工作区状态。 */
export interface WorkspaceChangesState {
  /** 归一化后的全部变更条目。 */
  files: WorkspaceGitFile[];
  /** 是否正在读取（首个快照前显示加载态）。 */
  loading: boolean;
  /** 当前目录是否可用 Git。 */
  available: boolean;
  /** 不可用原因或最近一次失败文本。 */
  reason: string | null;
  /** 当前分支名；不可用时也可能存在。 */
  branch: string | null;
  /** 徽标使用的变更文件计数。 */
  count: number;
  /** 应用搜索词后的条目子集。 */
  filtered: WorkspaceGitFile[];
  /** 手动刷新；`force` 跳过短时缓存。 */
  refresh: (force?: boolean) => Promise<void>;
  /** 未跟踪目录展开等调用方需要就地替换列表时的出口。 */
  setFiles: Dispatch<SetStateAction<WorkspaceGitFile[]>>;
}

/** 管理"变更"子域的 Git 状态读取、防抖同步与项目切换重置。 */
export function useWorkspaceChanges(
  options: UseWorkspaceChangesOptions,
): WorkspaceChangesState {
  const { projectPath, query, paneActive, changesActive, syncRevision, locale, onError } =
    options;
  const [files, setFiles] = useState<WorkspaceGitFile[]>([]);
  const [loading, setLoading] = useState(false);
  const [available, setAvailable] = useState(false);
  const [reason, setReason] = useState<string | null>(null);
  const [branch, setBranch] = useState<string | null>(null);
  const loadSeq = useRef(0);
  const lastSyncRevision = useRef(syncRevision);
  const hasSnapshot = useRef(false);
  const projectPathRef = useRef(projectPath);
  if (projectPathRef.current !== projectPath) {
    projectPathRef.current = projectPath;
    loadSeq.current += 1;
    hasSnapshot.current = false;
    lastSyncRevision.current = syncRevision;
  }

  /** 读取 Git 状态；工具完成后的刷新可跳过短时缓存。 */
  const refresh = useCallback(
    async (force = false) => {
      if (!projectPath || !api.isTauri()) {
        loadSeq.current += 1;
        setFiles([]);
        setAvailable(false);
        setBranch(null);
        setReason(null);
        setLoading(false);
        hasSnapshot.current = false;
        return;
      }
      const seq = ++loadSeq.current;
      const showSpinner = !hasSnapshot.current;
      if (showSpinner) setLoading(true);
      try {
        const res = await api.gitStatus(projectPath, { force });
        if (seq !== loadSeq.current) return;
        if (!res.available) {
          setFiles([]);
          setAvailable(false);
          setBranch(res.branch ?? null);
          setReason(res.reason ?? "unavailable");
        } else {
          setFiles(normalizeWorkspaceGitEntries(res.files ?? [], projectPath));
          setAvailable(true);
          setBranch(res.branch ?? null);
          setReason(null);
        }
        hasSnapshot.current = true;
      } catch (e) {
        if (seq !== loadSeq.current) return;
        if (!hasSnapshot.current) {
          setFiles([]);
          setAvailable(false);
          setBranch(null);
          setReason(String(e));
        } else {
          onError(localizeUiError(e, locale));
        }
      } finally {
        if (seq === loadSeq.current) setLoading(false);
      }
    },
    // 错误出口按引用捕获会拖垮缓存；调用方保证回调稳定语义即可。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [projectPath, locale],
  );

  // 仅在变更模式可见时同步 Git；工具状态连发防抖后强制读取终态。
  useEffect(() => {
    if (!paneActive || !changesActive) return;
    if (lastSyncRevision.current === syncRevision) {
      void refresh();
      return;
    }
    const timer = window.setTimeout(() => {
      lastSyncRevision.current = syncRevision;
      void refresh(true);
    }, WORKSPACE_SYNC_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [paneActive, changesActive, refresh, syncRevision]);

  const count = useMemo(() => countWorkspaceChangeFiles(files), [files]);
  const filtered = useMemo(
    () => filterWorkspaceGitEntries(files, query),
    [files, query],
  );

  return {
    files,
    loading,
    available,
    reason,
    branch,
    count,
    filtered,
    refresh,
    setFiles,
  };
}
