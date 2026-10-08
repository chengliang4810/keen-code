import { isReasoningSelectionReady } from "@/modules/ai/lib/reasoning";
import {
  effectiveCustomModel,
  resolveEndpointModel,
} from "@/modules/ai/config";
import { useModelCatalogStore } from "@/modules/ai/lib/modelCatalogState";
import { ensureDefaultChatDirectory } from "@/modules/ai/lib/defaultChatDirectory";
import { contextBlock } from "@/modules/ai/lib/composerContext";
import { commandApi } from "@/modules/ai/lib/commands";
import { commandPrompt } from "@/modules/ai/lib/commandPrompt";
import { invoke } from "@tauri-apps/api/core";
import {
  createContext,
  useContext,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { tryRunSlashCommand, type SlashCommandMeta } from "./slashCommands";
import { getChat, getTaskWorkspace, useChatStore, hasKeyForModel } from "@/modules/ai/store/chatStore";
import { toast } from "sonner";
import { useTranslation } from "@/modules/i18n";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { isConfiguredCustomModel } from "@/modules/ai/lib/modelSelection";
import {
  currentWorkspaceEnv,
  currentWorkspaceScopeKey,
} from "@/modules/workspace";

export type FileAttachment = {
  id: string;
  name: string;
  kind: "image" | "text" | "selection" | "context";
  mediaType: string;
  url?: string;
  text?: string;
  size: number;
  /** For kind === "selection": which surface it came from. */
  source?: "terminal" | "editor";
};

type MessagePart =
  | { type: "text"; text: string }
  | { type: "file"; mediaType: string; url: string; filename?: string };

export const MAX_TEXT_INLINE = 200_000;
export const ACCEPTED_FILES =
  "image/*,.txt,.md,.json,.yaml,.yml,.toml,.sh,.zsh,.bash,.py,.js,.jsx,.ts,.tsx,.rs,.go,.java,.c,.cpp,.h,.hpp,.html,.css,.csv,.log,.env,.config,.conf,.ini,Dockerfile,.dockerfile";

type ComposerCtx = {
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  value: string;
  setValue: React.Dispatch<React.SetStateAction<string>>;
  files: FileAttachment[];
  addFiles: (list: FileList | null) => Promise<void>;
  /** Attach a file by absolute path — used by the file explorer's "Attach to Agent". */
  attachFileByPath: (path: string) => Promise<void>;
  addContext: (id: string, name: string, text: string) => void;
  addContextFrom: (
    id: string,
    name: string,
    load: () => Promise<string>,
  ) => Promise<void>;
  removeFile: (id: string) => void;
  pickedCommands: SlashCommandMeta[];
  addCommand: (c: SlashCommandMeta) => void;
  removeCommand: (name: string) => void;
  isBusy: boolean;
  isAttaching: boolean;
  submit: () => void;
  stop: () => void;
  canSend: boolean;
};

const Ctx = createContext<ComposerCtx | null>(null);

export function useComposer(): ComposerCtx {
  const ctx = useContext(Ctx);
  if (!ctx)
    throw new Error("useComposer must be used inside <AiComposerProvider>");
  return ctx;
}

type ProviderProps = {
  children: React.ReactNode;
};

type ComposerDraft = {
  value: string;
  files: FileAttachment[];
  pickedCommands: SlashCommandMeta[];
};

const EMPTY_DRAFT: ComposerDraft = {
  value: "",
  files: [],
  pickedCommands: [],
};

export function AiComposerProvider({ children }: ProviderProps) {
  const tr = useTranslation();
  const sessionId = useChatStore((s) => s.activeSessionId);
  const selectedModelId = useChatStore((s) => s.selectedModelId);
  const customEndpoints = usePreferencesStore((s) => s.customEndpoints);
  useModelCatalogStore((s) => s.revision);
  const reasoningSelection = useChatStore(
    (s) =>
      (s.draftSession?.id === s.activeSessionId
        ? s.draftSession
        : s.sessions.find((entry) => entry.id === s.activeSessionId)
      )?.reasoningSelection,
  );
  const modelConfig = resolveEndpointModel(selectedModelId, customEndpoints);
  const reasoningReady = isReasoningSelectionReady(
    selectedModelId,
    modelConfig
      ? effectiveCustomModel(modelConfig.model, modelConfig.endpoint.baseURL)
          .reasoningLevels
      : [],
    reasoningSelection,
  );
  const draftSession = useChatStore((s) => s.draftSession);
  const status = useChatStore((s) => s.agentMeta.status);
  const sessionLoading = useChatStore((s) => s.sessionLoading);
  const sessionSubmitting = useChatStore((s) => s.sessionSubmitting);
  const isBusy =
    sessionLoading ||
    sessionSubmitting ||
    status === "thinking" ||
    status === "streaming" ||
    status === "awaiting-approval";

  // 按任务保存内存草稿，保持 Provider 和终端子树挂载，切换任务不重启 PTY。
  const [drafts, setDrafts] = useState<Record<string, ComposerDraft>>({});
  const draftKey = sessionId ?? "loading";
  const [pendingAttachments, setPendingAttachments] = useState<
    Record<string, number>
  >({});
  const isAttaching = (pendingAttachments[draftKey] ?? 0) > 0;
  const attachmentPending = useCallback(
    (delta: number) => {
      setPendingAttachments((all) => {
        const next = { ...all };
        const count = (next[draftKey] ?? 0) + delta;
        if (count > 0) next[draftKey] = count;
        else delete next[draftKey];
        return next;
      });
    },
    [draftKey],
  );
  const draft = drafts[draftKey] ?? EMPTY_DRAFT;
  const { value, files, pickedCommands } = draft;
  const setValue = useCallback(
    (update: React.SetStateAction<string>) => {
      setDrafts((all) => {
        const current = all[draftKey] ?? EMPTY_DRAFT;
        return {
          ...all,
          [draftKey]: {
            ...current,
            value:
              typeof update === "function" ? update(current.value) : update,
          },
        };
      });
    },
    [draftKey],
  );
  const setFiles = useCallback(
    (update: React.SetStateAction<FileAttachment[]>) => {
      setDrafts((all) => {
        const current = all[draftKey] ?? EMPTY_DRAFT;
        return {
          ...all,
          [draftKey]: {
            ...current,
            files:
              typeof update === "function" ? update(current.files) : update,
          },
        };
      });
    },
    [draftKey],
  );
  const setPickedCommands = useCallback(
    (update: React.SetStateAction<SlashCommandMeta[]>) => {
      setDrafts((all) => {
        const current = all[draftKey] ?? EMPTY_DRAFT;
        return {
          ...all,
          [draftKey]: {
            ...current,
            pickedCommands:
              typeof update === "function"
                ? update(current.pickedCommands)
                : update,
          },
        };
      });
    },
    [draftKey],
  );
  const sessions = useChatStore((s) => s.sessions);
  useEffect(() => {
    setDrafts((all) => {
      const entries = Object.entries(all).filter(
        ([id]) =>
          id === "loading" ||
          id === draftSession?.id ||
          sessions.some((s) => s.id === id),
      );
      return entries.length === Object.keys(all).length
        ? all
        : Object.fromEntries(entries);
    });
  }, [sessions, draftSession?.id]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const focusSignal = useChatStore((s) => s.focusSignal);
  const pendingPrefill = useChatStore((s) => s.pendingPrefill);
  const consumePrefill = useChatStore((s) => s.consumePrefill);
  const pendingSelections = useChatStore((s) => s.pendingSelections);
  const consumeSelections = useChatStore((s) => s.consumeSelections);

  useEffect(() => {
    if (focusSignal === 0) return;
    textareaRef.current?.focus();
    if (pendingPrefill != null) {
      const text = consumePrefill();
      if (text) setValue((v) => (v ? `${text}${v}` : text));
    }
  }, [focusSignal, pendingPrefill, consumePrefill, setValue]);

  // Re-focus the textarea whenever the agent finishes a response
  const prevIsBusyRef = useRef(false);
  useEffect(() => {
    if (prevIsBusyRef.current && !isBusy) {
      requestAnimationFrame(() => textareaRef.current?.focus());
    }
    prevIsBusyRef.current = isBusy;
  }, [isBusy, textareaRef]);

  useEffect(() => {
    if (pendingSelections.length === 0) return;
    const drained = consumeSelections();
    if (drained.length === 0) return;
    setFiles((prev) => {
      const existing = new Set(prev.map((f) => f.id));
      const next: FileAttachment[] = [];
      for (const sel of drained) {
        if (existing.has(sel.id)) continue;
        next.push({
          id: sel.id,
          name:
            sel.source === "editor" ? "Editor selection" : "Terminal selection",
          kind: "selection",
          mediaType: "text/plain",
          text: sel.text,
          size: sel.text.length,
          source: sel.source,
        });
      }
      return next.length ? [...prev, ...next] : prev;
    });
  }, [pendingSelections, consumeSelections, setFiles]);

  const addFiles = async (list: FileList | null) => {
    if (!list) return;
    attachmentPending(1);
    try {
      const next: FileAttachment[] = [];
      for (const f of Array.from(list)) {
        const att = await readAttachment(f);
        if (att) next.push(att);
        else
          toast.error(
            tr(
              "This file cannot be attached. Choose a text file under 200 KB.",
            ),
          );
      }
      if (next.length)
        setFiles((prev) => [
          ...prev,
          ...next.filter((file) => !prev.some((entry) => entry.id === file.id)),
        ]);
    } catch (error) {
      toast.error(tr("Could not attach file."), { description: String(error) });
    } finally {
      attachmentPending(-1);
    }
  };

  const removeFile = (id: string) =>
    setFiles((prev) => prev.filter((f) => f.id !== id));

  const addContext = (id: string, name: string, text: string) => {
    setFiles((prev) =>
      prev.some((file) => file.id === id)
        ? prev
        : [
            ...prev,
            {
              id,
              name,
              kind: "context",
              mediaType: "text/plain",
              text,
              size: text.length,
            },
          ],
    );
  };

  const addContextFrom = async (
    id: string,
    name: string,
    load: () => Promise<string>,
  ) => {
    attachmentPending(1);
    try {
      const text = await load();
      if (!text) toast.error(tr("This conversation has no text to reference."));
      else addContext(id, name, text);
    } catch (error) {
      toast.error(tr("Could not load conversation context."), {
        description: String(error),
      });
    } finally {
      attachmentPending(-1);
    }
  };

  const addCommand = (cmd: SlashCommandMeta) =>
    setPickedCommands((prev) =>
      prev.some((p) => p.name === cmd.name) ? prev : [...prev, cmd],
    );
  const removeCommand = (name: string) =>
    setPickedCommands((prev) => prev.filter((c) => c.name !== name));

  const attachFileByPath = useCallback(
    async (path: string) => {
      if (useChatStore.getState().sessionLoading) return;
      attachmentPending(1);
      try {
        type ReadResult =
          | { kind: "text"; content: string; size: number }
          | { kind: "binary"; size: number }
          | { kind: "toolarge"; size: number; limit: number };
        const result = await invoke<ReadResult>("fs_read_file", {
          path,
          workspace: currentWorkspaceEnv(),
        });
        if (result.kind !== "text") {
          toast.error(
            tr(
              "This file cannot be attached. Choose a text file under 200 KB.",
            ),
          );
          return;
        }
        if (
          result.size > MAX_TEXT_INLINE ||
          result.content.length > MAX_TEXT_INLINE
        ) {
          toast.error(
            tr(
              "This file cannot be attached. Choose a text file under 200 KB.",
            ),
          );
          return;
        }
        const name = path.split(/[\\/]/).pop() || path;
        const id = `path-${path}`;
        setFiles((prev) => {
          if (prev.some((f) => f.id === id)) return prev;
          const att: FileAttachment = {
            id,
            name,
            kind: "text",
            mediaType: "text/plain",
            text: result.content,
            size: result.size,
          };
          return [...prev, att];
        });
        // Open the AI panel & focus the input so the user sees the chip.
        useChatStore.getState().focusInput();
      } catch (e) {
        toast.error(tr("Could not attach file."), { description: String(e) });
      } finally {
        attachmentPending(-1);
      }
    },
    [setFiles, tr, attachmentPending],
  );

  // 监听器跟随当前任务；读取中的附件仍写回发起读取的任务草稿。
  useEffect(() => {
    const onAttach = (e: Event) => {
      const path = (e as CustomEvent<string>).detail;
      if (typeof path === "string" && path.length > 0)
        void attachFileByPath(path);
    };
    window.addEventListener("rcode:ai-attach-file", onAttach);
    return () => window.removeEventListener("rcode:ai-attach-file", onAttach);
  }, [attachFileByPath]);

  const submit = () => {
    if (
      isBusy ||
      isAttaching ||
      useChatStore.getState().sessionLoading ||
      useChatStore.getState().sessionSubmitting
    )
      return;
    const trimmed = value.trim();
    if (
      !trimmed &&
      files.length === 0 &&
      pickedCommands.length === 0
    )
      return;

    // Slash-command interception. `/plan` toggles plan mode; `/init` rewrites
    // the prompt to the AGENTS.md scan template before sending.
    let effectiveText = trimmed;
    let commandMarker: string | null = null;
    let commandSource = trimmed;
    if (
      pickedCommands.length > 0 &&
      !trimmed.startsWith("/") &&
      !trimmed.startsWith("#")
    ) {
      const builtin = pickedCommands.find((command) => !command.source);
      if (builtin) commandSource = `${builtin.invocation} ${trimmed}`.trim();
    }
    if (commandSource.startsWith("/") || commandSource.startsWith("#")) {
      const outcome = tryRunSlashCommand(commandSource);
      if (outcome.kind === "handled") {
        setValue("");
        setPickedCommands([]);
        if (outcome.toast) console.info(outcome.toast);
        return;
      }
      if (outcome.kind === "send-prompt") {
        effectiveText = outcome.prompt;
        if (outcome.commandName) {
          commandMarker = `<rcode-command name="${outcome.commandName}" />`;
        }
      }
    }

    const parts: MessagePart[] = [];
    const fileBlocks = files
      .filter((f) => f.kind === "text")
      .map(
        (f) =>
          `<file name="${f.name}" mediaType="${f.mediaType}">\n${f.text ?? ""}\n</file>`,
      );
    const selectionBlocks = files
      .filter((f) => f.kind === "selection")
      .map(
        (f) =>
          `<selection source="${f.source ?? "terminal"}">\n${f.text ?? ""}\n</selection>`,
      );
    const composeText = (body: string, marker: string | null) => [
      marker ?? "",
      ...files
        .filter((file) => file.kind === "context")
        .map((file) => contextBlock(file.name, file.text ?? "")),
      selectionBlocks.join("\n\n"),
      fileBlocks.join("\n\n"),
      body,
    ]
      .filter(Boolean)
      .join("\n\n");

    for (const f of files) {
      if (f.kind === "image" && f.url) {
        parts.push({
          type: "file",
          mediaType: f.mediaType,
          url: f.url,
          filename: f.name,
        });
      }
    }

    if (!sessionId) return;
    const store = useChatStore.getState();
    if (!hasKeyForModel(store.selectedModelId)) return;
    const selectedConfig = resolveEndpointModel(
      store.selectedModelId,
      usePreferencesStore.getState().customEndpoints,
    );
    const selectedSession =
      store.draftSession?.id === sessionId
        ? store.draftSession
        : store.sessions.find((entry) => entry.id === sessionId);
    if (
      selectedConfig &&
      !isReasoningSelectionReady(
        store.selectedModelId,
        effectiveCustomModel(
          selectedConfig.model,
          selectedConfig.endpoint.baseURL,
        ).reasoningLevels,
        selectedSession?.reasoningSelection,
      )
    )
      return;
    if (
      store.draftSession?.id === sessionId &&
      !store.draftSession.projectId &&
      !store.draftSession.projectless
    )
      return;
    const workspaceRoot = store.live.getWorkspaceRoot();
    const workspaceEnv = currentWorkspaceEnv();
    if (workspaceRoot)
      store.bindSessionWorkspace(
        sessionId,
        workspaceRoot,
        currentWorkspaceScopeKey(),
      );
    store.patchAgentMeta({ hitStepCap: false, compactionNotice: null });
    if (!store.panelOpen && !store.mini.open) store.openMini();
    useChatStore.setState({ sessionSubmitting: true });
    void (async () => {
      try {
        let commandRoot = getTaskWorkspace(sessionId);
        if (selectedSession?.projectless) {
          const root = await ensureDefaultChatDirectory();
          store.bindSessionWorkspace(sessionId, root, "local");
          commandRoot = root;
        }
        if (workspaceEnv.kind === "local") {
          const result = await commandPrompt(effectiveText, pickedCommands, (name, argumentsText) => commandApi.resolve(commandRoot, name, argumentsText));
          effectiveText = result.text;
          if (result.name) commandMarker = `<rcode-command name="${result.name}" />`;
        } else if (pickedCommands.some((command) => command.source)) {
          throw new Error("Could not load extensions.");
        }
        const composed = composeText(effectiveText, commandMarker);
        if (composed) parts.unshift({ type: "text", text: composed });
        const { getOrCreateChat } = await import("@/modules/ai/store/chatRuntime");
        if (!hasKeyForModel(useChatStore.getState().selectedModelId)) return;
        const chat = getOrCreateChat(sessionId);
        void chat.sendMessage({ role: "user", parts } as Parameters<
          typeof chat.sendMessage
        >[0]);
        setDrafts((all) => {
          const current = all[draftKey] ?? EMPTY_DRAFT;
          return { ...all, [draftKey]: {
            value: current.value === value ? "" : current.value,
            files: current.files.filter((file) => !files.includes(file)),
            pickedCommands: current.pickedCommands.filter((command) => !pickedCommands.includes(command)),
          } };
        });
      } catch (error) {
        // 加载失败保留草稿，不生成空会话，也不让导航一直处于锁定状态。
        toast.error(tr("Could not send message."), {
          description: String(error),
        });
      } finally {
        useChatStore.setState({ sessionSubmitting: false });
      }
    })();
    // Re-focus immediately after submit so the user can type a follow-up
    requestAnimationFrame(() => textareaRef.current?.focus());
  };

  const stop = () => {
    if (!sessionId) return;
    void getChat(sessionId)?.stop();
  };

  const canSend =
    !isBusy &&
    !isAttaching &&
    reasoningReady &&
    isConfiguredCustomModel(selectedModelId, customEndpoints) &&
    (draftSession?.id !== sessionId ||
      !!draftSession.projectId ||
      !!draftSession.projectless) &&
    (value.trim().length > 0 ||
      files.length > 0 ||
      pickedCommands.length > 0);

  const ctx: ComposerCtx = {
    textareaRef,
    value,
    setValue,
    files,
    addFiles,
    attachFileByPath,
    addContext,
    addContextFrom,
    removeFile,
    pickedCommands,
    addCommand,
    removeCommand,
    isBusy,
    isAttaching,
    submit,
    stop,
    canSend,
  };

  return <Ctx.Provider value={ctx}>{children}</Ctx.Provider>;
}

async function readAttachment(file: File): Promise<FileAttachment | null> {
  const id = `${file.name}-${file.size}-${file.lastModified}`;
  if (file.type.startsWith("image/")) {
    const url = await readAsDataURL(file);
    return {
      id,
      name: file.name,
      kind: "image",
      mediaType: file.type || "image/png",
      url,
      size: file.size,
    };
  }
  if (file.size > MAX_TEXT_INLINE) return null;
  const text = await file.text();
  return {
    id,
    name: file.name,
    kind: "text",
    mediaType: file.type || "text/plain",
    text,
    size: file.size,
  };
}

function readAsDataURL(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}
