import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";
import { useFloatingMenu, type FloatingPos } from "@/lib/floatingMenu";
import { applySkillAtSlash, detectSlashQueryFromEditor } from "@/lib/draftDoc";
import {
  buildSlashCatalog,
  flattenFilteredCatalog,
  type SkillInfo,
  type SlashItem,
} from "@/lib/slashCatalog";
import {
  buildComposerPlusEntries,
  uploadMatchesQuery,
  type ComposerPlusEntry,
} from "@/components/ComposerPlusPanel";
import { createT, type Locale, type MessageKey } from "@/i18n";
import type {
  ComposerApiPort,
  Ref,
  StateSetter,
} from "../useComposerController";

type SlashQuery = { start: number; query: string; end: number };
type LiveSlash = {
  present: boolean;
  query: string;
  start: number;
  end: number;
};

const EMPTY_LIVE_SLASH: LiveSlash = {
  present: false,
  query: "",
  start: 0,
  end: 0,
};

export interface UseComposerSlashMenuOptions {
  locale: Locale;
  api: ComposerApiPort;
  projectPath: string | null;
  setDraft: StateSetter<string>;
  onAction: (action: string) => void;
  composerInputRef: RefObject<HTMLDivElement | null>;
  composerShellRef: RefObject<HTMLDivElement | null>;
  composerPlusTriggerRef: RefObject<HTMLButtonElement | null>;
  composerPlusPanelRef: RefObject<HTMLDivElement | null>;
}

export interface ComposerSlashMenuController {
  skillsLoading: boolean;
  liveSlash: LiveSlash;
  liveSlashRef: Ref<LiveSlash>;
  slashQuery: SlashQuery | null;
  composerMenuOpen: boolean;
  showComposerPlus: boolean;
  setShowComposerPlus: StateSetter<boolean>;
  slashActiveIndex: number;
  setSlashActiveIndex: StateSetter<number>;
  slashFilterQuery: string;
  composerMenuEntries: ComposerPlusEntry[];
  composerMenuEntriesRef: Ref<ComposerPlusEntry[]>;
  resolveSlashTitle: (item: SlashItem) => string;
  resolveSlashDescription: (item: SlashItem) => string;
  onSlashQueryChange: (query: SlashQuery | null) => void;
  closeComposerMenu: () => void;
  applySlashItem: (item: SlashItem) => void;
  composerPlusPos: FloatingPos | null;
  composerPlusStyle: CSSProperties | undefined;
  composerPlusPanelRef: RefObject<HTMLDivElement | null>;
}

/** Owns slash detection, skill loading, palette state, filtering, and placement. */
export function useComposerSlashMenu({
  locale,
  api,
  projectPath,
  setDraft,
  onAction,
  composerInputRef,
  composerShellRef,
  composerPlusTriggerRef,
  composerPlusPanelRef,
}: UseComposerSlashMenuOptions): ComposerSlashMenuController {
  const tr = useMemo(() => createT(locale), [locale]);
  const apiRef = useRef(api);
  apiRef.current = api;
  const [skillInfos, setSkillInfos] = useState<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = useState(false);
  const [showComposerPlus, setShowComposerPlus] = useState(false);
  const showComposerPlusRef = useRef(false);
  showComposerPlusRef.current = showComposerPlus;
  const [slashQuery, setSlashQuery] = useState<SlashQuery | null>(null);
  const slashQueryRef = useRef<SlashQuery | null>(null);
  slashQueryRef.current = slashQuery;
  const [liveSlash, setLiveSlash] = useState<LiveSlash>(EMPTY_LIVE_SLASH);
  const liveSlashRef = useRef(liveSlash);
  liveSlashRef.current = liveSlash;
  const slashDismissedSigRef = useRef<string | null>(null);
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);

  useEffect(() => {
    if (!apiRef.current.isTauri()) return;
    let cancelled = false;
    setSkillsLoading(true);
    void apiRef.current
      .skillsList(projectPath)
      .then((result) => {
        if (cancelled) return;
        setSkillInfos(
          result.skills.map((skill) => ({
            name: skill.name,
            description: skill.description ?? "",
            source: skill.source,
            userInvocable: skill.userInvocable,
          })),
        );
      })
      .catch(() => {
        if (!cancelled) setSkillInfos([]);
      })
      .finally(() => {
        if (!cancelled) setSkillsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectPath]);

  const slashCatalog = useMemo(() => buildSlashCatalog(skillInfos), [skillInfos]);
  const resolveSlashTitle = useCallback(
    (item: SlashItem) => {
      if (item.titleKey) {
        try {
          return tr(item.titleKey as MessageKey);
        } catch {
          // Dynamic skill labels do not have i18n keys.
        }
      }
      return item.displayTitle || item.name;
    },
    [tr],
  );
  const resolveSlashDescription = useCallback(
    (item: SlashItem) => {
      if (item.descriptionKey) {
        try {
          return tr(item.descriptionKey as MessageKey);
        } catch {
          // Dynamic skill descriptions are already localized by the source.
        }
      }
      return item.displayDescription || "";
    },
    [tr],
  );
  const slashFilterQuery = liveSlash.present ? liveSlash.query : "";
  const slashFiltered = useMemo(
    () =>
      flattenFilteredCatalog(slashCatalog, slashFilterQuery, (item) => ({
        title: resolveSlashTitle(item),
        description: resolveSlashDescription(item),
      })),
    [
      resolveSlashDescription,
      resolveSlashTitle,
      slashCatalog,
      slashFilterQuery,
    ],
  );
  const showUploadInMenu = useMemo(
    () =>
      uploadMatchesQuery(slashFilterQuery, {
        title: tr("composer.addFiles"),
        hint: tr("composer.addFilesHint"),
      }),
    [slashFilterQuery, tr],
  );
  const composerMenuEntries = useMemo(
    () =>
      buildComposerPlusEntries({
        showUpload: showUploadInMenu,
        commands: slashFiltered.commands,
        skills: slashFiltered.skills,
      }),
    [showUploadInMenu, slashFiltered.commands, slashFiltered.skills],
  );
  const composerMenuEntriesRef = useRef(composerMenuEntries);
  composerMenuEntriesRef.current = composerMenuEntries;
  const composerMenuOpen = showComposerPlus || liveSlash.present;

  const onSlashQueryChange = useCallback((query: SlashQuery | null) => {
    setSlashQuery((previous) => {
      if (query == null) return previous == null ? previous : null;
      if (
        previous?.start === query.start &&
        previous.query === query.query &&
        previous.end === query.end
      ) {
        return previous;
      }
      return query;
    });
    slashQueryRef.current = query;
  }, []);

  const closeComposerMenu = useCallback(() => {
    const live = liveSlashRef.current;
    if (live.present) {
      slashDismissedSigRef.current = `${live.start}:${live.query}`;
    }
    setShowComposerPlus(false);
    setSlashQuery(null);
    slashQueryRef.current = null;
    liveSlashRef.current = EMPTY_LIVE_SLASH;
    setLiveSlash(EMPTY_LIVE_SLASH);
  }, []);

  useEffect(() => {
    let animationFrame = 0;
    let alive = true;
    const tick = () => {
      if (!alive) return;
      const detected = detectSlashQueryFromEditor(composerInputRef.current);
      let next: LiveSlash = detected
        ? {
            present: true,
            query: detected.query,
            start: detected.start,
            end: detected.end,
          }
        : EMPTY_LIVE_SLASH;
      if (next.present && slashDismissedSigRef.current != null) {
        const signature = `${next.start}:${next.query}`;
        if (signature === slashDismissedSigRef.current) {
          next = EMPTY_LIVE_SLASH;
        } else {
          slashDismissedSigRef.current = null;
        }
      }
      if (!next.present && detected == null) {
        slashDismissedSigRef.current = null;
      }
      const previous = liveSlashRef.current;
      if (
        previous.present !== next.present ||
        previous.query !== next.query ||
        previous.start !== next.start ||
        previous.end !== next.end
      ) {
        liveSlashRef.current = next;
        setLiveSlash(next);
        if (next.present) {
          onSlashQueryChange({
            start: next.start,
            query: next.query,
            end: next.end,
          });
        } else if (!showComposerPlusRef.current) {
          onSlashQueryChange(null);
        }
      }
      animationFrame = requestAnimationFrame(tick);
    };
    animationFrame = requestAnimationFrame(tick);
    return () => {
      alive = false;
      cancelAnimationFrame(animationFrame);
    };
  }, [composerInputRef, onSlashQueryChange]);

  const applySlashItem = useCallback(
    (item: SlashItem) => {
      const live = liveSlashRef.current;
      const query =
        slashQueryRef.current ??
        (live.present
          ? { start: live.start, query: live.query, end: live.end }
          : null);
      onSlashQueryChange(null);
      liveSlashRef.current = EMPTY_LIVE_SLASH;
      setLiveSlash(EMPTY_LIVE_SLASH);
      setShowComposerPlus(false);

      if (item.kind === "skill") {
        if (query) {
          setDraft((value) =>
            applySkillAtSlash(value, query.start, query.end, item.name),
          );
        } else {
          setDraft((value) => {
            const needsSpace = value.length > 0 && !/\s$/.test(value);
            return `${value}${needsSpace ? " " : ""}[[skill:${item.name}]] `;
          });
        }
        return;
      }
      if (query) {
        setDraft((value) => value.slice(0, query.start) + value.slice(query.end));
      }
      if (item.kind === "action") onAction(item.action ?? item.name);
    },
    [onAction, onSlashQueryChange, setDraft],
  );

  useEffect(() => {
    setSlashActiveIndex((index) =>
      composerMenuEntries.length
        ? Math.min(index, composerMenuEntries.length - 1)
        : 0,
    );
  }, [composerMenuEntries.length]);

  const { pos: composerPlusPos, style: composerPlusStyle } = useFloatingMenu({
    open: composerMenuOpen,
    triggerRef: composerShellRef,
    panelRef: composerPlusPanelRef,
    roots: [composerPlusTriggerRef, composerShellRef, composerInputRef],
    onClose: closeComposerMenu,
    placement: "up",
    fitContent: false,
    matchTriggerWidth: true,
    minWidth: 280,
    estHeight: 220,
    gap: 8,
    deps: [slashFilterQuery, composerMenuEntries.length],
  });

  return {
    skillsLoading,
    liveSlash,
    liveSlashRef,
    slashQuery,
    composerMenuOpen,
    showComposerPlus,
    setShowComposerPlus,
    slashActiveIndex,
    setSlashActiveIndex,
    slashFilterQuery,
    composerMenuEntries,
    composerMenuEntriesRef,
    resolveSlashTitle,
    resolveSlashDescription,
    onSlashQueryChange,
    closeComposerMenu,
    applySlashItem,
    composerPlusPos,
    composerPlusStyle,
    composerPlusPanelRef,
  };
}
