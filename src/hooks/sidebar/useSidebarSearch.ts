import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { filterSessionSearch, type SessionSearchHits } from "@/lib/sessionSearch";

export interface SidebarSearchOptions {
  projects: Project[];
  sessions: SessionRow[];
  composerInputRef?: RefObject<HTMLElement | null>;
}

export interface SidebarSearchResult {
  showSearch: boolean;
  setShowSearch: React.Dispatch<React.SetStateAction<boolean>>;
  searchQuery: string;
  setSearchQuery: React.Dispatch<React.SetStateAction<string>>;
  searchHits: SessionSearchHits;
  searchTriggerRef: RefObject<HTMLButtonElement | null>;
  searchReturnFocusRef: React.MutableRefObject<HTMLElement | null>;
  openSearch: () => void;
}

export function useSidebarSearch({
  projects,
  sessions,
  composerInputRef,
}: SidebarSearchOptions): SidebarSearchResult {
  const [showSearch, setShowSearch] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const searchTriggerRef = useRef<HTMLButtonElement>(null);
  const searchReturnFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!showSearch) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setShowSearch(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [showSearch]);

  const openSearch = useCallback(() => {
    const active = document.activeElement;
    const activeSidebarWidth =
      active instanceof HTMLElement
        ? active.closest(".sidebar")?.getBoundingClientRect().width
        : null;
    const sidebarWidth = searchTriggerRef.current
      ?.closest(".sidebar")
      ?.getBoundingClientRect().width;
    searchReturnFocusRef.current =
      active instanceof HTMLElement &&
      active !== document.body &&
      activeSidebarWidth !== 0
        ? active
        : sidebarWidth
          ? searchTriggerRef.current
          : composerInputRef?.current ?? null;
    setSearchQuery("");
    setShowSearch(true);
  }, [composerInputRef]);

  const searchHits = useMemo(
    () =>
      filterSessionSearch(
        searchQuery,
        sessions.map((item) => ({
          id: item.id,
          title: item.title,
          projectId: item.projectId,
          archived: item.archived,
        })),
        projects.map((project) => ({
          id: project.id,
          name: project.name,
          path: project.path,
        })),
      ),
    [projects, searchQuery, sessions],
  );

  return {
    showSearch,
    setShowSearch,
    searchQuery,
    setSearchQuery,
    searchHits,
    searchTriggerRef,
    searchReturnFocusRef,
    openSearch,
  };
}
