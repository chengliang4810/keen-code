import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";
import { createT, type Locale } from "@/i18n";
import { useFloatingMenu, type FloatingPos } from "@/lib/floatingMenu";
import {
  collectUserPromptHistory,
  filterPromptHistory,
  type PromptHistoryEntry,
} from "@/lib/composerPromptHistory";
import type { ChatMessage } from "@/lib/session";
import type {
  ComposerFeedbackPort,
  Ref,
  StateSetter,
} from "../useComposerController";

export interface UseComposerPromptHistoryOptions {
  locale: Locale;
  messages: ChatMessage[];
  setDraft: StateSetter<string>;
  feedback: ComposerFeedbackPort;
  closeComposerMenu: () => void;
  composerInputRef: RefObject<HTMLDivElement | null>;
  composerShellRef: RefObject<HTMLDivElement | null>;
}

export interface ComposerPromptHistoryController {
  handleDraftChange: (next: string) => void;
  promptHistoryIndex: number | null;
  promptHistoryIndexRef: Ref<number | null>;
  setPromptHistoryIndex: StateSetter<number | null>;
  promptHistoryOpen: boolean;
  promptHistoryOpenRef: Ref<boolean>;
  setPromptHistoryOpen: StateSetter<boolean>;
  promptHistoryFilter: string;
  setPromptHistoryFilter: StateSetter<string>;
  promptHistoryActive: number;
  setPromptHistoryActive: StateSetter<number>;
  promptHistoryFocusFilter: boolean;
  setPromptHistoryFocusFilter: StateSetter<boolean>;
  promptHistoryEntries: PromptHistoryEntry[];
  promptHistoryPanelRef: Ref<HTMLDivElement | null>;
  closePromptHistory: () => void;
  openPromptHistory: (options?: {
    focusFilter?: boolean;
    seedDraft?: boolean;
  }) => void;
  applyPromptHistoryEntry: (
    entry: PromptHistoryEntry,
    options?: { close?: boolean; listIndex?: number },
  ) => void;
  promptHistoryPos: FloatingPos | null;
  promptHistoryStyle: CSSProperties | undefined;
}

/** Owns prompt-history state, keyboard selection, and its floating panel. */
export function useComposerPromptHistory({
  locale,
  messages,
  setDraft,
  feedback,
  closeComposerMenu,
  composerInputRef,
  composerShellRef,
}: UseComposerPromptHistoryOptions): ComposerPromptHistoryController {
  const tr = useMemo(() => createT(locale), [locale]);
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const [promptHistoryIndex, setPromptHistoryIndex] = useState<number | null>(
    null,
  );
  const promptHistoryIndexRef = useRef<number | null>(null);
  promptHistoryIndexRef.current = promptHistoryIndex;
  const [promptHistoryOpen, setPromptHistoryOpen] = useState(false);
  const promptHistoryOpenRef = useRef(false);
  promptHistoryOpenRef.current = promptHistoryOpen;
  const [promptHistoryFilter, setPromptHistoryFilter] = useState("");
  const [promptHistoryActive, setPromptHistoryActive] = useState(0);
  const [promptHistoryFocusFilter, setPromptHistoryFocusFilter] =
    useState(false);

  const sessionPromptHistory = useMemo(
    () => collectUserPromptHistory(messages),
    [messages],
  );
  const promptHistoryEntries = useMemo(
    () => filterPromptHistory(sessionPromptHistory, promptHistoryFilter),
    [promptHistoryFilter, sessionPromptHistory],
  );
  const promptHistoryPanelRef = useRef<HTMLDivElement | null>(null);

  const closePromptHistory = useCallback(() => {
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
  }, []);

  const applyPromptHistoryEntry = useCallback(
    (
      entry: PromptHistoryEntry,
      options?: { close?: boolean; listIndex?: number },
    ) => {
      promptHistoryIndexRef.current = entry.historyIndex;
      setPromptHistoryIndex(entry.historyIndex);
      if (typeof options?.listIndex === "number") {
        setPromptHistoryActive(options.listIndex);
      }
      setDraft(entry.text);
      if (options?.close !== false) {
        closePromptHistory();
        requestAnimationFrame(() => composerInputRef.current?.focus());
      }
    },
    [closePromptHistory, composerInputRef, setDraft],
  );

  const openPromptHistory = useCallback(
    (options?: { focusFilter?: boolean; seedDraft?: boolean }) => {
      const history = collectUserPromptHistory(messagesRef.current);
      if (!history.length) {
        feedback.showToast(tr("slash.historyEmpty"), 2400);
        return;
      }
      closeComposerMenu();
      setPromptHistoryFilter("");
      setPromptHistoryActive(0);
      setPromptHistoryFocusFilter(options?.focusFilter === true);
      setPromptHistoryOpen(true);
      if (options?.seedDraft !== false) {
        promptHistoryIndexRef.current = 0;
        setPromptHistoryIndex(0);
        setDraft(history[0] ?? "");
      }
    },
    [closeComposerMenu, feedback, setDraft, tr],
  );

  const handleDraftChange = useCallback(
    (next: string) => {
      setDraft(next);
      const index = promptHistoryIndexRef.current;
      if (index == null) return;
      const history = collectUserPromptHistory(messagesRef.current);
      if (next !== history[index]) {
        promptHistoryIndexRef.current = null;
        setPromptHistoryIndex(null);
      }
    },
    [setDraft],
  );

  useEffect(() => {
    if (!promptHistoryOpen) return;
    setPromptHistoryActive((index) => {
      if (!promptHistoryEntries.length) return 0;
      return Math.min(index, promptHistoryEntries.length - 1);
    });
  }, [promptHistoryEntries.length, promptHistoryOpen]);

  const previousFilterRef = useRef(promptHistoryFilter);
  useEffect(() => {
    if (!promptHistoryOpen) return;
    if (previousFilterRef.current === promptHistoryFilter) return;
    previousFilterRef.current = promptHistoryFilter;
    setPromptHistoryActive(0);
  }, [promptHistoryFilter, promptHistoryOpen]);

  const { pos: promptHistoryPos, style: promptHistoryStyle } = useFloatingMenu({
    open: promptHistoryOpen,
    triggerRef: composerShellRef,
    panelRef: promptHistoryPanelRef,
    roots: [composerShellRef, composerInputRef, promptHistoryPanelRef],
    onClose: closePromptHistory,
    placement: "up",
    fitContent: false,
    matchTriggerWidth: true,
    minWidth: 280,
    estHeight: 280,
    gap: 8,
    deps: [promptHistoryFilter, promptHistoryEntries.length],
  });

  return {
    handleDraftChange,
    promptHistoryIndex,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    promptHistoryOpen,
    promptHistoryOpenRef,
    setPromptHistoryOpen,
    promptHistoryFilter,
    setPromptHistoryFilter,
    promptHistoryActive,
    setPromptHistoryActive,
    promptHistoryFocusFilter,
    setPromptHistoryFocusFilter,
    promptHistoryEntries,
    promptHistoryPanelRef,
    closePromptHistory,
    openPromptHistory,
    applyPromptHistoryEntry,
    promptHistoryPos,
    promptHistoryStyle,
  };
}
