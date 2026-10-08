import { useTranslation } from "@/modules/i18n";
import { Popover, PopoverAnchor } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { useEffect, useMemo, useRef, useState } from "react";
import { useWorkspaceFiles } from "@/modules/ai/hooks/useWorkspaceFiles";
import { useComposerCommands } from "@/modules/ai/hooks/useComposerCommands";
import { useComposer } from "@/modules/ai/lib/composer";
import { SLASH_COMMANDS, type SlashCommandMeta } from "@/modules/ai/lib/slashCommands";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { FilePickerContent } from "@/modules/ai/components/FilePicker";
import { CommandPickerContent } from "@/modules/ai/components/CommandPicker";

type CommandTrigger = {
  start: number;
  end: number;
  query: string;
};

type FileTrigger = {
  start: number;
  end: number;
  query: string;
};

function detectCommandTrigger(
  value: string,
  caret: number,
): CommandTrigger | null {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = value[i];
    if (ch === "/") {
      const prev = i === 0 ? " " : value[i - 1];
      if (!/\s/.test(prev)) return null;
      const slice = value.slice(i + 1, caret);
      if (!/^[a-z0-9_.:-]*$/i.test(slice)) return null;
      return { start: i, end: caret, query: slice.toLowerCase() };
    }
    if (/\s/.test(ch)) return null;
    if (!/[a-z0-9_.:-]/i.test(ch)) return null;
  }
  return null;
}

function detectFileTrigger(value: string, caret: number): FileTrigger | null {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = value[i];
    if (ch === "@") {
      const prev = i === 0 ? " " : value[i - 1];
      if (!/\s/.test(prev)) return null;
      const slice = value.slice(i + 1, caret);
      return { start: i, end: caret, query: slice };
    }
    if (/\s/.test(ch)) return null;
  }
  return null;
}

export function AiComposerInput({ spacious = false }: { spacious?: boolean }) {
  const tr = useTranslation();
  const c = useComposer();
  const workspaceRoot = useChatStore((s) => s.live.getWorkspaceRoot());

  const [trigger, setTrigger] = useState<CommandTrigger | null>(null);
  const dismissedCommand = useRef<{ value: string; caret: number } | null>(null);
  const catalog = useComposerCommands(trigger !== null);
  const [fileTrigger, setFileTrigger] = useState<FileTrigger | null>(null);
  const [activeSelection, setActiveSelection] = useState({ key: "", index: 0 });
  const workspaceFiles = useWorkspaceFiles(workspaceRoot, fileTrigger !== null);

  const [fileQuery, setFileQuery] = useState("");
  useEffect(() => {
    if (!fileTrigger) {
      setFileQuery("");
      return;
    }
    const q = fileTrigger.query;
    const t = window.setTimeout(() => setFileQuery(q), 50);
    return () => window.clearTimeout(t);
  }, [fileTrigger]);

  useEffect(() => {
    autoresize(c.textareaRef.current);
  }, [c.value, c.textareaRef]);

  useEffect(() => {
    const input = c.textareaRef.current;
    if (!input) return;
    let width = input.clientWidth;
    let frame: number | null = null;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry || entry.contentRect.width === width) return;
      width = entry.contentRect.width;
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        autoresize(input);
      });
    });
    observer.observe(input);
    return () => {
      observer.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [c.textareaRef]);

  const updateTrigger = () => {
    const el = c.textareaRef.current;
    if (!el) {
      setTrigger(null);
      setFileTrigger(null);
      return;
    }
    const caret = el.selectionStart ?? 0;
    const dismissed = dismissedCommand.current;
    if (!dismissed || dismissed.value !== c.value || dismissed.caret !== caret) {
      dismissedCommand.current = null;
    }
    setTrigger(
      dismissedCommand.current ? null : detectCommandTrigger(c.value, caret),
    );
    setFileTrigger(detectFileTrigger(c.value, caret));
  };

  useEffect(updateTrigger, [c.value, c.textareaRef]);

  const filteredItems = useMemo<SlashCommandMeta[]>(() => {
    if (!trigger) return [];
    const q = trigger.query;
    return [...Object.values(SLASH_COMMANDS), ...catalog.commands].filter((command) =>
      `${command.name} ${command.source ? command.label : tr(command.label)}`.toLowerCase().includes(q));
  }, [trigger, catalog.commands, tr]);

  const FILE_PICKER_CAP = 30;
  const filteredFiles = useMemo<string[]>(() => {
    if (!fileTrigger) return [];
    const q = fileQuery.toLowerCase();
    if (!q) return workspaceFiles.files.slice(0, FILE_PICKER_CAP);
    const out: string[] = [];
    for (const f of workspaceFiles.files) {
      if (f.toLowerCase().includes(q)) {
        out.push(f);
        if (out.length >= FILE_PICKER_CAP) break;
      }
    }
    return out;
  }, [fileTrigger, fileQuery, workspaceFiles.files]);

  const selectionKey = fileTrigger ? `file:${fileQuery}` : trigger ? `command:${trigger.query}` : "";
  const activeIndex = activeSelection.key === selectionKey ? activeSelection.index : 0;
  const setActiveIndex = (update: number | ((index: number) => number)) => setActiveSelection((current) => ({
    key: selectionKey,
    index: typeof update === "function" ? update(current.key === selectionKey ? current.index : 0) : update,
  }));

  const pickerOpen = trigger !== null || fileTrigger !== null;

  const onPickItem = (item: SlashCommandMeta) => {
    if (!trigger) return;
    const before = c.value.slice(0, trigger.start);
    const afterRaw = c.value.slice(trigger.end);
    c.addCommand(item);
    const after = afterRaw.replace(/^\s+/, "");
    c.setValue(`${before}${after}`);
    setTrigger(null);
    setActiveIndex(0);
    requestAnimationFrame(() => {
      const el = c.textareaRef.current;
      if (!el) return;
      const caret = before.length;
      el.focus();
      el.setSelectionRange(caret, caret);
    });
  };

  const onPickFile = async (filePath: string) => {
    if (!fileTrigger || !workspaceRoot) return;
    const before = c.value.slice(0, fileTrigger.start);
    const after = c.value.slice(fileTrigger.end);
    c.setValue(`${before}${after}`);
    setFileTrigger(null);
    setActiveIndex(0);
    const fullPath = workspaceRoot.endsWith("/")
      ? `${workspaceRoot}${filePath}`
      : `${workspaceRoot}/${filePath}`;
    await c.attachFileByPath(fullPath);
    requestAnimationFrame(() => {
      const el = c.textareaRef.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(before.length, before.length);
    });
  };

  const pickActive = () => {
    if (fileTrigger) {
      const file = filteredFiles[activeIndex];
      if (file) void onPickFile(file);
      return;
    }
    const it = filteredItems[activeIndex];
    if (it) onPickItem(it);
  };

  return (
    <Popover open={pickerOpen}>
      <PopoverAnchor asChild>
        <div className="flex items-start gap-2">
          <textarea
            ref={c.textareaRef}
            value={c.value}
            onChange={(e) => c.setValue(e.target.value)}
            onKeyUp={(event) => { if (event.key !== "Escape") updateTrigger(); }}
            onClick={() => {
              dismissedCommand.current = null;
              updateTrigger();
            }}
            onSelect={updateTrigger}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing) return;
              if (pickerOpen) {
                const items = fileTrigger ? filteredFiles : filteredItems;
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setActiveIndex((i) =>
                    Math.min(i + 1, Math.max(0, items.length - 1)),
                  );
                  return;
                }
                if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setActiveIndex((i) => Math.max(0, i - 1));
                  return;
                }
                if (e.key === "Tab" || e.key === "Enter") {
                  if (items.length > 0) {
                    e.preventDefault();
                    pickActive();
                    return;
                  }
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  if (fileTrigger) {
                    const before = c.value.slice(0, fileTrigger.start);
                    const after = c.value.slice(fileTrigger.end);
                    c.setValue(`${before}${after}`);
                    setFileTrigger(null);
                  } else {
                    dismissedCommand.current = {
                      value: c.value,
                      caret: c.textareaRef.current?.selectionStart ?? 0,
                    };
                    setTrigger(null);
                  }
                  return;
                }
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                c.submit();
              }
            }}
            placeholder={tr("Message input")}
            rows={1}
            aria-label={tr("Message input")}
            className={cn(
              "max-h-40 min-w-0 flex-1 resize-none bg-transparent text-ui-base leading-relaxed outline-none",
              "placeholder:text-muted-foreground/60",
              spacious && "min-h-14",
            )}
          />
        </div>
      </PopoverAnchor>
      {fileTrigger ? (
        <FilePickerContent
          files={filteredFiles}
          activeIndex={activeIndex}
          indexing={workspaceFiles.indexing}
          truncated={workspaceFiles.truncated}
          hasWorkspace={workspaceRoot !== null}
          onPick={(f) => void onPickFile(f)}
          onHover={setActiveIndex}
        />
      ) : (
        <CommandPickerContent
          items={filteredItems}
          activeIndex={activeIndex}
          onPick={onPickItem}
          onHover={setActiveIndex}
          loading={catalog.loading}
          error={catalog.error}
          onRetry={catalog.retry}
        />
      )}
    </Popover>
  );
}

function autoresize(el: HTMLTextAreaElement | null) {
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
}
