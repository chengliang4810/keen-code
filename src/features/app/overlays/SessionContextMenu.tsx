import { useMemo } from "react";
import type { ContextMenuState, Project, SessionRow } from "@/features/app/models";
import type { Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import * as api from "@/lib/api";
import { ContextMenu, type ContextMenuItem } from "@/components/ContextMenu";
import {
  IconCopy,
  IconExternalLink,
  IconFolderPlus,
  IconFork,
  IconListTree,
  IconRename,
  IconTrash,
} from "@/components/icons";
import type {
  ProjectAction,
  SessionAction,
  SetState,
  Translator,
} from "./types";

export interface SessionContextMenuProps {
  tr: Translator;
  locale: Locale;
  menu: ContextMenuState;
  setMenu: SetState<ContextMenuState>;
  projects: Project[];
  sessions: SessionRow[];
  setLocalError: SetState<string | null>;
  relocateProject: ProjectAction;
  removeProjectFromApp: ProjectAction;
  renameProject: ProjectAction;
  renameSession: SessionAction;
  confirmForkSession: SessionAction;
  viewTrajectory: SessionAction;
  copySessionId: SessionAction;
}

export function SessionContextMenu({
  tr,
  locale,
  menu,
  setMenu,
  projects,
  sessions,
  setLocalError,
  relocateProject,
  removeProjectFromApp,
  renameProject,
  renameSession,
  confirmForkSession,
  viewTrajectory,
  copySessionId,
}: SessionContextMenuProps) {
  const items = useMemo<ContextMenuItem[]>(() => {
    if (menu?.kind === "project") {
      const project = projects.find((candidate) => candidate.id === menu.id);
      if (!project) return [];
      return [
        {
          id: "reveal",
          label: tr("project.reveal"),
          icon: <IconExternalLink size={16} />,
          onClick: () => {
            void api.projectReveal(project.id).catch((error: unknown) => {
              const message = localizeUiError(error, locale);
              setLocalError(message);
            });
          },
        },
        {
          id: "relocate",
          label: tr("project.relocate"),
          icon: <IconFolderPlus size={16} />,
          onClick: () => void relocateProject(project),
        },
        {
          id: "rename",
          label: tr("project.rename"),
          icon: <IconRename size={16} />,
          onClick: () => void renameProject(project),
        },
        {
          id: "remove",
          label: tr("project.remove"),
          icon: <IconTrash size={16} />,
          danger: true,
          onClick: () => void removeProjectFromApp(project),
        },
      ];
    }

    if (menu?.kind === "session") {
      const session = sessions.find((candidate) => candidate.id === menu.id);
      if (!session) return [];
      return [
        {
          id: "rename",
          label: tr("session.rename"),
          icon: <IconRename size={16} />,
          onClick: () => void renameSession(session),
        },
        {
          id: "fork",
          label: tr("session.fork"),
          icon: <IconFork size={16} />,
          onClick: () => void confirmForkSession(session),
        },
        {
          id: "trajectory",
          label: tr("session.viewTrajectory"),
          icon: <IconListTree size={16} />,
          onClick: () => void viewTrajectory(session),
        },
        {
          id: "copy-id",
          label: tr("session.copyId"),
          icon: <IconCopy size={16} />,
          onClick: () => void copySessionId(session),
        },
      ];
    }

    return [];
  }, [
    confirmForkSession,
    copySessionId,
    locale,
    menu,
    projects,
    relocateProject,
    removeProjectFromApp,
    renameProject,
    renameSession,
    sessions,
    setLocalError,
    tr,
    viewTrajectory,
  ]);

  return (
    <ContextMenu
      open={menu !== null && items.length > 0}
      x={menu?.x ?? 0}
      y={menu?.y ?? 0}
      onClose={() => setMenu(null)}
      items={items}
      estimatedHeight={240}
    />
  );
}
