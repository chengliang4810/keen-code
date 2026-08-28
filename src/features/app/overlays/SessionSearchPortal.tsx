import { createPortal } from "react-dom";
import type { RefObject } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import type { SessionSearchHits } from "@/lib/sessionSearch";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  IconClose,
  IconFolder,
  IconNewChat,
  IconSearch,
} from "@/components/icons";
import type {
  AddProjectAction,
  NewChatAction,
  OpenSessionAction,
  SetState,
  Translator,
} from "./types";

export interface SessionSearchPortalProps {
  tr: Translator;
  open: boolean;
  setOpen: SetState<boolean>;
  query: string;
  setQuery: SetState<string>;
  returnFocusRef: RefObject<HTMLElement | null>;
  hits: SessionSearchHits;
  projects: Project[];
  sessions: SessionRow[];
  activeProject: Project | null;
  openSession: OpenSessionAction;
  newChat: NewChatAction;
  addProject: AddProjectAction;
  setProjectsOpen: SetState<boolean>;
  setExpandedProjects: SetState<Record<string, boolean>>;
}

export function SessionSearchPortal({
  tr,
  open,
  setOpen,
  query,
  setQuery,
  returnFocusRef,
  hits,
  projects,
  sessions,
  activeProject,
  openSession,
  newChat,
  addProject,
  setProjectsOpen,
  setExpandedProjects,
}: SessionSearchPortalProps) {
  if (!open || typeof document === "undefined") return null;

  return createPortal(
    <div className="overlay search-overlay" onClick={() => setOpen(false)}>
      <div
        className="search-panel"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
        aria-label={tr("sidebar.search")}
      >
        <div className="search-panel__head">
          <IconSearch size={16} />
          <Input
            autoFocus
            className="search-panel__input"
            placeholder={tr("search.placeholder")}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          <Button
            type="button"
            className="icon-btn modal-close"
            onClick={() => setOpen(false)}
            aria-label={tr("common.close")}
          >
            <IconClose size={16} />
          </Button>
        </div>
        {hits.matchedProjects.length > 0 ? (
          <>
            <div className="search-panel__section">
              {tr("sidebar.projects")}
            </div>
            {hits.matchedProjects.map((project) => (
              <Button
                key={project.id}
                type="button"
                className="search-panel__row"
                onClick={() => {
                  setOpen(false);
                  // Project is a folder: expand only; selection is for sessions.
                  setProjectsOpen(true);
                  setExpandedProjects((expanded) => ({
                    ...expanded,
                    [project.id]: true,
                  }));
                }}
              >
                <IconFolder size={15} />
                <span className="search-panel__title">{project.name}</span>
                <span className="search-panel__meta">{project.path}</span>
              </Button>
            ))}
          </>
        ) : null}
        <div className="search-panel__section">{tr("search.chats")}</div>
        {hits.matchedSessions.length === 0 ? (
          <div className="sidebar-empty" style={{ padding: 12 }}>
            {tr("search.noMatches")}
          </div>
        ) : null}
        {hits.matchedSessions.map((hit, index) => {
          const row = sessions.find((session) => session.id === hit.id);
          if (!row) return null;
          const project = projects.find(
            (candidate) => candidate.id === row.projectId,
          );
          const metaParts: string[] = [];
          if (project?.name) metaParts.push(project.name);
          if (index < 9) metaParts.push(`⌘${index + 1}`);
          return (
            <Button
              key={hit.id}
              type="button"
              className="search-panel__row"
              onClick={() => {
                setOpen(false);
                void openSession(row, project ?? null);
              }}
            >
              <IconNewChat size={15} />
              <span className="search-panel__body">
                <span className="search-panel__title">{row.title}</span>
              </span>
              <span className="search-panel__meta">
                {metaParts.join(" · ") || "—"}
              </span>
            </Button>
          );
        })}
        <div className="search-panel__foot">
          <Button
            type="button"
            className="search-panel__row"
            onClick={() => {
              setOpen(false);
              void newChat(activeProject);
            }}
          >
            <IconNewChat size={15} />
            <span className="search-panel__title">
              {tr("search.newChat")}
            </span>
          </Button>
          <Button
            type="button"
            className="search-panel__row"
            onClick={() => {
              setOpen(false);
              void addProject(returnFocusRef.current);
            }}
          >
            <IconFolder size={15} />
            <span className="search-panel__title">
              {tr("sidebar.addProject")}
            </span>
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
