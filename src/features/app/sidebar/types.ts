import type {
  Dispatch,
  DragEvent as ReactDragEvent,
  MouseEvent as ReactMouseEvent,
  SetStateAction,
} from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { Project, SessionRow } from "@/features/app/models";
import type { SessionSnapshot } from "@/lib/session";

export type SidebarTranslator = (key: MessageKey, vars?: Vars) => string;
export type SidebarSetState<T> = Dispatch<SetStateAction<T>>;

export type SidebarNewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => void | Promise<void>;

export type SidebarOpenSession = (
  session: SessionRow,
  project?: Project | null,
) => void | Promise<void>;

export type SidebarDragStart = (
  event: ReactDragEvent<HTMLElement>,
  kind: "project" | "session",
  id: string,
) => void;

export type SidebarDropSession = (
  event: ReactDragEvent<HTMLElement>,
  targetId: string,
) => void;

export type SidebarOpenSessionMenu = (
  event: ReactMouseEvent,
  session: SessionRow,
) => void;

export type SidebarArchiveSession = (
  session: SessionRow,
  archived?: boolean,
) => void | Promise<void>;

export type SidebarPinSession = (
  session: SessionRow,
  pinned?: boolean,
) => void | Promise<void>;

export interface SidebarSessionActions {
  startSidebarDrag: SidebarDragStart;
  endSidebarDrag: () => void;
  dropSession: SidebarDropSession;
  openSession: SidebarOpenSession;
  openSessionMenu: SidebarOpenSessionMenu;
  archiveSession: SidebarArchiveSession;
  pinSession: SidebarPinSession;
}

export interface SidebarSessionStatus {
  session: SessionSnapshot;
  busyIds: Set<string>;
  completedUnreadIds: Set<string>;
  pendingAskUserSessionIds: Set<string>;
}

export interface SidebarProjectDropHint {
  id: string;
  after: boolean;
}

export type SidebarDragOverProject = (
  event: ReactDragEvent<HTMLElement>,
  targetId: string,
) => void;

export type SidebarDropProject = SidebarDragOverProject;

export type SidebarOpenProjectMenu = (
  event: ReactMouseEvent,
  project: Project,
) => void;

export type SidebarRelocateProject = (
  project: Project,
) => void | Promise<void>;

export type SidebarApplyProjectOrder = (ids: string[]) => void;

export type SidebarAddProject = (
  returnFocus?: HTMLElement | null,
) => void | Promise<void>;

export type SidebarShowToast = (
  message: string,
  duration?: number,
) => void;
