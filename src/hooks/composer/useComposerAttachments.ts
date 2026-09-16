import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { createT, type Locale } from "@/i18n";
import { claimClipboardFiles } from "@/lib/clipboardPaste";
import {
  mergeAttachments,
  type Attachment,
} from "@/lib/attachments";
import { pathBasename } from "@/lib/filePath";
import {
  draftAttachmentUpdateTarget,
  mergeDraftNavigationAttachments,
} from "@/lib/draftNavigation";
import { localizeUiError } from "@/lib/session";
import type {
  ComposerApiPort,
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
}: UseComposerAttachmentsOptions): ComposerAttachmentsController {
  const tr = useMemo(() => createT(locale), [locale]);
  const portsRef = useRef({ api, navigation, feedback });
  portsRef.current = { api, navigation, feedback };

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
