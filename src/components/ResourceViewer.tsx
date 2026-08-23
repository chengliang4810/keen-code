import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";
/** 右侧资源工作台：多标签、预览、文件树与系统打开菜单。 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import DOMPurify from "dompurify";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { resolvePreviewSrc } from "@/lib/filePreviewSrc";
import { HtmlBrowser } from "@/components/HtmlBrowser";
import { EmbeddedBrowser } from "@/components/EmbeddedBrowser";
import { MarkdownBody } from "@/components/MarkdownBody";
import { OverlayScroll } from "@/components/OverlayScroll";
import { FileMediaPlayer } from "@/components/FileMediaPlayer";
import { ImageUi } from "@/components/ImageUi";
import {
  IconChevronDown,
  IconChevronRight,
  IconClose,
  IconCopy,
  IconEdit,
  IconFileDiff,
  IconFolder,
  IconFiles,
  IconListTree,
  IconSearch,
  IconTerminal,
} from "@/components/icons";
import { OfficeDocumentPreview } from "@/components/OfficeDocumentPreview";
import { CodePreview } from "@/components/CodePreview";
import { StructuredDiffPreview } from "@/components/StructuredDiffPreview";
import { TerminalPanel } from "@/components/TerminalPanel";
import {
  TrajectoryLedger,
  type TrajectoryLiveSource,
} from "@/components/TrajectoryLedger";
import { localizeUiError, type ChatMessage } from "@/lib/session";
import { isOfficeKind } from "@/lib/filePreviewSrc";
import {
  OpenLocationButton,
  type OpenLocationTarget,
} from "@/components/OpenLocationButton";
import { Tip } from "@/components/ui/tooltip";
import { ContextMenu, type ContextMenuItem } from "@/components/ContextMenu";
import { GlassModal } from "@/components/GlassModal";
import type { MessageKey } from "@/i18n";
import {
  buildUnifiedDiff,
  normalizePath,
  pathBaseName,
} from "@/lib/sessionChanges";
import {
  filterWorkspaceGitEntries,
  normalizeWorkspaceGitEntries,
  resolveWorkspaceAbsolutePath,
  workspaceGitKindBadge,
  workspaceGitKindMessageKey,
  type WorkspaceGitFile,
} from "@/lib/workspaceGit";
import {
  defaultResourceEditMode,
  isFsWriteConflict,
  isResourceDraftDirty,
  isResourceTextEditable,
} from "@/lib/resourceEdit";
import {
  loadResourceOpenTarget,
  loadResourceTreeWidth,
  RESOURCE_TREE_WIDTH_DEFAULT,
  RESOURCE_TREE_WIDTH_MAX,
  RESOURCE_TREE_WIDTH_MIN,
  saveResourceOpenTarget,
  saveResourceTreeWidth,
} from "@/lib/resourceViewerPreferences";
import {
  countWorkspaceChangeFiles,
  mergeLoadedTree,
  replaceWorkspaceDirectory,
  type ResourceTreeNode as TreeNode,
} from "@/lib/resourceViewerTree";

function clampTreeWidth(w: number, containerWidth: number): number {
  const maxByContainer = Math.max(
    RESOURCE_TREE_WIDTH_MIN,
    Math.floor(containerWidth * 0.55),
  );
  const max = Math.min(RESOURCE_TREE_WIDTH_MAX, maxByContainer);
  if (!Number.isFinite(w)) return RESOURCE_TREE_WIDTH_DEFAULT;
  return Math.min(
    max,
    Math.max(RESOURCE_TREE_WIDTH_MIN, Math.round(w)),
  );
}

/** 从对话或其他入口请求在资源面板中打开文件、链接或变更。 */
export type ResourceOpenTarget =
  | { type: "file"; path: string; title?: string }
  | { type: "url"; url: string; title?: string }
  /** 打开工作区 Git 变更侧栏。 */
  | { type: "changes"; path?: string }
  /** 打开指定会话的轨迹台账。 */
  | { type: "trajectory"; sessionId: string; title?: string };

export interface ResourceViewerProps {
  projectPath: string | null;
  projectName: string | null;
  locale: Locale;
  onClose?: () => void;
  /** 收到值时打开文件或链接，随后通知请求已消费。 */
  openRequest?: ResourceOpenTarget | null;
  onOpenRequestConsumed?: () => void;
  /** 右侧面板是否显示。 */
  paneActive?: boolean;
  /** Agent 工具状态变化时变化，用于事件驱动同步。 */
  syncRevision?: number;
  /** 当前查看会话的实时轨迹数据源。 */
  trajectoryLive?: TrajectoryLiveSource | null;
  /** 加载非当前会话的持久化轨迹消息。 */
  onLoadTrajectoryMessages?: (sessionId: string) => Promise<ChatMessage[]>;
}

/** 资源侧栏首版可见模式。 */
type SideMode = "files" | "changes" | "terminal" | "trajectory";

/** 工具状态连发时合并 Git 强制刷新的等待时间。 */
const WORKSPACE_SYNC_DEBOUNCE_MS = 200;
/** 工具状态连发时合并文件树刷新的等待时间。 */
const TREE_SYNC_DEBOUNCE_MS = 200;

type DiffViewState = {
  path: string;
  name: string;
  loading: boolean;
  /** 可用的统一差异文本。 */
  unified: string | null;
  /** 无法生成差异时展示的当前完整内容。 */
  afterOnly: string | null;
  error: string | null;
  source: "git" | "head" | "after" | null;
};

/** 同一项目正在执行的文件树刷新。 */
interface TreeRefreshRequest {
  /** 发起刷新时的项目路径。 */
  projectPath: string;
  /** 运行期间是否收到过新的刷新请求。 */
  queued: boolean;
  /** 包含最多一次尾随刷新的共享任务。 */
  promise: Promise<void>;
}

interface FileTab {
  id: string;
  relativePath: string;
  name: string;
  absolutePath: string;
  preview: api.FsReadResult | null;
  mediaSrc: string | null;
  error: string | null;
  loading: boolean;
  /** External URL tab (web page). */
  url?: string;
  tabKind?: "file" | "url";
  /** Editable buffer (text kinds only). */
  draftText?: string | null;
  /** Last loaded/saved text — dirty = draft !== baseline. */
  baselineText?: string | null;
  mtimeMs?: number | null;
  /** true = textarea editor; false = preview (markdown default). */
  editMode?: boolean;
  saving?: boolean;
}

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function baseName(p: string): string {
  const parts = p.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || p;
}

/** 根据当前支持的文档扩展名确定应用内预览类型。 */
function guessOfficeKind(name: string): string {
  const lower = name.toLowerCase();
  if (lower.endsWith(".docx") || lower.endsWith(".docm")) return "docx";
  if (lower.endsWith(".xlsx") || lower.endsWith(".xlsm")) return "xlsx";
  if (lower.endsWith(".pptx") || lower.endsWith(".pptm")) return "pptx";
  return "docx";
}

/** Lightweight file-kind chip for tree rows */
function FileKindMark({ name, isDir }: { name: string; isDir: boolean }) {
  if (isDir) {
    return (
      <span className="rp-kind rp-kind--dir" aria-hidden>
        <IconFolder size={14} />
      </span>
    );
  }
  const lower = name.toLowerCase();
  const ext = lower.includes(".") ? lower.split(".").pop() || "" : "";
  if (ext === "md" || ext === "mdx") {
    return <span className="rp-kind rp-kind--md" aria-hidden>M</span>;
  }
  if (ext === "ts" || ext === "tsx" || ext === "js" || ext === "jsx") {
    return <span className="rp-kind rp-kind--code" aria-hidden>{"{}"}</span>;
  }
  if (ext === "json" || ext === "toml" || ext === "yaml" || ext === "yml") {
    return <span className="rp-kind rp-kind--data" aria-hidden>{"{ }"}</span>;
  }
  if (ext === "gitignore" || lower === ".gitignore") {
    return <span className="rp-kind rp-kind--git" aria-hidden>◆</span>;
  }
  if (["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(ext)) {
    return <span className="rp-kind rp-kind--img" aria-hidden>▣</span>;
  }
  return (
    <span className="rp-kind rp-kind--file" aria-hidden>
      <IconFiles size={13} />
    </span>
  );
}

export function ResourceViewer({
  projectPath,
  projectName,
  locale,
  onClose,
  openRequest,
  onOpenRequestConsumed,
  paneActive = true,
  syncRevision = 0,
  trajectoryLive = null,
  onLoadTrajectoryMessages,
}: ResourceViewerProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [root, setRoot] = useState<TreeNode[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({
    "": true,
  });
  const [tabs, setTabs] = useState<FileTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [loadingTree, setLoadingTree] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [sideMode, setSideMode] = useState<SideMode>("files");
  const [treeWidth, setTreeWidth] = useState(loadResourceTreeWidth);
  const [resizingTree, setResizingTree] = useState(false);
  const splitRef = useRef<HTMLDivElement>(null);
  const [selectedChangePath, setSelectedChangePath] = useState<string | null>(
    null,
  );
  /** Tab id waiting for conflict resolve (reload vs overwrite). */
  const [conflictTabId, setConflictTabId] = useState<string | null>(null);
  /** Close tab while dirty — confirm discard. */
  const [discardTabId, setDiscardTabId] = useState<string | null>(null);
  const [diffView, setDiffView] = useState<DiffViewState | null>(null);
  const diffLoadSeq = useRef(0);
  const treeLoadSeq = useRef(0);
  /** 当前项目共享的文件树刷新任务。 */
  const treeRefreshInFlight = useRef<TreeRefreshRequest | null>(null);
  const workspaceLoadSeq = useRef(0);
  /** 每个未跟踪目录最近一次惰性读取的序号。 */
  const workspaceDirectoryLoadSeq = useRef<Record<string, number>>({});
  /** 最近一次已经纳入文件树查询的工具同步版本。 */
  const treeSyncRevision = useRef(syncRevision);
  /** 最近一次已经纳入 Git 状态查询的工具同步版本。 */
  const workspaceSyncRevision = useRef(syncRevision);
  /** 当前项目是否已有可展示的文件树快照。 */
  const treeHasSnapshot = useRef(false);
  /** 当前项目是否已有可展示的 Git 状态快照。 */
  const workspaceHasSnapshot = useRef(false);
  const snapshotProjectPath = useRef(projectPath);
  if (snapshotProjectPath.current !== projectPath) {
    snapshotProjectPath.current = projectPath;
    treeLoadSeq.current += 1;
    workspaceLoadSeq.current += 1;
    treeHasSnapshot.current = false;
    workspaceHasSnapshot.current = false;
    treeSyncRevision.current = syncRevision;
    workspaceSyncRevision.current = syncRevision;
  }
  /** 当前项目的 Git 工作区状态。 */
  const [workspaceFiles, setWorkspaceFiles] = useState<WorkspaceGitFile[]>([]);
  const [workspaceLoading, setWorkspaceLoading] = useState(false);
  const [workspaceAvailable, setWorkspaceAvailable] = useState(false);
  const [workspaceReason, setWorkspaceReason] = useState<string | null>(null);
  const [workspaceBranch, setWorkspaceBranch] = useState<string | null>(null);
  /** 当前正在读取的未跟踪目录。 */
  const [loadingWorkspaceDirectories, setLoadingWorkspaceDirectories] =
    useState<Record<string, boolean>>({});
  const [pathCopyFlash, setPathCopyFlash] = useState(false);
  /** 打开位置按钮当前使用的系统目标。 */
  const [openWithTarget, setOpenWithTarget] =
    useState<OpenLocationTarget>(loadResourceOpenTarget);
  /** 轨迹模式当前展示的会话；为空时跟随当前查看的会话。 */
  const [trajectorySessionId, setTrajectorySessionId] = useState<string | null>(
    null,
  );
  const [trajectorySessionTitle, setTrajectorySessionTitle] = useState<
    string | null
  >(null);

  const activeTab = tabs.find((t) => t.id === activeId) ?? null;
  const workspaceCount = countWorkspaceChangeFiles(workspaceFiles);
  const totalChangeBadge = workspaceCount;
  const filteredWorkspace = useMemo(
    () => filterWorkspaceGitEntries(workspaceFiles, query),
    [workspaceFiles, query],
  );

  /** 读取 Git 状态；工具完成后的刷新可跳过短时缓存。 */
  const refreshWorkspaceStatus = useCallback(async (force = false) => {
    if (!projectPath || !api.isTauri()) {
      workspaceLoadSeq.current += 1;
      setWorkspaceFiles([]);
      setWorkspaceAvailable(false);
      setWorkspaceBranch(null);
      setWorkspaceReason(null);
      setWorkspaceLoading(false);
      workspaceHasSnapshot.current = false;
      return;
    }
    const seq = ++workspaceLoadSeq.current;
    const showSpinner = !workspaceHasSnapshot.current;
    if (showSpinner) setWorkspaceLoading(true);
    try {
      const res = await api.gitStatus(projectPath, { force });
      if (seq !== workspaceLoadSeq.current) return;
      if (!res.available) {
        setWorkspaceFiles([]);
        setWorkspaceAvailable(false);
        setWorkspaceBranch(res.branch ?? null);
        setWorkspaceReason(res.reason ?? "unavailable");
      } else {
        setWorkspaceFiles(
          normalizeWorkspaceGitEntries(res.files ?? [], projectPath),
        );
        setWorkspaceAvailable(true);
        setWorkspaceBranch(res.branch ?? null);
        setWorkspaceReason(null);
      }
      workspaceHasSnapshot.current = true;
    } catch (e) {
      if (seq !== workspaceLoadSeq.current) return;
      if (!workspaceHasSnapshot.current) {
        setWorkspaceFiles([]);
        setWorkspaceAvailable(false);
        setWorkspaceBranch(null);
        setWorkspaceReason(String(e));
      } else {
        setError(localizeUiError(e, locale));
      }
    } finally {
      if (seq === workspaceLoadSeq.current) setWorkspaceLoading(false);
    }
  }, [projectPath]);

  // 仅在变更模式可见时同步 Git；工具状态连发防抖后强制读取终态。
  useEffect(() => {
    if (!paneActive || sideMode !== "changes") return;
    if (workspaceSyncRevision.current === syncRevision) {
      void refreshWorkspaceStatus();
      return;
    }
    const timer = window.setTimeout(() => {
      workspaceSyncRevision.current = syncRevision;
      void refreshWorkspaceStatus(true);
    }, WORKSPACE_SYNC_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [paneActive, refreshWorkspaceStatus, sideMode, syncRevision]);

  // Git 状态中不再存在所选路径时清空差异预览。
  useEffect(() => {
    if (!selectedChangePath) return;
    const n = normalizePath(selectedChangePath);
    const inWorkspace = workspaceFiles.some(
      (c) =>
        normalizePath(c.path) === n ||
        normalizePath(c.absolutePath) === n,
    );
    if (!inWorkspace) {
      setSelectedChangePath(null);
      setDiffView(null);
    }
  }, [workspaceFiles, selectedChangePath]);

  /** 按需用 Git 过滤后的实际文件替换目录占位项，不进入文件 Diff 请求链。 */
  const expandWorkspaceDirectory = useCallback(
    async (entry: WorkspaceGitFile) => {
      const key = normalizePath(entry.path);
      if (!entry.isDirectory || !key || !projectPath || !api.isTauri()) return;
      if (entry.isNestedRepository) {
        try {
          await api.pathReveal(entry.absolutePath || key);
        } catch (revealError) {
          setError(localizeUiError(revealError, locale));
        }
        return;
      }
      if (loadingWorkspaceDirectories[key]) return;

      const seq = (workspaceDirectoryLoadSeq.current[key] ?? 0) + 1;
      workspaceDirectoryLoadSeq.current[key] = seq;
      diffLoadSeq.current += 1;
      setSelectedChangePath(null);
      setDiffView(null);
      setLoadingWorkspaceDirectories((prev) => ({ ...prev, [key]: true }));
      try {
        const result = await api.gitUntrackedDirectory(projectPath, key);
        if (
          workspaceDirectoryLoadSeq.current[key] !== seq ||
          snapshotProjectPath.current !== projectPath
        ) {
          return;
        }
        const files = normalizeWorkspaceGitEntries(result.files, projectPath);
        setWorkspaceFiles((prev) =>
          replaceWorkspaceDirectory(prev, key, files),
        );
        if (result.truncated) {
          setError(
            tr("changes.workspace.directoryTruncated", { count: 2000 }),
          );
        }
      } catch (loadError) {
        if (workspaceDirectoryLoadSeq.current[key] === seq) {
          setError(localizeUiError(loadError, locale));
        }
      } finally {
        if (workspaceDirectoryLoadSeq.current[key] === seq) {
          setLoadingWorkspaceDirectories((prev) => ({
            ...prev,
            [key]: false,
          }));
        }
      }
    },
    [loadingWorkspaceDirectories, projectPath, tr],
  );

  const loadWorkspaceDiff = useCallback(
    async (entry: WorkspaceGitFile) => {
      if (entry.isDirectory) {
        await expandWorkspaceDirectory(entry);
        return;
      }
      const abs =
        normalizePath(entry.absolutePath) ||
        resolveWorkspaceAbsolutePath(projectPath, entry.path);
      const path = abs || normalizePath(entry.path);
      if (!path) return;
      const seq = ++diffLoadSeq.current;
      setSelectedChangePath(path);
      setDiffView({
        path,
        name: entry.name || pathBaseName(path),
        loading: true,
        unified: null,
        afterOnly: null,
        error: null,
        source: null,
      });

      const relName = entry.path || pathBaseName(path);

      // Prefer git unified diff for workspace rows
      if (projectPath && api.isTauri()) {
        try {
          const g = await api.gitFileDiff(projectPath, path);
          if (seq !== diffLoadSeq.current) return;
          if (g.available && g.diff?.trim()) {
            setDiffView({
              path,
              name: entry.name || pathBaseName(path),
              loading: false,
              unified: g.diff,
              afterOnly: null,
              error: null,
              source: "git",
            });
            return;
          }
        } catch {
          /* soft-fail */
        }

        // HEAD + working tree for local unified when porcelain has no unified text
        try {
          const [head, cur] = await Promise.all([
            api.gitShowFile(projectPath, path).catch(() => null),
            api.fsOpenPath(path, projectPath).catch(() => null),
          ]);
          if (seq !== diffLoadSeq.current) return;
          const afterText = cur?.text ?? null;
          if (head?.available && typeof head.content === "string" && afterText != null) {
            const unified = buildUnifiedDiff(relName, head.content, afterText);
            setDiffView({
              path,
              name: entry.name || pathBaseName(path),
              loading: false,
              unified,
              afterOnly: null,
              error: null,
              source: "head",
            });
            return;
          }
          if (afterText != null) {
            // Untracked / new: show full file as after-only
            setDiffView({
              path,
              name: entry.name || pathBaseName(path),
              loading: false,
              unified:
                entry.kind === "untracked" || entry.kind === "added"
                  ? buildUnifiedDiff(relName, "", afterText)
                  : null,
              afterOnly:
                entry.kind === "untracked" || entry.kind === "added"
                  ? null
                  : afterText,
              error: null,
              source:
                entry.kind === "untracked" || entry.kind === "added"
                  ? "git"
                  : "after",
            });
            return;
          }
        } catch {
          /* soft-fail */
        }
      }

      if (seq !== diffLoadSeq.current) return;
      setDiffView({
        path,
        name: entry.name || pathBaseName(path),
        loading: false,
        unified: null,
        afterOnly: null,
        error: null,
        source: null,
      });
    },
    [expandWorkspaceDirectory, projectPath],
  );

  const revealChangePath = useCallback(async (path: string) => {
    if (!path || !api.isTauri()) return;
    try {
      await api.pathReveal(path);
    } catch (e) {
      setError(localizeUiError(e, locale));
    }
  }, []);

  const copyChangePath = useCallback(async (path: string) => {
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      setPathCopyFlash(true);
      window.setTimeout(() => setPathCopyFlash(false), 1200);
    } catch (e) {
      setError(localizeUiError(e, locale));
    }
  }, []);

  const workspaceKindLabel = useCallback(
    (kind: string) =>
      tr(workspaceGitKindMessageKey(kind) as MessageKey),
    [tr],
  );

  const workspaceUnavailableLabel = useCallback(() => {
    const r = (workspaceReason || "").toLowerCase();
    if (r.includes("not a git") || r.includes("not a git repository")) {
      return tr("changes.workspace.noRepo");
    }
    if (r.includes("git not available") || r.includes("not available")) {
      return tr("changes.workspace.noGit");
    }
    return tr("changes.workspace.unavailable");
  }, [tr, workspaceReason]);

  // Drag-resize left navigator | right preview split
  useEffect(() => {
    if (!resizingTree) return;
    const onMove = (e: PointerEvent) => {
      const box = splitRef.current?.getBoundingClientRect();
      if (!box) return;
      const next = clampTreeWidth(e.clientX - box.left, box.width);
      setTreeWidth(next);
    };
    const onUp = () => {
      setResizingTree(false);
      setTreeWidth((w) => {
        try {
          saveResourceTreeWidth(w);
        } catch (persistError) {
          setError(localizeUiError(persistError, locale));
        }
        return w;
      });
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [resizingTree]);

  const loadDir = useCallback(
    async (relative: string): Promise<TreeNode[]> => {
      if (!projectPath) return [];
      if (!api.isTauri()) throw new Error("Tauri required");
      const entries = await api.fsListDir(projectPath, relative);
      return entries.map((e) => ({
        name: e.name,
        relativePath: e.relativePath,
        isDir: e.isDir,
        size: e.size,
        ext: e.ext,
        children: e.isDir ? [] : undefined,
        loaded: !e.isDir,
      }));
    },
    [projectPath],
  );

  /** 合并同项目的并发刷新，并在运行期间有新请求时只补一次尾随刷新。 */
  const refresh = useCallback((): Promise<void> => {
    if (!projectPath) {
      treeLoadSeq.current += 1;
      setRoot([]);
      treeHasSnapshot.current = false;
      return Promise.resolve();
    }

    const existing = treeRefreshInFlight.current;
    if (existing?.projectPath === projectPath) {
      existing.queued = true;
      return existing.promise;
    }

    const request: TreeRefreshRequest = {
      projectPath,
      queued: false,
      promise: Promise.resolve(),
    };
    const run = async () => {
      do {
        request.queued = false;
        const seq = ++treeLoadSeq.current;
        const showSpinner = !treeHasSnapshot.current;
        if (showSpinner) setLoadingTree(true);
        setError(null);
        try {
          const next = await loadDir("");
          if (
            seq !== treeLoadSeq.current ||
            snapshotProjectPath.current !== projectPath
          ) {
            return;
          }
          setRoot((prev) =>
            treeHasSnapshot.current ? mergeLoadedTree(prev, next) : next,
          );
          treeHasSnapshot.current = true;
        } catch (refreshError) {
          if (seq !== treeLoadSeq.current) return;
          setError(localizeUiError(refreshError, locale));
          if (!treeHasSnapshot.current) setRoot([]);
        } finally {
          if (seq === treeLoadSeq.current && showSpinner) {
            setLoadingTree(false);
          }
        }
      } while (
        request.queued && snapshotProjectPath.current === projectPath
      );
    };
    request.promise = run().finally(() => {
      if (treeRefreshInFlight.current === request) {
        treeRefreshInFlight.current = null;
      }
    });
    treeRefreshInFlight.current = request;
    return request.promise;
  }, [loadDir, projectPath]);

  useEffect(() => {
    setRoot([]);
    setTabs([]);
    setActiveId(null);
    setExpanded({ "": true });
    setQuery("");
    setWorkspaceFiles([]);
    setWorkspaceAvailable(false);
    setWorkspaceBranch(null);
    setWorkspaceReason(null);
    setLoadingWorkspaceDirectories({});
    workspaceDirectoryLoadSeq.current = {};
    treeHasSnapshot.current = false;
    workspaceHasSnapshot.current = false;
  }, [projectPath]);

  // 仅在文件模式可见时同步文件树；工具状态连发防抖并由 refresh 合并并发。
  useEffect(() => {
    if (!paneActive || sideMode !== "files" || !projectPath) return;
    if (treeSyncRevision.current === syncRevision) {
      void refresh();
      return;
    }
    const timer = window.setTimeout(() => {
      treeSyncRevision.current = syncRevision;
      void refresh();
    }, TREE_SYNC_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [paneActive, projectPath, refresh, sideMode, syncRevision]);

  const toggleDir = async (node: TreeNode) => {
    const key = node.relativePath;
    const willOpen = !expanded[key];
    setExpanded((ex) => ({ ...ex, [key]: willOpen }));
    if (willOpen && !node.loaded) {
      try {
        const children = await loadDir(node.relativePath);
        const patch = (list: TreeNode[]): TreeNode[] =>
          list.map((n) => {
            if (n.relativePath === key) return { ...n, children, loaded: true };
            if (n.children) return { ...n, children: patch(n.children) };
            return n;
          });
        setRoot((r) => patch(r));
      } catch (e) {
        setError(localizeUiError(e, locale));
      }
    }
  };

  const applyReadResult = (
    id: string,
    r: api.FsReadResult,
    src: string | null,
    relativePath: string,
  ) => {
    const editable = isResourceTextEditable({
      kind: r.kind,
      text: r.text,
      truncated: r.truncated,
      error: r.error,
    });
    const text = r.text ?? null;
    setTabs((prev) =>
      prev.map((t) =>
        t.id === id
          ? {
              ...t,
              preview: r,
              mediaSrc: src,
              absolutePath: r.absolutePath || "",
              relativePath: relativePath || r.relativePath || t.relativePath,
              name: r.name || baseName(relativePath || r.absolutePath || "file"),
              loading: false,
              tabKind: "file" as const,
              draftText: editable ? text : null,
              baselineText: editable ? text : null,
              mtimeMs: typeof r.mtimeMs === "number" ? r.mtimeMs : null,
              editMode: editable ? defaultResourceEditMode(r.kind) : false,
              saving: false,
            }
          : t,
      ),
    );
  };

  const activeTabEditable = useMemo(() => {
    if (!activeTab?.preview || activeTab.tabKind === "url") return false;
    return isResourceTextEditable({
      kind: activeTab.preview.kind,
      text: activeTab.baselineText ?? activeTab.preview.text,
      truncated: activeTab.preview.truncated,
      error: activeTab.preview.error,
    });
  }, [activeTab]);

  const updateActiveDraft = useCallback((text: string) => {
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeId ? { ...t, draftText: text } : t,
      ),
    );
  }, [activeId]);

  const revertActiveDraft = useCallback(() => {
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeId && t.baselineText != null
          ? { ...t, draftText: t.baselineText }
          : t,
      ),
    );
  }, [activeId]);

  const toggleActiveEditMode = useCallback(() => {
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeId ? { ...t, editMode: !t.editMode } : t,
      ),
    );
  }, [activeId]);

  const reloadActiveFile = useCallback(async () => {
    const tab = tabs.find((t) => t.id === activeId);
    if (!tab || tab.tabKind === "url" || !api.isTauri()) return;
    setTabs((prev) =>
      prev.map((t) =>
        t.id === tab.id ? { ...t, loading: true, error: null } : t,
      ),
    );
    try {
      let r: api.FsReadResult;
      if (projectPath && tab.relativePath && !tab.relativePath.startsWith("/") && !/^[A-Za-z]:[\\/]/.test(tab.relativePath)) {
        r = await api.fsReadFile(projectPath, tab.relativePath);
      } else if (tab.absolutePath) {
        r = await api.fsReadAbsolute(tab.absolutePath);
      } else {
        r = await api.fsOpenPath(tab.relativePath, projectPath);
      }
      const src = await resolvePreviewSrc(r);
      applyReadResult(tab.id, r, src, tab.relativePath);
    } catch (e) {
      setTabs((prev) =>
        prev.map((t) =>
          t.id === tab.id
            ? {
                ...t,
                loading: false,
                error: `${tr("resources.openFailed")}: ${String(e)}`,
              }
            : t,
        ),
      );
    }
  }, [activeId, projectPath, tabs, tr]);

  const saveActiveFile = useCallback(
    async (opts?: { force?: boolean }) => {
      const tab = tabs.find((t) => t.id === activeId);
      if (!tab || tab.tabKind === "url" || tab.draftText == null) return;
      if (!api.isTauri()) {
        setError(tr("resources.saveFailed"));
        return;
      }
      if (!isResourceDraftDirty(tab.draftText, tab.baselineText) && !opts?.force) {
        return;
      }
      setTabs((prev) =>
        prev.map((t) =>
          t.id === tab.id ? { ...t, saving: true, error: null } : t,
        ),
      );
      setError(null);
      try {
        const expected = opts?.force ? null : tab.mtimeMs ?? null;
        const underProject =
          !!projectPath &&
          tab.relativePath &&
          !tab.relativePath.startsWith("/") &&
          !/^[A-Za-z]:[\\/]/.test(tab.relativePath) &&
          (tab.absolutePath
            ? normalizePath(tab.absolutePath).startsWith(
                normalizePath(projectPath) + "/",
              ) ||
              normalizePath(tab.absolutePath) === normalizePath(projectPath)
            : true);

        let w: api.FsWriteResult;
        if (underProject && projectPath) {
          w = await api.fsWriteFile(
            projectPath,
            tab.relativePath,
            tab.draftText,
            expected,
          );
        } else if (tab.absolutePath) {
          w = await api.fsWriteAbsolute(
            tab.absolutePath,
            tab.draftText,
            expected,
          );
        } else {
          throw new Error(tr("resources.saveNoPath"));
        }

        const savedText = tab.draftText ?? "";
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tab.id
              ? {
                  ...t,
                  saving: false,
                  baselineText: savedText,
                  draftText: savedText,
                  mtimeMs: w.mtimeMs,
                  absolutePath: w.absolutePath || t.absolutePath,
                  preview: t.preview
                    ? {
                        ...t.preview,
                        text: savedText,
                        size: w.size,
                        mtimeMs: w.mtimeMs,
                        truncated: false,
                      }
                    : t.preview,
                }
              : t,
          ),
        );
      } catch (e) {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tab.id ? { ...t, saving: false } : t,
          ),
        );
        if (isFsWriteConflict(e)) {
          setConflictTabId(tab.id);
        } else {
          setError(localizeUiError(e, locale));
        }
      }
    },
    [activeId, projectPath, tabs, tr],
  );

  const openFile = async (relativePath: string) => {
    if (!projectPath) {
      setError(tr("main.noProject"));
      return;
    }
    if (!api.isTauri()) {
      setError(tr("resources.openFailed"));
      return;
    }
    const existing = tabs.find(
      (t) => t.tabKind !== "url" && t.relativePath === relativePath,
    );
    if (existing) {
      setTabs((prev) => {
        const hit = prev.find((t) => t.id === existing.id);
        if (!hit) return prev;
        return [hit, ...prev.filter((t) => t.id !== existing.id)];
      });
      setActiveId(existing.id);
      return;
    }
    const id = `tab_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
    const tab: FileTab = {
      id,
      relativePath,
      name: baseName(relativePath),
      absolutePath: "",
      preview: null,
      mediaSrc: null,
      error: null,
      loading: true,
      tabKind: "file",
    };
    // Newest tab on the left
    setTabs((prev) => [tab, ...prev]);
    setActiveId(id);
    try {
      const r = await api.fsReadFile(projectPath, relativePath);
      const src = await resolvePreviewSrc(r);
      applyReadResult(id, r, src, relativePath);
    } catch (e) {
      setTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? {
                ...t,
                loading: false,
                error: `${tr("resources.openFailed")}: ${String(e)}`,
              }
            : t,
        ),
      );
    }
  };

  /**
   * Open path from chat cards. Uses smart host resolver:
   * absolute → project-relative → suffix search under project root
   * (handles monorepo: agent writes `05-handoff/next.md` under a subfolder).
   */
  const openAbsoluteFile = useCallback(
    async (absolutePath: string, title?: string) => {
      if (!api.isTauri()) {
        setError(tr("resources.openFailed"));
        return;
      }
      const norm = absolutePath.trim();
      if (!norm) return;
      const existing = tabs.find(
        (t) =>
          t.tabKind !== "url" &&
          (t.absolutePath === norm || t.relativePath === norm),
      );
      if (existing) {
        // Move existing to front + activate (Chrome-like focus)
        setTabs((prev) => {
          const hit = prev.find((t) => t.id === existing.id);
          if (!hit) return prev;
          return [hit, ...prev.filter((t) => t.id !== existing.id)];
        });
        setActiveId(existing.id);
        return;
      }
      const id = `tab_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
      const tab: FileTab = {
        id,
        relativePath: norm,
        name: title || baseName(norm),
        absolutePath: norm,
        preview: null,
        mediaSrc: null,
        error: null,
        loading: true,
        tabKind: "file",
      };
      setTabs((prev) => [tab, ...prev]);
      setActiveId(id);
      try {
        const r = await api.fsOpenPath(norm, projectPath);
        const src = await resolvePreviewSrc(r);
        // Prefer project-relative tab key when file is under project
        let relKey = r.relativePath || baseName(norm);
        if (projectPath && r.absolutePath) {
          const root = projectPath.replace(/[/\\]+$/, "").replace(/\\/g, "/");
          const absN = r.absolutePath.replace(/\\/g, "/");
          if (absN.startsWith(root + "/")) {
            relKey = absN.slice(root.length + 1);
          }
        }
        applyReadResult(id, r, src, relKey);
      } catch (e) {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === id
              ? {
                  ...t,
                  loading: false,
                  error: `${tr("resources.openFailed")}: ${String(e)}`,
                }
              : t,
          ),
        );
      }
    },
    [projectPath, tabs, tr],
  );

  /** 从变更预览直接切回文件模式并打开当前工作区文件。 */
  const openCurrentChangeFile = useCallback(
    (path: string, name: string) => {
      setSideMode("files");
      void openAbsoluteFile(path, name);
    },
    [openAbsoluteFile],
  );

  const openUrl = useCallback(
    (url: string, title?: string) => {
      const u = url.trim();
      if (!u) return;
      const existing = tabs.find((t) => t.tabKind === "url" && t.url === u);
      if (existing) {
        setActiveId(existing.id);
        return;
      }
      const id = `tab_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
      let name = title || u;
      try {
        name = title || new URL(u).hostname || u;
      } catch {
        /* keep */
      }
      const tab: FileTab = {
        id,
        relativePath: u,
        name,
        absolutePath: "",
        preview: null,
        mediaSrc: null,
        error: null,
        loading: false,
        url: u,
        tabKind: "url",
      };
      setTabs((prev) => [tab, ...prev]);
      setActiveId(id);
    },
    [tabs],
  );

  /** 从工具时间线直接定位文件，并在变更面板加载该文件的 Git Diff。 */
  const openChangeDiff = useCallback(
    (path: string) => {
      const normalized = normalizePath(path);
      if (!normalized) return;
      const matched = workspaceFiles.find(
        (entry) =>
          normalizePath(entry.path) === normalized ||
          normalizePath(entry.absolutePath) === normalized,
      );
      const relativePath =
        matched?.path ||
        (projectPath && normalized.startsWith(`${normalizePath(projectPath)}/`)
          ? normalized.slice(normalizePath(projectPath).length + 1)
          : normalized);
      const entry: WorkspaceGitFile = matched || {
        path: relativePath,
        absolutePath:
          resolveWorkspaceAbsolutePath(projectPath, relativePath) || normalized,
        status: " M",
        indexStatus: " ",
        worktreeStatus: "M",
        kind: "modified",
        name: pathBaseName(normalized),
        isDirectory: false,
        isNestedRepository: false,
      };
      void loadWorkspaceDiff(entry);
    },
    [loadWorkspaceDiff, projectPath, workspaceFiles],
  );

  // 处理来自对话中文件、链接或变更卡片的打开请求。
  useEffect(() => {
    if (!openRequest) return;
    if (openRequest.type === "file") {
      setSideMode("files");
      void openAbsoluteFile(openRequest.path, openRequest.title);
    } else if (openRequest.type === "url") {
      setSideMode("files");
      openUrl(openRequest.url, openRequest.title);
    } else if (openRequest.type === "changes") {
      setSideMode("changes");
      if (openRequest.path) {
        openChangeDiff(openRequest.path);
      }
    } else if (openRequest.type === "trajectory") {
      setSideMode("trajectory");
      setTrajectorySessionId(openRequest.sessionId);
      setTrajectorySessionTitle(openRequest.title ?? null);
    }
    onOpenRequestConsumed?.();
  }, [
    openRequest,
    openChangeDiff,
    openAbsoluteFile,
    openUrl,
    onOpenRequestConsumed,
  ]);

  // 切换查看的会话后，轨迹台账跟随新会话；清除菜单固定的目标会话。
  useEffect(() => {
    setTrajectorySessionId(null);
    setTrajectorySessionTitle(null);
  }, [trajectoryLive?.sessionId]);

  /** Last tab gone → collapse the right pane (user can still re-open it manually). */
  const closePaneIfNoTabs = useCallback(
    (remaining: number) => {
      if (remaining === 0) onClose?.();
    },
    [onClose],
  );

  const closeTabForced = useCallback(
    (id: string) => {
      let remaining = -1;
      setTabs((prev) => {
        const idx = prev.findIndex((t) => t.id === id);
        if (idx < 0) {
          remaining = prev.length;
          return prev;
        }
        const next = prev.filter((t) => t.id !== id);
        remaining = next.length;
        if (activeId === id) {
          // Prefer neighbor on the left (newer), else right
          const neighbor = next[Math.max(0, idx - 1)] ?? next[0] ?? null;
          setActiveId(neighbor?.id ?? null);
        }
        return next;
      });
      if (remaining === 0) closePaneIfNoTabs(0);
    },
    [activeId, closePaneIfNoTabs],
  );

  const closeTab = useCallback(
    (id: string) => {
      const tab = tabs.find((t) => t.id === id);
      if (tab && isResourceDraftDirty(tab.draftText, tab.baselineText)) {
        setDiscardTabId(id);
        return;
      }
      closeTabForced(id);
    },
    [closeTabForced, tabs],
  );

  /** Chrome-style: close every tab except `id`. */
  const closeOtherTabs = useCallback(
    (id: string) => {
      setTabs((prev) => prev.filter((t) => t.id === id));
      setActiveId(id);
    },
    [],
  );

  /** Close tabs visually to the right of `id` (higher index; older tabs). */
  const closeTabsToRight = useCallback(
    (id: string) => {
      let remaining = -1;
      setTabs((prev) => {
        const idx = prev.findIndex((t) => t.id === id);
        if (idx < 0) {
          remaining = prev.length;
          return prev;
        }
        const next = prev.slice(0, idx + 1);
        remaining = next.length;
        if (activeId && !next.some((t) => t.id === activeId)) {
          setActiveId(id);
        }
        return next;
      });
      if (remaining === 0) closePaneIfNoTabs(0);
    },
    [activeId, closePaneIfNoTabs],
  );

  /** Close tabs visually to the left of `id` (lower index; newer tabs). */
  const closeTabsToLeft = useCallback(
    (id: string) => {
      let remaining = -1;
      setTabs((prev) => {
        const idx = prev.findIndex((t) => t.id === id);
        if (idx < 0) {
          remaining = prev.length;
          return prev;
        }
        const next = prev.slice(idx);
        remaining = next.length;
        if (activeId && !next.some((t) => t.id === activeId)) {
          setActiveId(id);
        }
        return next;
      });
      if (remaining === 0) closePaneIfNoTabs(0);
    },
    [activeId, closePaneIfNoTabs],
  );

  const closeAllTabs = useCallback(() => {
    setTabs([]);
    setActiveId(null);
    closePaneIfNoTabs(0);
  }, [closePaneIfNoTabs]);

  const [tabMenu, setTabMenu] = useState<{
    x: number;
    y: number;
    tabId: string;
  } | null>(null);

  const absPath =
    (diffView && sideMode === "changes" ? diffView.path : "") ||
    activeTab?.absolutePath ||
    "";
  /** 已选统一 Diff 当前是否显示；隐藏时仍保留其组件实例。 */
  const persistentDiffVisible =
    sideMode === "changes" && Boolean(diffView?.unified);

  const filterMatch = useCallback(
    (name: string, path: string) => {
      const q = query.trim().toLowerCase();
      if (!q) return true;
      return name.toLowerCase().includes(q) || path.toLowerCase().includes(q);
    },
    [query],
  );

  const renderTree = (nodes: TreeNode[], depth: number): ReactNode =>
    nodes
      .filter((n) => filterMatch(n.name, n.relativePath) || n.isDir)
      .map((n) => {
        const isOpen = !!expanded[n.relativePath];
        const active = activeTab?.relativePath === n.relativePath;
        return (
          <div key={n.relativePath || n.name}>
            <Tip label={n.relativePath}>
              <Button
                type="button"
                className={
                  "rp-tree__row" +
                  (active ? " is-active" : "") +
                  (n.isDir ? " is-dir" : "")
                }
                style={{ paddingLeft: 8 + depth * 12 }}
                onClick={(e) => {
                  e.preventDefault();
                  if (n.isDir) void toggleDir(n);
                  else void openFile(n.relativePath);
                }}
              >
                <span className={"rp-tree__chev" + (n.isDir && isOpen ? " is-open" : "")}>
                  {n.isDir ? (
                    <IconChevronDown size={12} className="chevron--disclose" />
                  ) : (
                    <span className="rp-tree__gap" />
                  )}
                </span>
                <FileKindMark name={n.name} isDir={n.isDir} />
                <span className="rp-tree__name">{n.name}</span>
              </Button>
            </Tip>
            {n.isDir && isOpen && n.children && n.children.length > 0 && (
              <div className="rp-tree__kids">
                {renderTree(n.children, depth + 1)}
              </div>
            )}
          </div>
        );
      });

  /** 渲染工作区变更；目录占位项按需替换为 Git 过滤后的实际文件。 */
  const renderWorkspaceRows = (entries: WorkspaceGitFile[]): ReactNode =>
    entries.map((entry) => {
      const key = normalizePath(entry.path);
      const abs =
        normalizePath(entry.absolutePath) ||
        resolveWorkspaceAbsolutePath(projectPath, entry.path);
      const active =
        !entry.isDirectory &&
        selectedChangePath != null &&
        (normalizePath(selectedChangePath) === abs ||
          normalizePath(selectedChangePath) === key);
      const directoryLoading =
        entry.isDirectory && Boolean(loadingWorkspaceDirectories[key]);
      return (
        <div
          key={`ws:${key}`}
          className={"rp-changes-row" + (active ? " is-active" : "")}
          role="listitem"
        >
          <Button
            type="button"
            className="rp-changes-row__main"
            title={abs || entry.path}
            disabled={directoryLoading}
            aria-expanded={
              entry.isDirectory && !entry.isNestedRepository
                ? false
                : undefined
            }
            onClick={() => void loadWorkspaceDiff(entry)}
          >
            <span
              className={
                "rp-changes-badge rp-changes-badge--" + entry.kind
              }
              aria-hidden
            >
              {entry.isDirectory ? (
                directoryLoading ? (
                  "…"
                ) : entry.isNestedRepository ? (
                  <IconFolder size={12} />
                ) : (
                  <IconChevronRight size={12} />
                )
              ) : (
                workspaceGitKindBadge(entry.kind)
              )}
            </span>
            <span className="rp-changes-row__meta">
              <span className="rp-changes-row__name">{entry.name}</span>
              <span className="rp-changes-row__path">{entry.path}</span>
              <span className="rp-changes-row__kind">
                {workspaceKindLabel(entry.kind)}
                {entry.status.trim() ? ` · ${entry.status}` : ""}
              </span>
            </span>
          </Button>
          <div className="rp-changes-row__actions">
            <Tip label={tr("changes.reveal")}>
              <Button
                type="button"
                className="chrome-btn"
                onClick={(event) => {
                  event.stopPropagation();
                  void revealChangePath(abs || entry.path);
                }}
              >
                <IconFolder size={13} />
              </Button>
            </Tip>
            <Tip label={tr("changes.copyPath")}>
              <Button
                type="button"
                className="chrome-btn"
                onClick={(event) => {
                  event.stopPropagation();
                  void copyChangePath(abs || entry.path);
                }}
              >
                <IconCopy size={13} />
              </Button>
            </Tip>
          </div>
        </div>
      );
    });

  const previewBody = useMemo(() => {
    // 变更模式选中文件后，以 Git 工作区差异替换普通文件预览。
    if (sideMode === "changes" && diffView && !diffView.unified) {
      if (diffView.loading) {
        return (
          <div className="rp-preview__msg">{tr("changes.loadingDiff")}</div>
        );
      }
      if (diffView.afterOnly) {
        return (
          <CodePreview
            code={diffView.afterOnly}
            fileName={diffView.name}
            footer={tr("changes.afterOnly")}
          />
        );
      }
      return (
        <div className="rp-changes-empty">
          <div className="rp-changes-empty__title">{tr("changes.noDiff")}</div>
          <div className="rp-changes-empty__hint">{tr("changes.noDiffHint")}</div>
          <div className="rp-changes-empty__actions">
            <Button
              type="button"
              className="rp-tool-btn"
              onClick={() => void revealChangePath(diffView.path)}
            >
              <IconFolder size={14} />
              <span className="rp-tool-btn__label">{tr("changes.reveal")}</span>
            </Button>
            <Button
              type="button"
              className="rp-tool-btn"
              onClick={() => void copyChangePath(diffView.path)}
            >
              <IconCopy size={14} />
              <span className="rp-tool-btn__label">
                {pathCopyFlash
                  ? tr("changes.pathCopied")
                  : tr("changes.copyPath")}
              </span>
            </Button>
          </div>
        </div>
      );
    }

    // URL tabs render via EmbeddedBrowser below (native Webview host).
    // Keep other kinds here so useMemo deps stay correct.
    if (activeTab?.tabKind === "url" && activeTab.url) {
      return null;
    }
    const preview = activeTab?.preview;
    if (!preview) {
      if (activeTab?.error) {
        return <div className="rp-preview__msg">{activeTab.error}</div>;
      }
      return null;
    }
    if (preview.error && !preview.text && !preview.base64 && !preview.stream) {
      return <div className="rp-preview__msg">{preview.error}</div>;
    }
    const mediaSrc = activeTab?.mediaSrc ?? null;
    const dataUrl =
      preview.base64 && preview.mime
        ? `data:${preview.mime};base64,${preview.base64}`
        : null;
    const src = mediaSrc || dataUrl;

    // Text editor shell: full-height pane + in-content toolbar (not chrome).
    // Markdown defaults to preview; other editable kinds open the source editor.
    const canEdit = isResourceTextEditable({
      kind: preview.kind,
      text: activeTab?.baselineText ?? preview.text,
      truncated: preview.truncated,
      error: preview.error,
    });
    if (canEdit && activeTab && activeTab.draftText != null) {
      const draftText = activeTab.draftText;
      const isMarkdown = preview.kind === "markdown";
      const showEditor = activeTab.editMode || !isMarkdown;
      const dirty = isResourceDraftDirty(draftText, activeTab.baselineText);
      return (
        <div className="rp-editor">
          <div
            className="rp-editor__toolbar"
            role="toolbar"
            aria-label={tr("resources.editorToolbar")}
          >
            {isMarkdown ? (
              <Tip
                label={
                  activeTab.editMode
                    ? tr("resources.previewMode")
                    : tr("resources.editMode")
                }
              >
                <Button
                  type="button"
                  className={
                    "rp-editor__tool-btn" +
                    (activeTab.editMode ? " is-on" : "")
                  }
                  disabled={!!activeTab.saving}
                  onClick={toggleActiveEditMode}
                  aria-pressed={!!activeTab.editMode}
                  aria-label={
                    activeTab.editMode
                      ? tr("resources.previewMode")
                      : tr("resources.editMode")
                  }
                >
                  <IconEdit size={14} />
                  <span className="rp-editor__tool-btn-label">
                    {activeTab.editMode
                      ? tr("resources.previewMode")
                      : tr("resources.editMode")}
                  </span>
                </Button>
              </Tip>
            ) : null}
            <div className="rp-editor__toolbar-spacer" />
            {dirty ? (
              <Tip label={tr("resources.revert")}>
                <Button
                  type="button"
                  className="rp-editor__tool-btn"
                  disabled={!!activeTab.saving}
                  onClick={() => revertActiveDraft()}
                >
                  {tr("resources.revert")}
                </Button>
              </Tip>
            ) : null}
            <Tip label={tr("resources.save")}>
              <Button
                type="button"
                className={
                  "rp-editor__tool-btn rp-editor__tool-btn--save" +
                  (dirty ? " is-dirty" : "")
                }
                disabled={!!activeTab.saving || !dirty}
                onClick={() => void saveActiveFile()}
              >
                {activeTab.saving
                  ? tr("resources.saving")
                  : tr("resources.save")}
              </Button>
            </Tip>
            {dirty ? (
              <span className="rp-editor__dirty-label" role="status">
                {tr("resources.unsaved")}
              </span>
            ) : null}
          </div>
          {preview.truncated ? (
            <div className="rp-editor__banner" role="status">
              {tr("resources.truncated")}
            </div>
          ) : null}
          {showEditor ? (
            <Textarea
              className="rp-editor__textarea"
              value={draftText}
              spellCheck={preview.kind === "text"}
              disabled={!!activeTab.saving}
              aria-label={tr("resources.editorAria", { name: preview.name })}
              onChange={(e) => updateActiveDraft(e.target.value)}
              onKeyDown={(e) => {
                if ((e.metaKey || e.ctrlKey) && e.key === "s") {
                  e.preventDefault();
                  void saveActiveFile();
                  return;
                }
                if (
                  e.key === "Tab" &&
                  !e.metaKey &&
                  !e.ctrlKey &&
                  !e.altKey
                ) {
                  e.preventDefault();
                  const el = e.currentTarget;
                  const start = el.selectionStart;
                  const end = el.selectionEnd;
                  const next =
                    draftText.slice(0, start) +
                    "  " +
                    draftText.slice(end);
                  updateActiveDraft(next);
                  requestAnimationFrame(() => {
                    el.selectionStart = el.selectionEnd = start + 2;
                  });
                }
              }}
            />
          ) : (
            <OverlayScroll className="rp-editor__preview-scroll">
              <div className="rp-editor__preview-body rp-preview__md">
                <MarkdownBody>
                  {draftText || preview.text || ""}
                </MarkdownBody>
              </div>
            </OverlayScroll>
          )}
        </div>
      );
    }

    // Word、Excel、PowerPoint 与 ODF 文档预览。
    if (
      isOfficeKind(preview.kind) &&
      preview.absolutePath &&
      preview.kind !== "image"
    ) {
      return (
        <OfficeDocumentPreview
          kind={preview.kind === "office" ? guessOfficeKind(preview.name) : preview.kind}
          absolutePath={preview.absolutePath}
          name={preview.name}
          locale={locale}
          textFallback={preview.text}
          errorFromHost={preview.error}
          embedded
        />
      );
    }

    switch (preview.kind) {
      case "image":
        if (
          preview.text &&
          (preview.mime.includes("svg") || preview.name.endsWith(".svg"))
        ) {
          return (
            <div
              className="rp-preview__svg"
              dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(preview.text) }}
            />
          );
        }
        return src ? (
          <ImageUi
            layout="pane"
            className="rp-preview__img"
            src={src}
            alt={preview.name}
            path={preview.absolutePath || undefined}
            labels={{
              viewImage: tr("image.view"),
              copyImage: tr("image.copy"),
              reveal: tr("attach.reveal"),
              copyPath: tr("attach.copyPath"),
            }}
          />
        ) : (
          <div className="rp-preview__msg">{tr("resources.binary")}</div>
        );
      case "audio":
      case "video":
        return src ? (
          <FileMediaPlayer
            kind={preview.kind}
            src={src}
            mime={preview.mime}
            title={preview.name}
            absolutePath={preview.absolutePath || undefined}
            labels={{
              loadError: tr("media.loadError"),
              openExternal: tr("media.openExternal"),
              loading: tr("resources.loading"),
            }}
          />
        ) : (
          <div className="rp-preview__msg">{tr("resources.binary")}</div>
        );
      case "markdown":
        return (
          <div className="rp-preview__md">
            <MarkdownBody>
              {activeTab?.draftText ?? preview.text ?? ""}
            </MarkdownBody>
          </div>
        );
      case "html":
        // Do not use file:// in iframe — WKWebView/Tauri blocks it (blank page).
        // HtmlBrowser uses srcDoc (host text) or asset fetch; scripts work, full-bleed.
        return (
          <HtmlBrowser
            locale={locale}
            title={preview.name}
            absolutePath={preview.absolutePath || null}
            html={preview.text}
          />
        );
      case "json": {
        let body = preview.text ?? "";
        try {
          body = JSON.stringify(JSON.parse(body), null, 2);
        } catch {
          /* keep raw */
        }
        return (
          <CodePreview
            code={body}
            fileName={preview.name.endsWith(".json") ? preview.name : "data.json"}
            language="json"
            footer={
              preview.truncated ? tr("resources.truncated") : null
            }
          />
        );
      }
      default:
        if (preview.text) {
          return (
            <CodePreview
              code={preview.text}
              fileName={preview.name}
              footer={
                preview.truncated ? tr("resources.truncated") : null
              }
            />
          );
        }
        return (
          <div className="rp-preview__msg">
            {preview.error || tr("resources.binary")}
            <div className="rp-preview__meta">
              {preview.name} · {formatSize(preview.size)}
            </div>
          </div>
        );
    }
  }, [
    activeTab,
    tr,
    locale,
    sideMode,
    diffView,
    revealChangePath,
    copyChangePath,
    pathCopyFlash,
    updateActiveDraft,
    saveActiveFile,
    revertActiveDraft,
    toggleActiveEditMode,
  ]);

  // No project and no open tabs → empty; allow absolute/url tabs without a project.
  if (!projectPath && tabs.length === 0) {
    return (
      <div className="rp" data-testid="resource-viewer">
        <div className="rp-chrome">
          <div
            className="rp-mode-tabs"
            role="tablist"
            aria-label={tr("resources.title")}
          >
            <Button
              type="button"
              role="tab"
              aria-selected={sideMode === "files"}
              className={
                "rp-mode-tab" + (sideMode === "files" ? " is-active" : "")
              }
              onClick={() => setSideMode("files")}
            >
              <IconFiles size={14} />
              {tr("changes.files")}
            </Button>
            <Button
              type="button"
              role="tab"
              aria-selected={sideMode === "changes"}
              className={
                "rp-mode-tab" +
                (sideMode === "changes" ? " is-active" : "")
              }
              onClick={() => setSideMode("changes")}
            >
              <IconFileDiff size={14} />
              {tr("changes.title")}
            </Button>
            <Button
              type="button"
              role="tab"
              aria-selected={sideMode === "terminal"}
              className={
                "rp-mode-tab" + (sideMode === "terminal" ? " is-active" : "")
              }
              onClick={() => setSideMode("terminal")}
            >
              <IconTerminal size={14} />
              {tr("terminal.title")}
            </Button>
            <Button
              type="button"
              role="tab"
              aria-selected={sideMode === "trajectory"}
              className={
                "rp-mode-tab" + (sideMode === "trajectory" ? " is-active" : "")
              }
              onClick={() => setSideMode("trajectory")}
            >
              <IconListTree size={14} />
              {tr("trajectory.title")}
            </Button>
          </div>
        </div>
        {sideMode === "trajectory" ? (
          <TrajectoryLedger
            locale={locale}
            sessionId={null}
            title={null}
            live={trajectoryLive}
            onLoadMessages={
              onLoadTrajectoryMessages ?? (async () => [] as ChatMessage[])
            }
          />
        ) : (
          <div className="rp__empty-state">
            <div className="rp__empty-title">{tr("main.noProject")}</div>
            <div className="rp__empty-desc">{tr("resources.needProject")}</div>
          </div>
        )}
      </div>
    );
  }

  /** 顶层模式标签分离文件和变更；文件模式内保留已打开文件标签。 */
  return (
    <div
      className="rp"
      data-testid="resource-viewer"
      aria-label={projectName ?? tr("resources.title")}
    >
      <div className="rp-chrome">
        <div
          className="rp-mode-tabs"
          role="tablist"
          aria-label={tr("resources.title")}
        >
          <Button
            type="button"
            role="tab"
            aria-selected={sideMode === "files"}
            className={
              "rp-mode-tab" + (sideMode === "files" ? " is-active" : "")
            }
            onClick={() => setSideMode("files")}
          >
            <IconFiles size={14} />
            {tr("changes.files")}
          </Button>
          <Button
            type="button"
            role="tab"
            aria-selected={sideMode === "changes"}
            className={
              "rp-mode-tab" +
              (sideMode === "changes" ? " is-active" : "")
            }
            onClick={() => setSideMode("changes")}
          >
            <IconFileDiff size={14} />
            {tr("changes.title")}
            {totalChangeBadge > 0 ? (
              <span className="rp-mode-tab__count">
                {totalChangeBadge > 99 ? "99+" : totalChangeBadge}
              </span>
            ) : null}
          </Button>
          <Button
            type="button"
            role="tab"
            aria-selected={sideMode === "terminal"}
            className={
              "rp-mode-tab" + (sideMode === "terminal" ? " is-active" : "")
            }
            onClick={() => setSideMode("terminal")}
          >
            <IconTerminal size={14} />
            {tr("terminal.title")}
          </Button>
          <Button
            type="button"
            role="tab"
            aria-selected={sideMode === "trajectory"}
            className={
              "rp-mode-tab" + (sideMode === "trajectory" ? " is-active" : "")
            }
            onClick={() => setSideMode("trajectory")}
          >
            <IconListTree size={14} />
            {tr("trajectory.title")}
          </Button>
        </div>
        {absPath ? (
          <div className="rp-chrome__actions">
            <OpenLocationButton
              path={absPath}
              target={openWithTarget}
              onTargetChange={(t) => {
                try {
                  saveResourceOpenTarget(t);
                  setOpenWithTarget(t);
                } catch (persistError) {
                  setError(localizeUiError(persistError, locale));
                }
              }}
              onOpenError={(e) => setError(localizeUiError(e, locale))}
              compact
              labels={{
                openLocation: tr("main.openLocation"),
                openHint: tr("main.openLocationHint"),
                openMenu: tr("main.openLocationMenu"),
                finder: tr("resources.revealFolder"),
                systemDefault: tr("resources.openDefault"),
                copyPath: tr("attach.copyPath"),
              }}
            />
          </div>
        ) : null}
      </div>

      {sideMode === "files" ? (
        <div className="rp-file-tabs">
          <div
            className="rp-tabs"
            role="tablist"
            aria-label={tr("resources.files")}
          >
          <div className="rp-tabs__scroll">
            {tabs.length === 0 ? (
              <div className="rp-tabs__placeholder">
                <span className="rp-tabs__hint">{tr("resources.emptyPreview")}</span>
              </div>
            ) : (
              tabs.map((t) => {
                const active = t.id === activeId;
                return (
                  <Tip
                    key={t.id}
                    label={
                      active
                        ? t.relativePath || t.name
                        : `${t.name}\n${t.relativePath || ""}`
                    }
                  >
                    <Button
                      type="button"
                      role="tab"
                      aria-selected={active}
                      title={t.relativePath || t.name}
                      className={
                        "rp-tab" +
                        (active ? " is-active" : " is-inactive") +
                        (t.tabKind === "url" ? " rp-tab--url" : "")
                      }
                      onClick={() => setActiveId(t.id)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        setTabMenu({
                          x: e.clientX,
                          y: e.clientY,
                          tabId: t.id,
                        });
                      }}
                    >
                      <FileKindMark
                        name={t.tabKind === "url" ? "web.html" : t.name}
                        isDir={false}
                      />
                      <span className="rp-tab__name">
                        {isResourceDraftDirty(t.draftText, t.baselineText)
                          ? `• ${t.name}`
                          : t.name}
                      </span>
                      {active ? (
                          <span
                            className="rp-tab__x"
                            role="button"
                            tabIndex={0}
                            title={tr("resources.tabClose")}
                            onClick={(e) => {
                              e.stopPropagation();
                              closeTab(t.id);
                            }}
                            onKeyDown={(e) => {
                              if (e.key === "Enter" || e.key === " ") {
                                e.stopPropagation();
                                closeTab(t.id);
                              }
                            }}
                          >
                            ×
                          </span>
                      ) : isResourceDraftDirty(t.draftText, t.baselineText) ? (
                        <span className="rp-tab__dirty" aria-hidden>
                          •
                        </span>
                      ) : null}
                    </Button>
                  </Tip>
                );
              })
            )}
          </div>
          </div>
        </div>
      ) : null}

      {error && (
        <div className="rp__error" role="alert">
          {error}
          <Tip label={tr("common.dismiss")}>
            <Button
              type="button"
              className="chrome-btn"
              onClick={() => setError(null)}
            >
              <IconClose size={12} />
            </Button>
          </Tip>
        </div>
      )}
      {activeTab?.error && (
        <div className="rp__error" role="alert">
          {activeTab.error}
        </div>
      )}

      <TerminalPanel
        projectPath={projectPath}
        locale={locale}
        active={sideMode === "terminal"}
      />

      {sideMode === "trajectory" ? (
        <TrajectoryLedger
          locale={locale}
          sessionId={trajectorySessionId}
          title={trajectorySessionTitle ?? trajectoryLive?.title ?? null}
          live={trajectoryLive}
          onLoadMessages={
            onLoadTrajectoryMessages ?? (async () => [] as ChatMessage[])
          }
        />
      ) : null}

      {/* Split: preview | resizer | tree */}
      <div
        ref={splitRef}
        className={
          "rp-split" +
          (resizingTree ? " is-resizing" : "") +
          (sideMode === "terminal" || sideMode === "trajectory"
            ? " is-hidden"
            : "")
        }
      >
        <div className="rp-split__preview">
          {diffView?.unified ? (
            <div
              className={
                "rp-change-preview rp-change-preview--persistent" +
                (persistentDiffVisible ? "" : " is-hidden")
              }
              aria-hidden={!persistentDiffVisible}
            >
              <div className="rp-change-preview__toolbar">
                <Button
                  type="button"
                  className="rp-tool-btn"
                  onClick={() =>
                    openCurrentChangeFile(diffView.path, diffView.name)
                  }
                >
                  <IconFiles size={14} />
                  <span className="rp-tool-btn__label">
                    {tr("changes.openFile")}
                  </span>
                </Button>
              </div>
              <div className="rp-preview-code-host">
                <StructuredDiffPreview
                  patch={diffView.unified}
                  locale={locale}
                />
              </div>
            </div>
          ) : null}
          {persistentDiffVisible ? null : sideMode === "changes" && diffView ? (
            diffView.loading ? (
              <div className="rp__empty-state">
                <div className="rp__empty-desc">{tr("changes.loadingDiff")}</div>
              </div>
            ) : diffView.afterOnly ? (
              <div className="rp-change-preview">
                <div className="rp-change-preview__toolbar">
                  <Button
                    type="button"
                    className="rp-tool-btn"
                    onClick={() =>
                      openCurrentChangeFile(diffView.path, diffView.name)
                    }
                  >
                    <IconFiles size={14} />
                    <span className="rp-tool-btn__label">
                      {tr("changes.openFile")}
                    </span>
                  </Button>
                </div>
                <div className="rp-preview-code-host">{previewBody}</div>
              </div>
            ) : (
              <div className="rp__empty-state">{previewBody}</div>
            )
          ) : !activeTab ? (
            <div className="rp__empty-state">
              <div className="rp__empty-title">
                {sideMode === "changes" &&
                workspaceCount === 0
                    ? tr("changes.empty")
                    : sideMode === "changes"
                      ? tr("changes.title")
                      : tr("resources.emptyPreview")}
              </div>
              <div className="rp__empty-desc">
                {sideMode === "changes" &&
                workspaceCount === 0
                    ? tr("changes.emptyHint")
                    : sideMode === "changes"
                      ? tr("changes.workspace.emptyHint")
                      : tr("resources.emptyPreviewHint")}
              </div>
            </div>
          ) : activeTab.loading ? (
            <div className="rp__empty-state">
              <div className="rp__empty-desc">{tr("resources.loading")}</div>
            </div>
          ) : activeTab.tabKind === "url" && activeTab.url ? (
            /* Native child Webview over host (GitHub etc. block iframe) */
            <div className="rp-preview-browser rp-preview-browser--url">
              <EmbeddedBrowser
                url={activeTab.url}
                title={activeTab.name}
                locale={locale}
                active
              />
            </div>
          ) : activeTabEditable && activeTab.preview ? (
            /* Full-height editor shell (toolbar + textarea / md preview) */
            <div className="rp-preview-code-host rp-preview-editor-host">
              {previewBody}
            </div>
          ) : activeTab.preview?.kind === "html" ? (
            <div className="rp-preview-browser">{previewBody}</div>
          ) : activeTab.preview &&
            isOfficeKind(activeTab.preview.kind) &&
            activeTab.preview.kind !== "image" ? (
            <div className="rp-preview-office">{previewBody}</div>
          ) : activeTab.preview?.text &&
            (activeTab.preview.kind === "json" ||
              activeTab.preview.kind === "text" ||
              activeTab.preview.kind === "code" ||
              // host may classify source as generic text
              (!["markdown", "html", "image", "audio", "video"].includes(
                activeTab.preview.kind,
              ) &&
                !!activeTab.preview.text)) ? (
            <div className="rp-preview-code-host">{previewBody}</div>
          ) : (
            <OverlayScroll className="rp-preview-scroll">
              <div className="rp-preview-body">{previewBody}</div>
            </OverlayScroll>
          )}
        </div>

          <>
            <div
              className="rp-split__resizer"
              role="separator"
              aria-orientation="vertical"
              aria-label={tr("resources.resizeTree")}
              aria-valuenow={treeWidth}
              onPointerDown={(e) => {
                e.preventDefault();
                setResizingTree(true);
              }}
            />
            <div
              className="rp-split__tree"
              style={{
                width: treeWidth,
                flex: `0 0 ${treeWidth}px`,
                maxWidth: treeWidth,
                minWidth: RESOURCE_TREE_WIDTH_MIN,
              }}
            >
              <div className="rp-tree-search">
                <IconSearch size={14} />
                <Input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder={tr("resources.filterPh")}
                  aria-label={tr("resources.filterPh")}
                />
              </div>
              <OverlayScroll className="rp-tree-scroll">
                {sideMode === "changes" ? (
                  <div className="rp-changes-list" role="list">
                    {/* Git 工作区变更。 */}
                    <div className="rp-changes-section">
                      <div className="rp-changes-section__head">
                        <span className="rp-changes-section__title">
                          {tr("changes.section.workspace")}
                        </span>
                        {workspaceCount > 0 ? (
                          <span className="rp-changes-section__count">
                            {workspaceCount}
                          </span>
                        ) : null}
                        {workspaceBranch ? (
                          <span
                            className="rp-changes-section__branch"
                            title={tr("changes.workspace.branch", {
                              branch: workspaceBranch,
                            })}
                          >
                            {workspaceBranch}
                          </span>
                        ) : null}
                      </div>
                      {workspaceLoading && workspaceFiles.length === 0 ? (
                        <div className="rp-changes-section__empty">
                          {tr("changes.workspace.loading")}
                        </div>
                      ) : !workspaceAvailable ? (
                        <div className="rp-changes-section__empty">
                          {workspaceUnavailableLabel()}
                        </div>
                      ) : filteredWorkspace.length === 0 ? (
                        <div className="rp-changes-section__empty">
                          {tr("changes.workspace.empty")}
                        </div>
                      ) : (
                        renderWorkspaceRows(filteredWorkspace)
                      )}
                    </div>
                  </div>
                ) : loadingTree ? (
                  <div className="rp__empty-state rp__empty-state--sm">
                    {tr("resources.loading")}
                  </div>
                ) : root.length === 0 ? (
                  <div className="rp__empty-state rp__empty-state--sm">
                    {tr("resources.empty")}
                  </div>
                ) : (
                  renderTree(root, 0)
                )}
              </OverlayScroll>
            </div>
          </>
      </div>

      {/* Chrome-style tab context menu */}
      {(() => {
        const idx = tabMenu
          ? tabs.findIndex((t) => t.id === tabMenu.tabId)
          : -1;
        const hasLeft = idx > 0;
        const hasRight = idx >= 0 && idx < tabs.length - 1;
        const hasOthers = tabs.length > 1;
        const tabId = tabMenu?.tabId ?? "";
        const items: ContextMenuItem[] = [
          {
            id: "close",
            label: tr("resources.tabClose"),
            onClick: () => closeTab(tabId),
          },
          {
            id: "close-others",
            label: tr("resources.tabCloseOthers"),
            disabled: !hasOthers,
            onClick: () => closeOtherTabs(tabId),
          },
          {
            id: "close-right",
            label: tr("resources.tabCloseRight"),
            disabled: !hasRight,
            onClick: () => closeTabsToRight(tabId),
          },
          {
            id: "close-left",
            label: tr("resources.tabCloseLeft"),
            disabled: !hasLeft,
            onClick: () => closeTabsToLeft(tabId),
          },
          {
            id: "close-all",
            label: tr("resources.tabCloseAll"),
            onClick: () => closeAllTabs(),
          },
        ];
        return (
          <ContextMenu
            open={!!tabMenu}
            x={tabMenu?.x ?? 0}
            y={tabMenu?.y ?? 0}
            onClose={() => setTabMenu(null)}
            items={items}
            className="rp-tab-menu"
          />
        );
      })()}

      <GlassModal
        open={!!conflictTabId}
        onClose={() => setConflictTabId(null)}
        title={tr("resources.conflictTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              onClick={() => {
                setConflictTabId(null);
                void reloadActiveFile();
              }}
            >
              {tr("resources.conflictReload")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              onClick={() => {
                setConflictTabId(null);
                void saveActiveFile({ force: true });
              }}
            >
              {tr("resources.conflictOverwrite")}
            </Button>
          </>
        }
      >
        <p className="rp-modal-copy">{tr("resources.conflictBody")}</p>
      </GlassModal>

      <GlassModal
        open={!!discardTabId}
        onClose={() => setDiscardTabId(null)}
        title={tr("resources.discardTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              onClick={() => setDiscardTabId(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              onClick={() => {
                const id = discardTabId;
                setDiscardTabId(null);
                if (id) closeTabForced(id);
              }}
            >
              {tr("resources.discardConfirm")}
            </Button>
          </>
        }
      >
        <p className="rp-modal-copy">{tr("resources.discardBody")}</p>
      </GlassModal>
    </div>
  );
}
