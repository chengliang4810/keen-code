import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import type { Project } from "@/features/app/models";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import {
  buildWorktreeSiblingPath,
  mainWorktreePath,
  pathsEqual,
  sanitizeWorktreeName,
} from "@/lib/gitWorktree";
import { pathBasename } from "@/lib/attachments";
import * as api from "@/lib/api";

export type WorktreeSessionBinding = {
  silent?: boolean;
};

export type WorktreeCreateOptions = {
  startNewChat?: boolean;
};

export type WorktreeFinalizeProjectOptions = {
  bindSession: boolean;
  /** Suppress the generic project-added/bound toast for a worktree action. */
  silent?: boolean;
};

export type FinalizeAddedProject = (
  project: Project,
  options: WorktreeFinalizeProjectOptions,
) => Promise<Project | void>;

export type BindSessionProject = (
  project: Project | null,
  options?: WorktreeSessionBinding,
) => Promise<void>;

export type NewChat = (project?: Project | null) => Promise<void>;

export type ShowToast = (message: string, durationMs?: number) => void;

export type SetLocalError = Dispatch<SetStateAction<string | null>>;

export interface UseWorktreesOptions {
  activeProject: Project | null;
  projects: Project[];
  locale: Locale;
  finalizeAddedProject: FinalizeAddedProject;
  bindSessionProject: BindSessionProject;
  newChat: NewChat;
  showToast: ShowToast;
  setLocalError: SetLocalError;
}

export interface UseWorktreesResult {
  gitWorktrees: api.GitWorktreeEntry[];
  gitWorktreesAvailable: boolean | null;
  gitWorktreesLoading: boolean;
  gitWorktreesReason: string | null;
  refreshGitWorktrees: () => Promise<void>;

  worktreeCreateOpen: boolean;
  setWorktreeCreateOpen: Dispatch<SetStateAction<boolean>>;
  worktreeCreateName: string;
  setWorktreeCreateName: Dispatch<SetStateAction<string>>;
  worktreeCreateRef: string;
  setWorktreeCreateRef: Dispatch<SetStateAction<string>>;
  worktreeCreateBusy: boolean;
  worktreeCreateError: string | null;
  setWorktreeCreateError: Dispatch<SetStateAction<string | null>>;
  worktreeCreateStartChat: boolean;
  worktreeCreatePreviewPath: string | null;
  openWorktreeCreate: (options?: WorktreeCreateOptions) => void;
  submitWorktreeCreate: () => Promise<void>;

  worktreeGcOpen: boolean;
  setWorktreeGcOpen: Dispatch<SetStateAction<boolean>>;
  worktreeGcForce: boolean;
  setWorktreeGcForce: Dispatch<SetStateAction<boolean>>;
  worktreeGcBusy: boolean;
  worktreeGcPreviewBusy: boolean;
  worktreeGcError: string | null;
  setWorktreeGcError: Dispatch<SetStateAction<string | null>>;
  worktreeGcPreview: api.GitWorktreeGcResult | null;
  setWorktreeGcPreview: Dispatch<
    SetStateAction<api.GitWorktreeGcResult | null>
  >;
  openWorktreeGc: () => void;
  refreshWorktreeGcPreview: () => Promise<void>;
  submitWorktreeGc: () => Promise<void>;
  switchToWorktree: (worktree: api.GitWorktreeEntry) => Promise<void>;
}

/** 管理 Git worktree 列表、切换、创建以及失效记录清理。 */
export function useWorktrees({
  activeProject,
  projects,
  locale,
  finalizeAddedProject,
  bindSessionProject,
  newChat,
  showToast,
  setLocalError,
}: UseWorktreesOptions): UseWorktreesResult {
  const tr = useMemo(() => createT(locale), [locale]);
  const [gitWorktrees, setGitWorktrees] = useState<api.GitWorktreeEntry[]>([]);
  const [gitWorktreesAvailable, setGitWorktreesAvailable] = useState<
    boolean | null
  >(null);
  const [gitWorktreesLoading, setGitWorktreesLoading] = useState(false);
  const [gitWorktreesReason, setGitWorktreesReason] = useState<string | null>(
    null,
  );
  const [worktreeCreateOpen, setWorktreeCreateOpen] = useState(false);
  const [worktreeCreateName, setWorktreeCreateName] = useState("");
  const [worktreeCreateRef, setWorktreeCreateRef] = useState("");
  const [worktreeCreateBusy, setWorktreeCreateBusy] = useState(false);
  const [worktreeCreateError, setWorktreeCreateError] = useState<string | null>(
    null,
  );
  const [worktreeCreateStartChat, setWorktreeCreateStartChat] = useState(false);
  const [worktreeGcOpen, setWorktreeGcOpen] = useState(false);
  const [worktreeGcForce, setWorktreeGcForce] = useState(false);
  const [worktreeGcBusy, setWorktreeGcBusy] = useState(false);
  const [worktreeGcPreviewBusy, setWorktreeGcPreviewBusy] = useState(false);
  const [worktreeGcError, setWorktreeGcError] = useState<string | null>(null);
  const [worktreeGcPreview, setWorktreeGcPreview] =
    useState<api.GitWorktreeGcResult | null>(null);

  const gitWorktreesReqRef = useRef(0);
  const gitWorktreesPathRef = useRef<string | null>(null);

  const refreshGitWorktrees = useCallback(async () => {
    const path = activeProject?.path?.trim() || null;
    if (!path || !api.isTauri()) {
      gitWorktreesReqRef.current += 1;
      gitWorktreesPathRef.current = null;
      setGitWorktrees([]);
      setGitWorktreesAvailable(null);
      setGitWorktreesReason(null);
      setGitWorktreesLoading(false);
      return;
    }

    const requestId = ++gitWorktreesReqRef.current;
    // Keep the current list for a soft refresh, but never show another
    // project's rows while the active project path changes.
    if (gitWorktreesPathRef.current !== path) {
      gitWorktreesPathRef.current = path;
      setGitWorktrees([]);
      setGitWorktreesAvailable(null);
      setGitWorktreesReason(null);
    }
    setGitWorktreesLoading(true);
    try {
      const result = await api.gitWorktreesList(path);
      if (requestId !== gitWorktreesReqRef.current) return;
      if (!result.available) {
        setGitWorktrees([]);
        setGitWorktreesAvailable(false);
        setGitWorktreesReason(result.reason?.trim() || "unavailable");
      } else {
        setGitWorktrees(result.worktrees ?? []);
        setGitWorktreesAvailable(true);
        setGitWorktreesReason(null);
      }
    } catch (error) {
      if (requestId !== gitWorktreesReqRef.current) return;
      setGitWorktrees([]);
      setGitWorktreesAvailable(false);
      setGitWorktreesReason(String(error));
    } finally {
      if (requestId === gitWorktreesReqRef.current) {
        setGitWorktreesLoading(false);
      }
    }
  }, [activeProject?.path]);

  useEffect(() => {
    void refreshGitWorktrees();
  }, [refreshGitWorktrees]);

  const openWorktreeGc = useCallback(() => {
    setWorktreeGcForce(false);
    setWorktreeGcError(null);
    setWorktreeGcBusy(false);
    setWorktreeGcPreview(null);
    setWorktreeGcOpen(true);
  }, []);

  const refreshWorktreeGcPreview = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path || !worktreeGcOpen) return;
    setWorktreeGcPreviewBusy(true);
    setWorktreeGcError(null);
    try {
      const result = await api.gitWorktreeGc({
        projectPath: activeProject.path,
        dryRun: true,
        force: worktreeGcForce,
      });
      setWorktreeGcPreview(result);
    } catch (error) {
      setWorktreeGcPreview(null);
      setWorktreeGcError(localizeUiError(error, locale));
    } finally {
      setWorktreeGcPreviewBusy(false);
    }
  }, [activeProject?.path, locale, worktreeGcForce, worktreeGcOpen]);

  useEffect(() => {
    if (!worktreeGcOpen) return;
    void refreshWorktreeGcPreview();
  }, [refreshWorktreeGcPreview, worktreeGcOpen]);

  const submitWorktreeGc = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path) return;
    setWorktreeGcBusy(true);
    setWorktreeGcError(null);
    try {
      const result = await api.gitWorktreeGc({
        projectPath: activeProject.path,
        dryRun: false,
        force: worktreeGcForce,
      });
      setWorktreeGcOpen(false);
      setWorktreeGcPreview(null);
      setWorktreeGcForce(false);
      await refreshGitWorktrees();
      setLocalError(null);
      showToast(
        result.prunedCount > 0
          ? tr("composer.worktreeGcDone", {
              n: String(result.prunedCount),
            })
          : tr("composer.worktreeGcDoneNone"),
        2800,
      );
    } catch (error) {
      setWorktreeGcError(localizeUiError(error, locale));
    } finally {
      setWorktreeGcBusy(false);
    }
  }, [
    activeProject?.path,
    locale,
    refreshGitWorktrees,
    setLocalError,
    showToast,
    tr,
    worktreeGcForce,
  ]);

  const switchToWorktree = useCallback(
    async (worktree: api.GitWorktreeEntry) => {
      if (!api.isTauri()) return;
      const path = worktree.path?.trim();
      if (!path) return;
      try {
        const existing = projects.find((project) => pathsEqual(project.path, path));
        let target = existing;
        if (target) {
          await bindSessionProject(target, { silent: true });
        } else {
          const added = (await api.projectCreate(
            pathBasename(path),
            path,
          )) as Project;
          target =
            (await finalizeAddedProject(added, {
              bindSession: true,
              silent: true,
            })) ?? added;
        }
        setLocalError(null);
        showToast(
          tr("composer.worktreeSwitched", {
            name: target.name,
            branch: worktree.branch || tr("composer.worktreeDetached"),
          }),
          2500,
        );
      } catch (error) {
        showToast(localizeUiError(error, locale), 4500);
      }
    },
    [
      bindSessionProject,
      finalizeAddedProject,
      locale,
      projects,
      setLocalError,
      showToast,
      tr,
    ],
  );

  const openWorktreeCreate = useCallback(
    (options?: WorktreeCreateOptions) => {
      setWorktreeCreateName("");
      setWorktreeCreateRef("");
      setWorktreeCreateError(null);
      setWorktreeCreateBusy(false);
      setWorktreeCreateStartChat(!!options?.startNewChat);
      setWorktreeCreateOpen(true);
    },
    [],
  );

  const worktreeCreatePreviewPath = useMemo(() => {
    try {
      const main = mainWorktreePath(gitWorktrees) || activeProject?.path || "";
      if (!main || !worktreeCreateName.trim()) return null;
      return buildWorktreeSiblingPath(main, worktreeCreateName.trim());
    } catch {
      return null;
    }
  }, [activeProject?.path, gitWorktrees, worktreeCreateName]);

  const submitWorktreeCreate = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path) return;
    const rawName = worktreeCreateName.trim();
    if (!rawName) {
      setWorktreeCreateError(tr("composer.worktreeNameRequired"));
      return;
    }

    let safeName: string;
    try {
      safeName = sanitizeWorktreeName(rawName);
    } catch {
      setWorktreeCreateError(tr("composer.worktreeNameInvalid"));
      return;
    }

    setWorktreeCreateBusy(true);
    setWorktreeCreateError(null);
    try {
      const created = await api.gitWorktreeAdd(
        activeProject.path,
        safeName,
        worktreeCreateRef.trim() || null,
      );
      setWorktreeCreateOpen(false);
      await refreshGitWorktrees();

      const branch =
        created.branch?.trim() ||
        created.name ||
        tr("composer.worktreeDetached");
      const existing = projects.find((project) =>
        pathsEqual(project.path, created.path),
      );
      let target = existing;
      if (!target) {
        const added = (await api.projectCreate(
          pathBasename(created.path),
          created.path,
        )) as Project;
        target =
          (await finalizeAddedProject(added, {
            bindSession: false,
            silent: true,
          })) ?? added;
      }

      if (worktreeCreateStartChat) {
        await newChat(target);
        showToast(
          tr("composer.worktreeCreatedChat", {
            name: created.name,
            branch,
          }),
          2800,
        );
      } else {
        await bindSessionProject(target, { silent: true });
        showToast(
          tr("composer.worktreeCreated", {
            name: created.name,
            branch,
          }),
          2800,
        );
      }
      setLocalError(null);
    } catch (error) {
      setWorktreeCreateError(localizeUiError(error, locale));
    } finally {
      setWorktreeCreateBusy(false);
    }
  }, [
    activeProject?.path,
    bindSessionProject,
    finalizeAddedProject,
    locale,
    newChat,
    projects,
    refreshGitWorktrees,
    setLocalError,
    showToast,
    tr,
    worktreeCreateName,
    worktreeCreateRef,
    worktreeCreateStartChat,
  ]);

  return {
    gitWorktrees,
    gitWorktreesAvailable,
    gitWorktreesLoading,
    gitWorktreesReason,
    refreshGitWorktrees,
    worktreeCreateOpen,
    setWorktreeCreateOpen,
    worktreeCreateName,
    setWorktreeCreateName,
    worktreeCreateRef,
    setWorktreeCreateRef,
    worktreeCreateBusy,
    worktreeCreateError,
    setWorktreeCreateError,
    worktreeCreateStartChat,
    worktreeCreatePreviewPath,
    openWorktreeCreate,
    submitWorktreeCreate,
    worktreeGcOpen,
    setWorktreeGcOpen,
    worktreeGcForce,
    setWorktreeGcForce,
    worktreeGcBusy,
    worktreeGcPreviewBusy,
    worktreeGcError,
    setWorktreeGcError,
    worktreeGcPreview,
    setWorktreeGcPreview,
    openWorktreeGc,
    refreshWorktreeGcPreview,
    submitWorktreeGc,
    switchToWorktree,
  };
}
