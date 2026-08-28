import { useCallback, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import { localizeUiError, type SessionSnapshot } from "@/lib/session";
import * as api from "@/lib/api";
import { pathsEqual } from "@/lib/gitWorktree";
import { pathBasename } from "@/lib/attachments";
import type { DragZone } from "@/lib/dragZone";
import type { SettingsSectionId } from "@/lib/settingsCatalog";
import type { Project } from "@/features/app/models";

export interface AddProjectIntent {
  bindSession: boolean;
}

export type ProjectDialogTranslator = (
  key: MessageKey,
  vars?: Vars,
) => string;

export interface UseProjectDialogOptions {
  projects: Project[];
  /** The session currently projected by the workbench. */
  activeSession: SessionSnapshot;
  finalizeAddedProject: (
    project: Project,
    intent: AddProjectIntent,
    activeSession: SessionSnapshot,
  ) => void | Promise<void>;
  navigateSettings: (section?: SettingsSectionId) => void;
  locale: Locale;
  tr: ProjectDialogTranslator;
  setDragZone: Dispatch<SetStateAction<DragZone>>;
  setLocalError: Dispatch<SetStateAction<string | null>>;
  showToast: (message: string, duration?: number) => void;
}

/** 管理添加项目弹窗及其原生目录选择/拖放来源。 */
export function useProjectDialog({
  projects,
  activeSession,
  finalizeAddedProject,
  navigateSettings,
  locale,
  tr,
  setDragZone,
  setLocalError,
  showToast,
}: UseProjectDialogOptions) {
  const [addProjectIntent, setAddProjectIntent] =
    useState<AddProjectIntent | null>(null);
  const [addProjectName, setAddProjectName] = useState("");
  const [addProjectPath, setAddProjectPath] = useState("");
  const [addProjectBusy, setAddProjectBusy] = useState(false);
  const [addProjectError, setAddProjectError] = useState<string | null>(null);
  const addProjectNameRef = useRef<HTMLInputElement>(null);
  const addProjectDropRef = useRef<HTMLButtonElement>(null);
  const addProjectReturnFocusRef = useRef<HTMLElement | null>(null);
  const addProjectSourceRequestRef = useRef(0);
  const addProjectNameEditedRef = useRef(false);

  const applyAddProjectSource = useCallback(
    (path: string) => {
      setAddProjectPath(path);
      if (!addProjectNameEditedRef.current) {
        setAddProjectName(
          projects.find((project) => pathsEqual(project.path, path))?.name ??
            pathBasename(path),
        );
      }
      setAddProjectError(null);
    },
    [projects],
  );

  const selectAddProjectSourceFromPaths = useCallback(
    async (paths: string[]) => {
      if (!paths.length || !api.isTauri()) return;
      const request = ++addProjectSourceRequestRef.current;
      // A replacement must not leave the previous folder submittable while
      // the host is still classifying the new path.
      setAddProjectPath("");
      setAddProjectError(null);
      try {
        const classified = await api.pathsClassify(paths);
        if (request !== addProjectSourceRequestRef.current) return;
        const dirs = classified.filter((entry) => entry.exists && entry.isDir);
        if (!dirs.length) {
          setAddProjectError(tr("addProject.folderOnly"));
          return;
        }
        if (dirs.length > 1) {
          setAddProjectError(tr("addProject.oneFolderOnly"));
          return;
        }
        applyAddProjectSource(dirs[0]!.path);
      } catch (error) {
        if (request === addProjectSourceRequestRef.current) {
          setAddProjectError(localizeUiError(error, locale));
        }
      }
    },
    [applyAddProjectSource, locale, tr],
  );

  const resetAddProject = useCallback(() => {
    addProjectSourceRequestRef.current += 1;
    addProjectNameEditedRef.current = false;
    setAddProjectIntent(null);
    setAddProjectName("");
    setAddProjectPath("");
    setAddProjectError(null);
    setAddProjectBusy(false);
    setDragZone(null);
  }, [setDragZone]);

  const openAddProject = useCallback(
    (opts: AddProjectIntent, returnFocus?: HTMLElement | null) => {
      resetAddProject();
      setLocalError(null);
      addProjectReturnFocusRef.current =
        returnFocus ??
        (document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null);
      setAddProjectIntent(opts);
    },
    [resetAddProject, setLocalError],
  );

  const closeAddProject = useCallback(() => {
    if (!addProjectBusy) resetAddProject();
  }, [addProjectBusy, resetAddProject]);

  const pickAddProjectDirectory = useCallback(async () => {
    setAddProjectError(null);
    if (!api.isTauri()) {
      setAddProjectError(tr("error.needTauri"));
      return;
    }
    const request = ++addProjectSourceRequestRef.current;
    try {
      const path = await api.pickDirectory();
      if (request === addProjectSourceRequestRef.current && path) {
        applyAddProjectSource(path);
      }
    } catch (error) {
      if (request === addProjectSourceRequestRef.current) {
        setAddProjectError(localizeUiError(error, locale));
      }
    }
  }, [applyAddProjectSource, locale, tr]);

  const submitAddProject = useCallback(async () => {
    const intent = addProjectIntent;
    const name = addProjectName.trim();
    if (!intent || addProjectBusy) return;
    if (!name) {
      setAddProjectError(tr("addProject.nameRequired"));
      addProjectNameRef.current?.focus();
      return;
    }
    const existing = addProjectPath
      ? projects.find((project) => pathsEqual(project.path, addProjectPath))
      : null;
    if (existing) {
      await finalizeAddedProject(existing, intent, activeSession);
      resetAddProject();
      showToast(tr("addProject.existingSelected", { name: existing.name }));
      return;
    }
    setAddProjectBusy(true);
    setAddProjectError(null);
    try {
      const project = (await api.projectCreate(
        name,
        addProjectPath || null,
      )) as Project;
      await finalizeAddedProject(project, intent, activeSession);
      resetAddProject();
    } catch (error) {
      setAddProjectError(localizeUiError(error, locale));
    } finally {
      setAddProjectBusy(false);
    }
  }, [
    activeSession,
    addProjectBusy,
    addProjectIntent,
    addProjectName,
    addProjectPath,
    finalizeAddedProject,
    locale,
    projects,
    resetAddProject,
    showToast,
    tr,
  ]);

  const addProject = useCallback(
    (returnFocus?: HTMLElement | null) => {
      openAddProject({ bindSession: false }, returnFocus);
    },
    [openAddProject],
  );

  const openProjectDirectorySettings = useCallback(() => {
    resetAddProject();
    navigateSettings("general");
  }, [navigateSettings, resetAddProject]);

  return {
    addProjectIntent,
    addProjectName,
    setAddProjectName,
    addProjectPath,
    addProjectBusy,
    addProjectError,
    setAddProjectError,
    addProjectNameRef,
    addProjectDropRef,
    addProjectReturnFocusRef,
    addProjectNameEditedRef,
    applyAddProjectSource,
    selectAddProjectSourceFromPaths,
    resetAddProject,
    openAddProject,
    closeAddProject,
    pickAddProjectDirectory,
    submitAddProject,
    addProject,
    openProjectDirectorySettings,
  };
}
