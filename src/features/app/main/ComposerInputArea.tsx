import { createPortal } from "react-dom";
import type {
  CSSProperties,
  Dispatch,
  KeyboardEvent as ReactKeyboardEvent,
  MutableRefObject,
  RefObject,
  SetStateAction,
} from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import type { Attachment } from "@/lib/attachments";
import type { ComposerPlusEntry } from "@/components/ComposerPlusPanel";
import type { FloatingPos } from "@/lib/floatingMenu";
import type { SlashItem } from "@/lib/slashCatalog";
import type { PromptHistoryEntry } from "@/lib/composerPromptHistory";
import { ComposerEditor } from "@/components/ComposerEditor";
import {
  ComposerPlusPanel,
} from "@/components/ComposerPlusPanel";
import { PromptHistoryPanel } from "@/components/PromptHistoryPanel";
import { canType } from "@/lib/session";
import { isDraftEmpty, parseStoredContent } from "@/lib/draftDoc";
import {
  collectUserPromptHistory,
  shouldHandlePromptHistoryKey,
  stepPromptHistory,
} from "@/lib/composerPromptHistory";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

export interface ComposerInputAreaProps {
  locale: Locale;
  tr: Translator;
  session: SessionSnapshot;
  messages: ChatMessage[];
  draft: string;
  setDraft: SetState<string>;
  handleDraftChange: (value: string) => void;
  attachments: Attachment[];
  addPastedFiles: (files: File[]) => Promise<void>;
  addAttachmentsFromPaths: (paths: string[]) => Promise<void>;
  pickComposerFiles: () => Promise<void>;
  composerInputRef: RefObject<HTMLDivElement | null>;
  composerMenuOpen: boolean;
  composerMenuEntries: ComposerPlusEntry[];
  composerMenuEntriesRef: MutableRefObject<ComposerPlusEntry[]>;
  slashActiveIndex: number;
  setSlashActiveIndex: SetState<number>;
  applySlashItem: (item: SlashItem) => void;
  liveSlash: { present: boolean; query: string; start: number; end: number };
  slashFilterQuery: string;
  skillsLoading: boolean;
  composerPlusPos: FloatingPos | null;
  composerPlusStyle?: CSSProperties;
  composerPlusPanelRef: RefObject<HTMLDivElement | null>;
  resolveSlashTitle: (item: SlashItem) => string;
  resolveSlashDescription: (item: SlashItem) => string;
  promptHistoryOpen: boolean;
  promptHistoryPos: FloatingPos | null;
  promptHistoryStyle?: CSSProperties;
  promptHistoryPanelRef: RefObject<HTMLDivElement | null>;
  promptHistoryEntries: PromptHistoryEntry[];
  promptHistoryActive: number;
  setPromptHistoryActive: SetState<number>;
  promptHistoryFocusFilter: boolean;
  promptHistoryFilter: string;
  setPromptHistoryFilter: SetState<string>;
  promptHistoryOpenRef: MutableRefObject<boolean>;
  promptHistoryIndexRef: MutableRefObject<number | null>;
  setPromptHistoryIndex: SetState<number | null>;
  closePromptHistory: () => void;
  openPromptHistory: (options?: {
    focusFilter?: boolean;
    seedDraft?: boolean;
  }) => void;
  applyPromptHistoryEntry: (
    entry: PromptHistoryEntry,
    options?: { close?: boolean; listIndex?: number },
  ) => void;
  closeComposerMenu: () => void;
  onSlashQueryChange: (
    query: { start: number; query: string; end: number } | null,
  ) => void;
  send: () => Promise<void>;
  hasConfiguredModel: boolean;
}

export function ComposerInputArea({
  locale,
  tr,
  session,
  messages,
  draft,
  setDraft,
  handleDraftChange,
  attachments,
  addPastedFiles,
  addAttachmentsFromPaths,
  pickComposerFiles,
  composerInputRef,
  composerMenuOpen,
  composerMenuEntries,
  composerMenuEntriesRef,
  slashActiveIndex,
  setSlashActiveIndex,
  applySlashItem,
  liveSlash,
  slashFilterQuery,
  skillsLoading,
  composerPlusPos,
  composerPlusStyle,
  composerPlusPanelRef,
  resolveSlashTitle,
  resolveSlashDescription,
  promptHistoryOpen,
  promptHistoryPos,
  promptHistoryStyle,
  promptHistoryPanelRef,
  promptHistoryEntries,
  promptHistoryActive,
  setPromptHistoryActive,
  promptHistoryFocusFilter,
  promptHistoryFilter,
  setPromptHistoryFilter,
  promptHistoryOpenRef,
  promptHistoryIndexRef,
  setPromptHistoryIndex,
  closePromptHistory,
  openPromptHistory,
  applyPromptHistoryEntry,
  closeComposerMenu,
  onSlashQueryChange,
  send,
  hasConfiguredModel,
}: ComposerInputAreaProps) {
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (
      event.nativeEvent.isComposing ||
      (event.nativeEvent as KeyboardEvent).keyCode === 229
    ) {
      return;
    }

    if (composerMenuOpen) {
      // Ref = same array the panel renders (never desync).
      const entries = composerMenuEntriesRef.current;
      const count = entries.length;
      if (event.key === "ArrowDown") {
        event.preventDefault();
        if (!count) return;
        setSlashActiveIndex((index) => (index + 1) % count);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        if (!count) return;
        setSlashActiveIndex((index) => (index - 1 + count) % count);
        return;
      }
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        const entry =
          entries[
            Math.min(
              Math.max(0, slashActiveIndex),
              Math.max(0, count - 1),
            )
          ];
        if (!entry) return;
        if (entry.kind === "upload") void pickComposerFiles();
        else applySlashItem(entry.item);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeComposerMenu();
        return;
      }
      if (event.key === "Tab" && count > 0) {
        event.preventDefault();
        const entry =
          entries[Math.min(Math.max(0, slashActiveIndex), count - 1)]!;
        if (entry.kind === "upload") void pickComposerFiles();
        else applySlashItem(entry.item);
        return;
      }
    }

    // Prompt history picker open: ↑/↓ move selection; Enter/Tab apply; Esc closes.
    if (promptHistoryOpenRef.current && !composerMenuOpen) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePromptHistory();
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        const entry = promptHistoryEntries[promptHistoryActive];
        if (entry) {
          event.preventDefault();
          applyPromptHistoryEntry(entry, {
            listIndex: promptHistoryActive,
          });
          return;
        }
      }
      if (event.key === "ArrowUp" || event.key === "ArrowDown") {
        event.preventDefault();
        if (promptHistoryEntries.length === 0) return;
        if (event.key === "ArrowUp") {
          const next = Math.min(
            promptHistoryActive + 1,
            promptHistoryEntries.length - 1,
          );
          setPromptHistoryActive(next);
          const entry = promptHistoryEntries[next];
          if (entry) {
            applyPromptHistoryEntry(entry, {
              close: false,
              listIndex: next,
            });
          }
          return;
        }
        // ArrowDown: newer; past newest closes like Build.
        if (promptHistoryActive <= 0) {
          promptHistoryIndexRef.current = null;
          setPromptHistoryIndex(null);
          setDraft("");
          closePromptHistory();
          return;
        }
        const next = promptHistoryActive - 1;
        setPromptHistoryActive(next);
        const entry = promptHistoryEntries[next];
        if (entry) {
          applyPromptHistoryEntry(entry, {
            close: false,
            listIndex: next,
          });
        }
        return;
      }
    }

    // Empty draft ↑ opens history; existing browsing supports ↑/↓ stepping.
    if (
      (event.key === "ArrowUp" || event.key === "ArrowDown") &&
      !composerMenuOpen &&
      !promptHistoryOpenRef.current
    ) {
      const history = collectUserPromptHistory(messages);
      const draftEmpty = isDraftEmpty(parseStoredContent(draft));
      const browsing = promptHistoryIndexRef.current !== null;
      if (
        shouldHandlePromptHistoryKey({
          key: event.key,
          draftEmpty,
          browsing,
          historyLength: history.length,
        })
      ) {
        event.preventDefault();
        if (event.key === "ArrowUp" && !browsing) {
          openPromptHistory({
            focusFilter: false,
            seedDraft: true,
          });
          return;
        }
        const step = stepPromptHistory(
          history,
          promptHistoryIndexRef.current,
          event.key === "ArrowUp" ? "up" : "down",
        );
        promptHistoryIndexRef.current = step.index;
        setPromptHistoryIndex(step.index);
        setDraft(step.text);
        if (step.index == null) {
          closePromptHistory();
        } else if (!promptHistoryOpenRef.current) {
          openPromptHistory({
            focusFilter: false,
            seedDraft: false,
          });
          setPromptHistoryActive(step.index);
        } else {
          setPromptHistoryActive(step.index);
        }
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const hasBody =
        !isDraftEmpty(parseStoredContent(draft)) || attachments.length > 0;
      if (hasBody && hasConfiguredModel) void send();
    }
    if (event.key === "Escape") {
      if (promptHistoryOpenRef.current) {
        closePromptHistory();
        return;
      }
      closeComposerMenu();
    }
  };

  return (
    <>
      {composerMenuOpen && composerPlusPos && typeof document !== "undefined"
        ? createPortal(
            <ComposerPlusPanel
              open
              panelRef={composerPlusPanelRef}
              locale={locale}
              entries={composerMenuEntries}
              filterQuery={liveSlash.present ? slashFilterQuery : undefined}
              skillsLoading={skillsLoading}
              activeIndex={slashActiveIndex}
              onActiveIndexChange={setSlashActiveIndex}
              onSelectUpload={() => void pickComposerFiles()}
              onSelectSlash={applySlashItem}
              resolveTitle={resolveSlashTitle}
              resolveDescription={resolveSlashDescription}
              style={{ ...composerPlusStyle, zIndex: 10050 }}
            />,
            document.body,
          )
        : null}
      {promptHistoryOpen && promptHistoryPos && typeof document !== "undefined"
        ? createPortal(
            <PromptHistoryPanel
              open
              panelRef={promptHistoryPanelRef}
              entries={promptHistoryEntries}
              query={promptHistoryFilter}
              activeIndex={promptHistoryActive}
              focusFilter={promptHistoryFocusFilter}
              labels={{
                title: tr("promptHistory.title"),
                placeholder: tr("promptHistory.placeholder"),
                empty: tr("promptHistory.empty"),
                emptyFilter: tr("promptHistory.emptyFilter"),
                aria: tr("promptHistory.aria"),
              }}
              onQueryChange={setPromptHistoryFilter}
              onActiveIndexChange={(index) => {
                setPromptHistoryActive(index);
                const entry = promptHistoryEntries[index];
                if (entry && !promptHistoryFocusFilter) {
                  applyPromptHistoryEntry(entry, {
                    close: false,
                    listIndex: index,
                  });
                }
              }}
              onSelect={(entry) => applyPromptHistoryEntry(entry)}
              onClose={closePromptHistory}
              style={{ ...promptHistoryStyle, zIndex: 10050 }}
            />,
            document.body,
          )
        : null}
      <ComposerEditor
        editorRef={composerInputRef}
        className="composer__input"
        value={draft}
        disabled={!canType(session.state)}
        placeholder={tr("composer.placeholder")}
        onChange={handleDraftChange}
        onSlashQueryChange={onSlashQueryChange}
        onPasteFiles={(files) => void addPastedFiles(files)}
        onPastePaths={(paths) => void addAttachmentsFromPaths(paths)}
        onKeyDown={onKeyDown}
      />
    </>
  );
}
