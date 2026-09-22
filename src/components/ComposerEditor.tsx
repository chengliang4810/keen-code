/**
 * Contenteditable composer: plain text + inline skill chips.
 * Value is stored form with [[skill:name]] tokens.
 *
 * Slash filter: parent also derives query from `value` (draft). This editor
 * still emits caret-based slashQuery for mid-line tokens and live IME updates.
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type CompositionEvent,
  type DragEventHandler,
  type FormEvent,
  type KeyboardEvent,
  type Ref,
} from "react";
import { assignRef } from "@/lib/reactRefs";
import {
  clipboardLooksLikeMedia,
  clipboardPlainText,
  collectFilesFromDataTransfer,
  collectLocalPathsFromDataTransfer,
  isFileUrlOnlyText,
  readClipboardMediaFiles,
} from "@/lib/clipboardPaste";
import {
  detectSlashQuery,
  parseStoredContent,
  segmentsFromEditorDom,
  serializeStored,
  type DraftSegment,
} from "@/lib/draftDoc";
import {
  composerMentionTriggerForKind,
  detectComposerMentionQuery,
  encodeComposerMention,
  removeComposerMentionAtCaret,
  type ComposerMentionQuery,
} from "@/lib/composerMentions";

function clearNode(el: HTMLElement) {
  while (el.firstChild) el.removeChild(el.firstChild);
}

function appendTextWithBreaks(el: HTMLElement, text: string) {
  const parts = text.split("\n");
  parts.forEach((part, i) => {
    if (part) el.appendChild(document.createTextNode(part));
    if (i < parts.length - 1) el.appendChild(document.createElement("br"));
  });
}

function makeSkillChipEl(name: string): HTMLElement {
  const wrap = document.createElement("span");
  wrap.className = "skill-chip skill-chip--sm skill-chip--editor";
  wrap.contentEditable = "false";
  wrap.dataset.skill = name;
  wrap.setAttribute("data-skill", name);

  const icon = document.createElement("span");
  icon.className = "skill-chip__glyph";
  icon.setAttribute("aria-hidden", "true");
  icon.textContent = "⚒";

  const label = document.createElement("span");
  label.className = "skill-chip__name";
  label.textContent = name;

  wrap.appendChild(icon);
  wrap.appendChild(label);
  return wrap;
}

function makeMentionChipEl(mention: Extract<DraftSegment, { type: "mention" }>["mention"]): HTMLElement {
  const wrap = document.createElement("span");
  wrap.className = `composer-mention composer-mention--${mention.kind}`;
  wrap.contentEditable = "false";
  wrap.dataset.composerMention = encodeComposerMention(mention);
  // 结构化数据由 draftDoc 通过 token 解码；这里同时保留可读属性便于 WebView
  // 辅助技术和调试查看，不依赖可见文本反推 mention 身份。
  wrap.dataset.mentionId = mention.id;
  wrap.dataset.mentionKind = mention.kind;
  wrap.dataset.mentionValue = mention.value;
  const visibleTrigger = composerMentionTriggerForKind(mention.kind);
  wrap.setAttribute("aria-label", `${visibleTrigger}${mention.label}`);
  wrap.textContent = `${visibleTrigger}${mention.label}`;
  return wrap;
}

function renderSegmentsInto(el: HTMLElement, segments: DraftSegment[]) {
  clearNode(el);
  for (const seg of segments) {
    if (seg.type === "text") {
      appendTextWithBreaks(el, seg.text);
    } else if (seg.type === "skill") {
      el.appendChild(makeSkillChipEl(seg.name));
    } else {
      el.appendChild(makeMentionChipEl(seg.mention));
    }
  }
}

function serializeDom(el: HTMLElement): string {
  const segs = segmentsFromEditorDom(el);
  return serializeStored(segs.length ? segs : [{ type: "text", text: "" }]);
}

function getTextBeforeCaret(el: HTMLElement): string | null {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!el.contains(range.startContainer)) return null;
  const pre = document.createRange();
  pre.selectNodeContents(el);
  pre.setEnd(range.startContainer, range.startOffset);
  const frag = pre.cloneContents();
  const tmp = document.createElement("div");
  tmp.appendChild(frag);
  return serializeDom(tmp);
}

function placeCaretAtEnd(el: HTMLElement) {
  el.focus();
  const sel = window.getSelection();
  if (!sel) return;
  const range = document.createRange();
  range.selectNodeContents(el);
  range.collapse(false);
  sel.removeAllRanges();
  sel.addRange(range);
}

/**
 * Paste as plain text only — strip HTML / rich styles from clipboard.
 * Uses insertText when available (keeps undo); falls back to Range insert.
 */
function insertPlainTextAtSelection(text: string) {
  if (!text) return;
  const plain = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");

  try {
    if (document.queryCommandSupported?.("insertText")) {
      const ok = document.execCommand("insertText", false, plain);
      if (ok) return;
    }
  } catch {
    /* fall through */
  }

  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return;
  const range = sel.getRangeAt(0);
  range.deleteContents();

  const frag = document.createDocumentFragment();
  const parts = plain.split("\n");
  parts.forEach((part, i) => {
    if (part) frag.appendChild(document.createTextNode(part));
    if (i < parts.length - 1) frag.appendChild(document.createElement("br"));
  });
  const last = frag.lastChild;
  range.insertNode(frag);
  if (last) {
    range.setStartAfter(last);
    range.collapse(true);
    sel.removeAllRanges();
    sel.addRange(range);
  }
}

export type ComposerEditorProps = {
  value: string;
  onChange: (stored: string) => void;
  disabled?: boolean;
  ariaLabel: string;
  placeholder?: string;
  className?: string;
  onKeyDown?: (e: KeyboardEvent<HTMLDivElement>) => void;
  onSlashQueryChange?: (
    q: { start: number; query: string; end: number } | null,
  ) => void;
  onMentionQueryChange?: (
    q: ComposerMentionQuery | null,
  ) => void;
  editorRef?: Ref<HTMLDivElement | null>;
  onPasteFiles?: (files: File[]) => void;
  onPastePaths?: (paths: string[]) => void;
  onDragEnter?: DragEventHandler<HTMLDivElement>;
  onDragOver?: DragEventHandler<HTMLDivElement>;
  onDragLeave?: DragEventHandler<HTMLDivElement>;
  onDrop?: DragEventHandler<HTMLDivElement>;
  /**
   * When the paste event looks like media but has no File objects (and async
   * Clipboard API also fails), parent should try native OS clipboard.
   * `expectMedia: true` → show a failure toast if nothing was attached.
   */
  onPasteMediaFallback?: (opts?: {
    expectMedia?: boolean;
  }) => void | Promise<void>;
};

export function ComposerEditor({
  value,
  onChange,
  disabled,
  ariaLabel,
  placeholder,
  className,
  onKeyDown,
  onSlashQueryChange,
  onMentionQueryChange,
  editorRef,
  onPasteFiles,
  onPastePaths,
  onDragEnter,
  onDragOver,
  onDragLeave,
  onDrop,
  onPasteMediaFallback,
}: ComposerEditorProps) {
  const elRef = useRef<HTMLDivElement | null>(null);
  const lastValue = useRef(value);
  const composing = useRef(false);
  const focused = useRef(false);
  /**
   * DOM may show typed / IME glyphs before React `value` commits.
   * Track live emptiness so the overlay placeholder never paints over ink.
   */
  const [domEmpty, setDomEmpty] = useState(() => !value.trim());

  const setRefs = useCallback(
    (node: HTMLDivElement | null) => {
      elRef.current = node;
      assignRef(editorRef, node);
    },
    [editorRef],
  );

  const resize = useCallback(() => {
    const el = elRef.current;
    if (!el) return;
    // 与 ZCode LexicalChatInput 一致：自然排版，CSS 控制 40px 最小高度和 160px 滚动上限。
    el.style.height = "auto";
  }, []);

  const emitSlash = useCallback(() => {
    const el = elRef.current;
    if (!el || !onSlashQueryChange) return;
    const beforeCaret = getTextBeforeCaret(el);
    const full = serializeDom(el);
    // Prefer full text — more reliable after IME confirms 汉字.
    const fromFull = detectSlashQuery(full);
    const fromCaret =
      beforeCaret != null ? detectSlashQuery(beforeCaret) : null;
    const q = fromFull ?? fromCaret;
    if (!q) {
      // During composition the DOM may briefly not contain `/…`; keep prior.
      if (composing.current) return;
      onSlashQueryChange(null);
      return;
    }
    const end = fromFull ? full.length : (beforeCaret?.length ?? full.length);
    onSlashQueryChange({ start: q.start, query: q.query, end });
  }, [onSlashQueryChange]);

  const emitMention = useCallback(() => {
    const el = elRef.current;
    if (!el || !onMentionQueryChange) return;
    const beforeCaret = getTextBeforeCaret(el);
    const query = beforeCaret
      ? detectComposerMentionQuery(beforeCaret)
      : null;
    onMentionQueryChange(query);
  }, [onMentionQueryChange]);

  const syncDomEmpty = useCallback((el: HTMLElement) => {
    const stored = serializeDom(el);
    const empty =
      !stored.trim() ||
      (parseStoredContent(stored).every(
        (s) => s.type === "text" && !s.text.trim(),
      ) &&
        !stored.includes("[[skill:"));
    setDomEmpty(empty);
  }, []);

  const commitFromDom = useCallback(
    (el: HTMLElement) => {
      let stored = serializeDom(el);
      if (
        /\[\[skill:[a-zA-Z0-9_.:-]+\]\]/.test(stored) &&
        !el.querySelector("[data-skill]")
      ) {
        renderSegmentsInto(el, parseStoredContent(stored));
        stored = serializeDom(el);
        placeCaretAtEnd(el);
      }
      syncDomEmpty(el);
      if (stored !== lastValue.current) {
        lastValue.current = stored;
        onChange(stored);
      }
      emitSlash();
      emitMention();
      resize();
    },
    [onChange, emitMention, emitSlash, resize, syncDomEmpty],
  );

  useLayoutEffect(() => {
    const el = elRef.current;
    if (!el) return;
    if (composing.current) return;
    const current = serializeDom(el);
    if (current === value && el.childNodes.length > 0) {
      lastValue.current = value;
      resize();
      return;
    }
    if (focused.current && value === lastValue.current) {
      resize();
      return;
    }
    if (focused.current && value !== lastValue.current) {
      renderSegmentsInto(el, parseStoredContent(value));
      lastValue.current = value;
      placeCaretAtEnd(el);
      resize();
      emitSlash();
      emitMention();
      return;
    }
    renderSegmentsInto(el, parseStoredContent(value));
    lastValue.current = value;
    resize();
    // 外部草稿更新会重建 Observer，不能依赖旧 Observer 通知菜单。
    emitSlash();
    emitMention();
  }, [value, resize, emitMention, emitSlash]);

  const onInput = (e: FormEvent<HTMLDivElement>) => {
    // Hide placeholder as soon as the DOM has glyphs (incl. IME preedit).
    syncDomEmpty(e.currentTarget);
    if (composing.current) {
      // Live pinyin in DOM — update slash filter without committing draft yet.
      emitSlash();
      emitMention();
      resize();
      return;
    }
    commitFromDom(e.currentTarget);
  };

  const onPaste = (e: ClipboardEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();

    // Prefer nativeEvent — React's synthetic clipboardData is empty on some WebViews.
    const cd =
      e.clipboardData ??
      (e.nativeEvent as globalThis.ClipboardEvent | undefined)?.clipboardData ??
      null;

    const files = collectFilesFromDataTransfer(cd);
    const plain = clipboardPlainText(cd);
    const paths = collectLocalPathsFromDataTransfer(cd);
    if (paths.length && onPastePaths) {
      onPastePaths(paths);
    }
    if (!paths.length && files.length && onPasteFiles) {
      onPasteFiles(files);
    } else if (onPasteFiles && clipboardLooksLikeMedia(cd)) {
      // Screenshot paste: event often has image/* types but no File objects.
      void (async () => {
        const asyncFiles = await readClipboardMediaFiles();
        if (asyncFiles.length) {
          onPasteFiles(asyncFiles);
          return;
        }
        await onPasteMediaFallback?.({ expectMedia: true });
      })();
    } else if (!files.length && onPasteMediaFallback) {
      // Empty-looking paste on Mac can still be a pure bitmap clipboard.
      // Only run native fallback when no text is about to be inserted.
      const plainProbe = clipboardPlainText(cd);
      if (!plainProbe.trim()) {
        void (async () => {
          const asyncFiles = await readClipboardMediaFiles();
          if (asyncFiles.length) {
            onPasteFiles?.(asyncFiles);
            return;
          }
          // Soft try — no error toast if clipboard has no image.
          await onPasteMediaFallback({ expectMedia: false });
        })();
      }
    }

    if (!plain) return;
    if ((files.length || paths.length) && isFileUrlOnlyText(plain)) return;
    insertPlainTextAtSelection(plain);
    const el = elRef.current;
    if (el) commitFromDom(el);
  };

  const flushAfterIme = useCallback(
    (el: HTMLElement) => {
      composing.current = false;
      commitFromDom(el);
      // WebKit may finalize the text node after compositionend.
      requestAnimationFrame(() => {
        commitFromDom(el);
        requestAnimationFrame(() => commitFromDom(el));
      });
      window.setTimeout(() => commitFromDom(el), 0);
      window.setTimeout(() => commitFromDom(el), 50);
    },
    [commitFromDom],
  );

  /**
   * Live sync while focused: contenteditable + IME can change the DOM without a
   * clean input event. MutationObserver keeps draft + slash filter aligned with
   * what the user actually sees (including after 汉字 selection).
   */
  useEffect(() => {
    const el = elRef.current;
    if (!el) return;

    let raf = 0;
    const sync = () => {
      if (!elRef.current) return;
      if (composing.current) {
        emitSlash();
        emitMention();
        return;
      }
      const live = serializeDom(el);
      if (live !== lastValue.current) {
        commitFromDom(el);
      } else {
        emitSlash();
        emitMention();
      }
    };
    const schedule = () => {
      if (raf) cancelAnimationFrame(raf);
      raf = requestAnimationFrame(sync);
    };

    const mo = new MutationObserver(schedule);
    mo.observe(el, {
      childList: true,
      subtree: true,
      characterData: true,
    });

    return () => {
      mo.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [commitFromDom, emitMention, emitSlash, value]);

  const valueEmpty =
    !value.trim() ||
    (parseStoredContent(value).every(
      (s) => s.type === "text" && !s.text.trim(),
    ) &&
      !value.includes("[[skill:"));
  // Both prop and live DOM must be empty — otherwise placeholder covers ink.
  const isEmpty = valueEmpty && domEmpty;

  // External value clear (send / clear) must restore placeholder.
  useEffect(() => {
    if (valueEmpty) {
      const el = elRef.current;
      if (el) syncDomEmpty(el);
      else setDomEmpty(true);
    } else {
      setDomEmpty(false);
    }
  }, [valueEmpty, value, syncDomEmpty]);

  return (
    <div className="composer-editor-wrap">
      {isEmpty && placeholder ? (
        <div className="composer-editor__placeholder" aria-hidden>
          {placeholder}
        </div>
      ) : null}
      <div
        ref={setRefs}
        className={className ?? "composer__input"}
        contentEditable={!disabled}
        role="textbox"
        aria-multiline
        aria-label={ariaLabel}
        aria-placeholder={placeholder}
        data-placeholder={placeholder}
        suppressContentEditableWarning
        onFocus={() => {
          focused.current = true;
        }}
        onBlur={() => {
          focused.current = false;
        }}
        onInput={onInput}
        onPaste={onPaste}
        onDragEnter={onDragEnter}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
        onKeyUp={() => {
          if (!composing.current) {
            emitSlash();
            emitMention();
          }
        }}
        onClick={() => {
          emitSlash();
          emitMention();
        }}
        onCompositionStart={() => {
          composing.current = true;
        }}
        onCompositionUpdate={() => {
          emitSlash();
          emitMention();
        }}
        onCompositionEnd={(e: CompositionEvent<HTMLDivElement>) => {
          flushAfterIme(e.currentTarget);
        }}
        onKeyDown={(e) => {
          const ne = e.nativeEvent;
          if (ne.isComposing || ne.keyCode === 229 || composing.current) {
            return;
          }
          if (
            (e.key === "Backspace" || e.key === "Delete") &&
            removeComposerMentionAtCaret(
              e.currentTarget,
              e.key === "Backspace" ? "backward" : "forward",
            )
          ) {
            e.preventDefault();
            commitFromDom(e.currentTarget);
            return;
          }
          onKeyDown?.(e);
        }}
      />
    </div>
  );
}
