import { useTranslation } from "@/modules/i18n";
import { notifyDocumentSaved } from "@/modules/lsp";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { currentWorkspaceEnv } from "@/modules/workspace";
import { invoke } from "@tauri-apps/api/core";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";
import {
  detectEol,
  type Eol,
  normalizeToLf,
  restoreEol,
} from "@/modules/editor/lib/eol";

type ReadResult =
  | {
      kind: "text";
      content: string;
      size: number;
      mtime: number;
      version: string;
    }
  | { kind: "binary"; size: number }
  | { kind: "toolarge"; size: number; limit: number };

type WriteResult = { mtime: number; version: string };

export const FORCE_READ_LIMIT = 50 * 1024 * 1024;

export type DocumentState =
  | { status: "loading" }
  | { status: "ready"; content: string; size: number }
  | { status: "binary"; size: number }
  | { status: "toolarge"; size: number; limit: number }
  | { status: "error"; message: string };

export type DocumentOperation = {
  path: string;
  generation: number;
  readId: number;
};

type Options = {
  path: string;
  documentId?: number | string;
  onDirtyChange?: (dirty: boolean) => void;
};

export function useDocument({
  path,
  documentId = path,
  onDirtyChange,
}: Options) {
  const tr = useTranslation();
  const [doc, setDoc] = useState<DocumentState>({ status: "loading" });
  const [dirty, setDirty] = useState(false);
  const docRef = useRef(doc);
  docRef.current = doc;
  const scopeRef = useRef({ path, documentId, generation: 0, mounted: false });
  const readIdRef = useRef(0);
  const savedRef = useRef("");
  const bufferRef = useRef("");
  const eolRef = useRef<Eol>("\n");
  const diskVersionRef = useRef<string | null>(null);
  const dirtyRef = useRef(false);
  const forceRef = useRef(false);
  const writeQueueRef = useRef<Promise<void>>(Promise.resolve());
  const queuedWriteRef = useRef<{
    operation: DocumentOperation;
    overwrite: boolean;
    promise: Promise<boolean>;
  } | null>(null);

  const autoSave = usePreferencesStore((s) => s.editorAutoSave);
  const autoSaveDelay = usePreferencesStore((s) => s.editorAutoSaveDelay);
  const autoSaveRef = useRef({ autoSave, autoSaveDelay });
  autoSaveRef.current = { autoSave, autoSaveDelay };
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const updateDirty = useCallback((value: boolean) => {
    dirtyRef.current = value;
    setDirty(value);
  }, []);
  const clearAutoSaveTimer = useCallback(() => {
    if (timeoutRef.current !== null) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
  }, []);
  const captureOperation = useCallback((): DocumentOperation => {
    const { path: currentPath, generation } = scopeRef.current;
    return { path: currentPath, generation, readId: readIdRef.current };
  }, []);
  const isCurrentOperation = useCallback((operation: DocumentOperation) => {
    const scope = scopeRef.current;
    return (
      scope.mounted &&
      scope.path === operation.path &&
      scope.generation === operation.generation
    );
  }, []);

  const writeToDisk = useCallback(
    async (operation: DocumentOperation, overwrite = false) => {
      if (!isCurrentOperation(operation)) return false;
      const content = bufferRef.current;
      const result = await invoke<WriteResult>("fs_write_file", {
        path: operation.path,
        content: restoreEol(content, eolRef.current),
        expectedVersion: diskVersionRef.current,
        overwrite,
        workspace: currentWorkspaceEnv(),
        source: "editor",
      });
      if (!isCurrentOperation(operation)) return false;
      readIdRef.current += 1;
      diskVersionRef.current = result.version;
      savedRef.current = content;
      updateDirty(bufferRef.current !== content);
      notifyDocumentSaved(operation.path);
      return true;
    },
    [isCurrentOperation, updateDirty],
  );

  const queueWrite = useCallback(
    (operation: DocumentOperation, overwrite = false): Promise<boolean> => {
      const queued = queuedWriteRef.current;
      if (
        queued?.operation.path === operation.path &&
        queued.operation.generation === operation.generation &&
        queued.overwrite === overwrite
      )
        return queued.promise;
      const writing: Promise<boolean> = writeQueueRef.current.then(() => {
        if (queuedWriteRef.current?.promise === writing)
          queuedWriteRef.current = null;
        if (!isCurrentOperation(operation)) return false;
        if (!overwrite && bufferRef.current === savedRef.current) return true;
        return writeToDisk(operation, overwrite);
      });
      queuedWriteRef.current = { operation, overwrite, promise: writing };
      writeQueueRef.current = writing.then(
        () => {},
        () => {},
      );
      return writing;
    },
    [isCurrentOperation, writeToDisk],
  );

  const saveNow = useCallback(async (): Promise<boolean> => {
    const operation = captureOperation();
    try {
      return await queueWrite(operation);
    } catch (error) {
      if (!isCurrentOperation(operation)) return false;
      if (!String(error).startsWith("FILE_CONFLICT:")) throw error;
      const name = operation.path.split(/[\\/]/).pop() ?? operation.path;
      toast.warning(tr("File changed on disk"), {
        id: `save-conflict:${operation.path}`,
        description: tr(
          "{value0} was modified by another program while you had unsaved changes. Overwrite to keep your version.",
          { value0: name },
        ),
        action: {
          label: tr("Overwrite"),
          onClick: () =>
            void queueWrite(operation, true).catch((e) =>
              toast.error(String(e)),
            ),
        },
      });
      return false;
    }
  }, [captureOperation, isCurrentOperation, tr, queueWrite]);
  const saveNowRef = useRef(saveNow);
  saveNowRef.current = saveNow;
  const scheduleAutoSave = useCallback(() => {
    clearAutoSaveTimer();
    const { autoSave: active, autoSaveDelay: delay } = autoSaveRef.current;
    if (active && dirtyRef.current) {
      timeoutRef.current = setTimeout(() => {
        timeoutRef.current = null;
        void saveNowRef.current().catch((e) => console.error("[autosave]", e));
      }, delay);
    }
  }, [clearAutoSaveTimer]);

  const onDirtyChangeRef = useRef(onDirtyChange);
  onDirtyChangeRef.current = onDirtyChange;
  useEffect(() => {
    onDirtyChangeRef.current?.(dirty);
  }, [dirty]);

  const adoptRead = useCallback(
    (res: ReadResult, skipIfUnchanged = false) => {
      if (res.kind === "text") {
        eolRef.current = detectEol(res.content);
        diskVersionRef.current = res.version;
        const content = normalizeToLf(res.content);
        if (skipIfUnchanged && content === savedRef.current) return;
        savedRef.current = content;
        bufferRef.current = content;
        updateDirty(false);
        setDoc({ status: "ready", content, size: res.size });
      } else if (res.kind === "binary") {
        setDoc({ status: "binary", size: res.size });
      } else {
        setDoc({ status: "toolarge", size: res.size, limit: res.limit });
      }
    },
    [updateDirty],
  );

  const readFromDisk = useCallback(
    (force: boolean, reload = false) => {
      const operation = captureOperation();
      const readId = ++readIdRef.current;
      return invoke<ReadResult>("fs_read_file", {
        path: operation.path,
        workspace: currentWorkspaceEnv(),
        force,
      })
        .then((res) => {
          if (
            !isCurrentOperation(operation) ||
            readId !== readIdRef.current ||
            dirtyRef.current
          )
            return;
          adoptRead(res, reload);
        })
        .catch((error) => {
          if (!isCurrentOperation(operation) || readId !== readIdRef.current)
            return;
          if (reload)
            console.warn("[editor] reload failed", operation.path, error);
          else setDoc({ status: "error", message: String(error) });
        });
    },
    [adoptRead, captureOperation, isCurrentOperation],
  );

  useLayoutEffect(() => {
    const scope = scopeRef.current;
    const sameDocument = scope.documentId === documentId;
    const relocated = sameDocument && docRef.current.status === "ready";
    scopeRef.current = {
      path,
      documentId,
      generation: scope.generation + 1,
      mounted: true,
    };
    readIdRef.current += 1;
    clearAutoSaveTimer();
    if (relocated) {
      if (dirtyRef.current) scheduleAutoSave();
      else void readFromDisk(forceRef.current, true);
    } else {
      if (!sameDocument) forceRef.current = false;
      diskVersionRef.current = null;
      savedRef.current = "";
      bufferRef.current = "";
      updateDirty(false);
      setDoc({ status: "loading" });
      void readFromDisk(forceRef.current);
    }
    return () => {
      scopeRef.current.mounted = false;
      readIdRef.current += 1;
      clearAutoSaveTimer();
    };
  }, [
    path,
    documentId,
    clearAutoSaveTimer,
    readFromDisk,
    scheduleAutoSave,
    updateDirty,
  ]);

  const openAnyway = useCallback(() => {
    if (dirtyRef.current) return;
    forceRef.current = true;
    setDoc({ status: "loading" });
    void readFromDisk(true);
  }, [readFromDisk]);
  const reload = useCallback((): boolean => {
    if (dirtyRef.current) return false;
    void readFromDisk(forceRef.current, true);
    return true;
  }, [readFromDisk]);
  const save = useCallback(async (): Promise<boolean> => {
    clearAutoSaveTimer();
    return saveNow();
  }, [clearAutoSaveTimer, saveNow]);

  const adoptDiskText = useCallback(
    (
      diskText: string,
      version: string,
      operation: DocumentOperation,
    ): string | null => {
      if (
        !isCurrentOperation(operation) ||
        operation.readId !== readIdRef.current
      )
        return null;
      readIdRef.current += 1;
      eolRef.current = detectEol(diskText);
      diskVersionRef.current = version;
      const content = normalizeToLf(diskText);
      savedRef.current = content;
      updateDirty(bufferRef.current !== content);
      return content;
    },
    [isCurrentOperation, updateDirty],
  );
  const onChange = useCallback(
    (next: string) => {
      bufferRef.current = next;
      updateDirty(next !== savedRef.current);
      scheduleAutoSave();
    },
    [scheduleAutoSave, updateDirty],
  );

  return {
    doc,
    dirty,
    onChange,
    save,
    reload,
    adoptDiskText,
    openAnyway,
    captureOperation,
    isCurrentOperation,
  };
}
