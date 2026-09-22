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
  hasUnreadyAttachments,
  mergeAttachments,
  type Attachment,
} from "@/lib/attachments";
import { getInjectedHostTransportAdapter } from "@/components/host/hostMode";
import type { HostUploadedAttachment } from "@/components/host/hostMode";
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

let pendingAttachmentSequence = 0;

function createPendingAttachmentPath(): string {
  const randomUuid = globalThis.crypto?.randomUUID?.();
  if (randomUuid) return `pending-attachment://${randomUuid}`;
  pendingAttachmentSequence += 1;
  return `pending-attachment://${Date.now()}-${pendingAttachmentSequence}`;
}

function createRemoteAttachmentPath(resourceId: string): string {
  return `remote-attachment://${resourceId}`;
}

/** 浏览器 Web Host 的文件选择器只负责取得 File，上传仍由 Host adapter 完成。 */
function pickBrowserFiles(): Promise<File[]> {
  if (typeof document === "undefined") return Promise.resolve([]);
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.tabIndex = -1;
    input.setAttribute("aria-hidden", "true");
    input.style.display = "none";
    const finish = () => {
      resolve(input.files ? Array.from(input.files) : []);
      input.remove();
    };
    input.addEventListener("change", finish, { once: true });
    input.addEventListener("cancel", finish, { once: true });
    (document.body ?? document.documentElement).appendChild(input);
    input.click();
  });
}

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
    retry: string;
    uploading: string;
    failed: string;
  };
  addAttachmentsFromPaths: (paths: string[]) => Promise<boolean>;
  addPastedFiles: (files: File[]) => Promise<void>;
  pickComposerFiles: () => Promise<void>;
  retryAttachment: (attachment: Attachment) => Promise<void>;
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
  const retryUploadsRef = useRef(new Map<string, () => Promise<boolean>>());

  const updateAttachment = useCallback(
    (path: string, update: (attachment: Attachment) => Attachment) => {
      setAttachments((previous) =>
        previous.map((attachment) =>
          attachment.path === path ? update(attachment) : attachment,
        ),
      );
    },
    [setAttachments],
  );

  const uploadRemoteFiles = useCallback(
    async (
      files: File[],
      uploadAttachment: (file: File) => Promise<HostUploadedAttachment>,
    ) => {
      const currentPorts = portsRef.current;
      let allSucceeded = true;
      for (const file of files) {
        const pendingPath = createPendingAttachmentPath();
        const fileName = file.name || "uploaded-file";
        const pending: Attachment = {
          source: "remote",
          path: pendingPath,
          name: fileName,
          isDir: false,
          uploadStatus: "uploading",
          uploadProgress: 0,
          contentType: file.type || "application/octet-stream",
          size: file.size,
        };
        setAttachments((previous) => mergeAttachments(previous, [pending]));
        const upload = async () => {
          try {
            updateAttachment(pendingPath, (attachment) => ({
              ...attachment,
              uploadProgress: 0.25,
            }));
            const uploaded = await uploadAttachment(file);
            if (
              !uploaded.resourceId ||
              !uploaded.fileName ||
              !uploaded.contentType ||
              !uploaded.previewUrl ||
              !Number.isSafeInteger(uploaded.size) ||
              uploaded.size < 0
            ) {
              throw new Error("Web Host 返回了无效的附件信息。");
            }
            updateAttachment(pendingPath, (attachment) => ({
              ...attachment,
              source: "remote",
              path: createRemoteAttachmentPath(uploaded.resourceId),
              name: uploaded.fileName,
              resourceId: uploaded.resourceId,
              contentType: uploaded.contentType,
              size: uploaded.size,
              previewUrl: uploaded.previewUrl,
              uploadStatus: "ready",
              uploadProgress: 1,
              uploadError: undefined,
            }));
            retryUploadsRef.current.delete(pendingPath);
            return true;
          } catch (cause) {
            const message = localizeUiError(cause, locale);
            updateAttachment(pendingPath, (attachment) => ({
              ...attachment,
              uploadStatus: "failed",
              uploadProgress: 0,
              uploadError: message,
            }));
            currentPorts.feedback.setLocalError(message);
            return false;
          }
        };
        retryUploadsRef.current.set(pendingPath, upload);
        if (!(await upload())) allSucceeded = false;
      }
      if (allSucceeded) currentPorts.feedback.setLocalError(null);
      return allSucceeded;
    },
    [locale, setAttachments, updateAttachment],
  );

  const addAttachmentsFromPaths = useCallback(
    async (paths: string[]) => {
      const currentPorts = portsRef.current;
      const request = currentPorts.navigation.location();
      if (!paths.length) {
        currentPorts.feedback.setLocalError(tr("attach.droppedNone"));
        return false;
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
          : [];
        if (!next.length) {
          currentPorts.feedback.setLocalError(tr("attach.droppedNone"));
          return false;
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
        return target === "current" || target === "snapshot";
      } catch (cause) {
        currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
        return false;
      }
    },
    [locale, setAttachments, tr],
  );

  const pickComposerFiles = useCallback(async () => {
    closeComposerMenu();
    const currentPorts = portsRef.current;
    if (!currentPorts.api.isTauri()) {
      const transport = getInjectedHostTransportAdapter();
      if (!transport?.uploadAttachment) {
        currentPorts.feedback.setLocalError(tr("composer.attachPasteFailed"));
        return;
      }
      try {
        const files = await pickBrowserFiles();
        if (!files.length) return;
        const uploaded = await uploadRemoteFiles(files, transport.uploadAttachment);
        if (uploaded) {
          const label = files.length === 1
            ? files[0]!.name || "uploaded-file"
            : tr("composer.attachCount", { n: String(files.length) });
          currentPorts.feedback.showToast(
            tr("composer.attachSaved", { name: label }),
            2200,
          );
        }
      } catch (cause) {
        currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
      }
      return;
    }
    try {
      const paths = await currentPorts.api.attachments.pickFiles();
      if (!paths.length) return;
      const attached = await addAttachmentsFromPaths(paths);
      if (!attached) return;
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
  }, [addAttachmentsFromPaths, closeComposerMenu, locale, tr, uploadRemoteFiles]);

  const addPastedFiles = useCallback(
    async (files: File[]) => {
      const currentPorts = portsRef.current;
      if (!files.length) return;
      const claimed = claimClipboardFiles(
        files,
        claimedClipboardFilesRef.current,
      );
      if (!claimed.length) return;
      try {
        if (!currentPorts.api.isTauri()) {
          const transport = getInjectedHostTransportAdapter();
          if (!transport?.uploadAttachment) {
            currentPorts.feedback.setLocalError(tr("composer.attachPasteFailed"));
            return;
          }
          await uploadRemoteFiles(claimed, transport.uploadAttachment);
          return;
        }
        let allSucceeded = true;
        for (const file of claimed) {
          const pendingPath = createPendingAttachmentPath();
          const pending: Attachment = {
            path: pendingPath,
            name: file.name || "pasted-file",
            isDir: false,
            uploadStatus: "uploading",
            uploadProgress: 0,
          };
          setAttachments((previous) => mergeAttachments(previous, [pending]));
          const upload = async () => {
            try {
              const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
              updateAttachment(pendingPath, (attachment) => ({
                ...attachment,
                uploadProgress: 0.5,
              }));
              const savedPath = await currentPorts.api.attachments.savePastedFile(
                file.name || "pasted-file",
                bytes,
              );
              updateAttachment(pendingPath, (attachment) => ({
                ...attachment,
                uploadProgress: 0.9,
              }));
              const attached = await addAttachmentsFromPaths([savedPath]);
              if (!attached) throw new Error(tr("attach.droppedNone"));
              setAttachments((previous) =>
                previous.filter((attachment) => attachment.path !== pendingPath),
              );
              retryUploadsRef.current.delete(pendingPath);
              return true;
            } catch (cause) {
              updateAttachment(pendingPath, (attachment) => ({
                ...attachment,
                uploadStatus: "failed",
                uploadProgress: 0,
                uploadError: localizeUiError(cause, locale),
              }));
              currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
              return false;
            }
          };
          retryUploadsRef.current.set(pendingPath, upload);
          if (!(await upload())) allSucceeded = false;
        }
        if (allSucceeded) currentPorts.feedback.setLocalError(null);
      } catch (cause) {
        currentPorts.feedback.setLocalError(localizeUiError(cause, locale));
      } finally {
        if (typeof window !== "undefined") {
          window.setTimeout(
            () => claimedClipboardFilesRef.current.clear(),
            500,
          );
        } else {
          claimedClipboardFilesRef.current.clear();
        }
      }
    },
    [
      addAttachmentsFromPaths,
      locale,
      setAttachments,
      tr,
      updateAttachment,
      uploadRemoteFiles,
    ],
  );

  const retryAttachment = useCallback(async (attachment: Attachment) => {
    const retry = retryUploadsRef.current.get(attachment.path);
    if (!retry) return;
    setAttachments((previous) =>
      previous.map((current) =>
        current.path === attachment.path
          ? { ...current, uploadStatus: "uploading", uploadProgress: 0, uploadError: undefined }
          : current,
      ),
    );
    const succeeded = await retry();
    if (succeeded && !hasUnreadyAttachments(attachmentsRef.current)) {
      portsRef.current.feedback.setLocalError(null);
    }
  }, [setAttachments]);

  const attachmentLabels = useMemo(
    () => ({
      open: tr("attach.open"),
      reveal: tr("attach.reveal"),
      copyPath: tr("attach.copyPath"),
      copyImage: tr("attach.copyImage"),
      addToComposer: tr("attach.addToComposer"),
      remove: tr("composer.attachRemove"),
      viewImage: tr("image.view"),
      retry: tr("composer.attachRetry"),
      uploading: tr("composer.attachUploading"),
      failed: tr("composer.attachFailed"),
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
    retryAttachment,
  };
}
