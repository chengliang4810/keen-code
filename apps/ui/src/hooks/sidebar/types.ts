import type { Dispatch, SetStateAction } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import type { MessageKey, Vars } from "@/i18n";

export type SidebarSetState<T> = Dispatch<SetStateAction<T>>;

export type SidebarShowToast = (message: string, durationMs?: number) => void;

export type SidebarOpenSession = (
  session: SessionRow,
  project?: Project | null,
) => void | Promise<void>;

export type SidebarNewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => void | Promise<void>;

export type SidebarDragKind = "project" | "session";

export interface SidebarDropHint {
  id: string;
  after: boolean;
}

export type SidebarTranslator = (key: MessageKey, vars?: Vars) => string;
