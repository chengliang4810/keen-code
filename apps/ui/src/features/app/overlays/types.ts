import type { Dispatch, SetStateAction } from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { Project, SessionRow } from "@/features/app/models";

export type SetState<T> = Dispatch<SetStateAction<T>>;

export type Translator = (key: MessageKey, vars?: Vars) => string;

export type AsyncAction = () => void | Promise<void>;

export type AddProjectAction = (
  returnFocus?: HTMLElement | null,
) => void | Promise<void>;

export type OpenSessionAction = (
  session: SessionRow,
  project?: Project | null,
) => void | Promise<void>;

export type NewChatAction = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => void | Promise<void>;

export type ProjectAction = (project: Project) => void | Promise<void>;

export type SessionAction = (session: SessionRow) => void | Promise<void>;
