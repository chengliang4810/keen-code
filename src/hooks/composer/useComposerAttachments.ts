import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { createT, type Locale } from "@/i18n";
import { claimClipboardFiles } from "@/lib/clipboardPaste";
import {
  mergeAttachments,
  pathBasename,
  type Attachment,
} from "@/lib/attachments";
import {
  draftAttachmentUpdateTarget,
  mergeDraftNavigationAttachments,
} from "@/lib/draftNavigation";
import { localizeUiError } from "@/lib/session";
import { toClientDragPoint } from "@/lib/dragZone";
import type {
  ComposerApiPort,
  ComposerDropPort,
  ComposerFeedbackPort,
  ComposerNavigationPort,
  Ref,
  StateSetter,
} from "../useComposerController";

export interface UseComposerAttachmentsOptions {
  locale: Locale;
  api: ComposerApiPort;
  navigation: ComposerNavigationPort;
  feedback: ComposerFeedbackPort;
  closeComposerMenu: () => void;
  drop?: ComposerDropPort;
}

export interface ComposerAttachmentsController {
  attachments: Attachment[];
  attachmentsRef: Ref<Attachment[]>;
  setAttachments: StateSetter<Attachment[]>;
  attachmentLabels: {
    open: string;
    reveal: string;
    copyPath: string;
    copyImage: string;
    addToComposer: string;
    remove: string;
    viewImage: string;
  };
  addAttachmentsFromPaths: (paths: string[]) => Promise<void>;
  addPastedFiles: (files: File[]) => Promise<void>;
  pickComposerFiles: () => Promise<void>;
}

/** Owns attachment state plus all composer file-input side effects. */
export function useComposerAttachments({
  locale,
  api,
  navigation,
  feedback,
  closeComposerMenu,
  drop,
}: UseComposerAttachmentsOptions): ComposerAttachmentsController {
  const tr = useMemo(() => createT(locale), [locale]);
  const portsRef = useRef({ api, navigation, feedback });
  portsRef.current = { api, navigation, feedback };
  const dropRef = useRef(drop);
  dropRef.current = drop;

  const [attachments, setAttachmentsState] = useState<Attachment[]>([]);
  const attachmentsRef = useRef<Attachment[]>([]);
  const setAttachments = useCallback(
    (update: SetStateAction<Attachment[]>) => {
      const next =
        typeof update === "function"
          ? update(attachmentsRef.current)
          : update;
      attachmentsRef.current = next;
      setAttachmentsState(next);
    },
    [],
  );
  const claimedClipboardFilesRef = useRef(new Set<string>());

  const addAttachmentsFromPaths = useCallback(
    async (paths: string[]) => {
      const currentPorts = portsRef.current;
      const request = currentPorts.navigation.location();
      if (!paths.length) {
        currentPorts.feedback.setLocalError(tr("attach.droppedNone"));
        return;
      }
      try {
        const next = currentPorts.api.isTauri()
          ? (await currentPorts.api.attachments.classifyPaths(paths)).map(
              (entry) => ({
                path: entry.path,
                name: entry.name,
                isDir: entry.isDir,
              }),
            )
          : paths.map((path) => ({
              path,
              name: pathBasename(path),
              isDir: false,
            }));
        if (!next.length) {
          currentPorts.feedback.setLocalError(tr("attach.droppedNone"));
          return;
        }
        const target = draftAttachmentUpdateTarget(
          request,
          currentPorts.navigation.location(),
          currentPorts.navigation.snapshotRef.current,
        );
        if (target === "current") {
          setAttachments((previous) => mergeAttachments(previous, next));
        } else if (target === "snapshot") {
          const snapshot = currentPorts.navigation.snapshotRef.current;
          if (snapshot) {
            currentPorts.navigation.snapshotRef.current =
              mergeDraftNavigationAttachments(snapshot, next);
          }
        }
      } catch (cause) {
        currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
      }
    },
    [locale, setAttachments, tr],
  );

  const pickComposerFiles = useCallback(async () => {
    closeComposerMenu();
    const currentPorts = portsRef.current;
    if (!currentPorts.api.isTauri()) {
      currentPorts.feedback.setLocalError(
        tr("composer.attachPasteFailed"),
      );
      return;
    }
    try {
      const paths = await currentPorts.api.attachments.pickFiles();
      if (!paths.length) return;
      await addAttachmentsFromPaths(paths);
      currentPorts.feedback.setLocalError(null);
      const label =
        paths.length === 1
          ? pathBasename(paths[0]!)
          : tr("composer.attachCount", { n: String(paths.length) });
      currentPorts.feedback.showToast(
        tr("composer.attachSaved", { name: label }),
        2200,
      );
    } catch (cause) {
      currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
    }
  }, [addAttachmentsFromPaths, closeComposerMenu, locale, tr]);

  const addPastedFiles = useCallback(
    async (files: File[]) => {
      const currentPorts = portsRef.current;
      if (!files.length || !currentPorts.api.isTauri()) return;
      const claimed = claimClipboardFiles(
        files,
        claimedClipboardFilesRef.current,
      );
      if (!claimed.length) return;
      try {
        const paths: string[] = [];
        for (const file of claimed) {
          paths.push(
            await currentPorts.api.attachments.savePastedFile(
              file.name || "pasted-file",
              Array.from(new Uint8Array(await file.arrayBuffer())),
            ),
          );
        }
        await addAttachmentsFromPaths(paths);
        currentPorts.feedback.setLocalError(null);
      } catch (cause) {
        currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
      } finally {
        window.setTimeout(
          () => claimedClipboardFilesRef.current.clear(),
          500,
        );
      }
    },
    [addAttachmentsFromPaths, locale],
  );

  const attachmentLabels = useMemo(
    () => ({
      open: tr("attach.open"),
      reveal: tr("attach.reveal"),
      copyPath: tr("attach.copyPath"),
      copyImage: tr("attach.copyImage"),
      addToComposer: tr("attach.addToComposer"),
      remove: tr("composer.attachRemove"),
      viewImage: tr("image.view"),
    }),
    [tr],
  );

  useEffect(() => {
    if (!drop) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const dragPathsRef: Ref<string[]> = { current: [] };
    void (async () => {
      if (!portsRef.current.api.isTauri()) return;
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const webview = getCurrentWebview();
        const windowHandle = getCurrentWindow();
        const factor = await windowHandle.scaleFactor();
        const stopListening = await webview.onDragDropEvent((event) => {
          if (cancelled) return;
          const currentDrop = dropRef.current;
          if (!currentDrop) return;
          const payload = event.payload;
          if (
            (payload.type === "enter" || payload.type === "drop") &&
            payload.paths?.length
          ) {
            dragPathsRef.current = payload.paths;
          }
          if (payload.type === "leave") {
            currentDrop.setDragZone(null);
            dragPathsRef.current = [];
            return;
          }
          if (payload.type === "enter" || payload.type === "over") {
            const point = toClientDragPoint(
              payload.position,
              factor,
              currentDrop.platform,
            );
            currentDrop.setDragZone(currentDrop.hitZone(point.x, point.y));
            return;
          }
          if (payload.type !== "drop") return;
          const point = toClientDragPoint(
            payload.position,
            factor,
            currentDrop.platform,
          );
          const zone = currentDrop.hitZone(point.x, point.y);
          const paths = payload.paths?.length
            ? payload.paths
            : dragPathsRef.current;
          currentDrop.setDragZone(null);
          dragPathsRef.current = [];
          if (!paths.length) {
            portsRef.current.feedback.setLocalError(
              tr("attach.droppedNone"),
            );
            return;
          }
          if (zone === "project") {
            void currentDrop.onProjectPaths(paths);
          } else if (zone === "main") {
            void addAttachmentsFromPaths(paths);
          }
        });
        if (cancelled) stopListening();
        else unlisten = stopListening;
      } catch {
        // Webview drag events are optional in browser preview.
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [addAttachmentsFromPaths, drop, tr]);

  useEffect(() => {
    if (!drop) return;
    const onDragOver = (event: globalThis.DragEvent) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
    };
    const onDrop = (event: globalThis.DragEvent) => {
      if (!event.dataTransfer?.files?.length) return;
      const paths = Array.from(event.dataTransfer.files)
        .map((file) => (file as File & { path?: string }).path || "")
        .filter(Boolean);
      const currentDrop = dropRef.current;
      if (!currentDrop) return;
      const zone = currentDrop.hitZone(event.clientX, event.clientY);
      if (!paths.length) return;
      event.preventDefault();
      event.stopPropagation();
      currentDrop.setDragZone(null);
      if (zone === "project") void currentDrop.onProjectPaths(paths);
      else if (zone === "main") void addAttachmentsFromPaths(paths);
    };
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("drop", onDrop);
    };
  }, [addAttachmentsFromPaths, drop]);

  return {
    attachments,
    attachmentsRef,
    setAttachments,
    attachmentLabels,
    addAttachmentsFromPaths,
    addPastedFiles,
    pickComposerFiles,
  };
}
