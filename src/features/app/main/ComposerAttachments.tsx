import type { Dispatch, SetStateAction } from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { ComposerController } from "@/hooks/useComposerController";
import { AttachmentCard } from "@/components/AttachmentCard";
import { isImagePath, mergeAttachments, type Attachment } from "@/lib/attachments";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

export interface ComposerAttachmentsProps {
  tr: Translator;
  attachments: Attachment[];
  attachLabels: ComposerController["attachmentLabels"];
  setAttachments: SetState<Attachment[]>;
}

export function ComposerAttachments({
  tr,
  attachments,
  attachLabels,
  setAttachments,
}: ComposerAttachmentsProps) {
  if (attachments.length === 0) return null;
  const galleryPaths = attachments
    .filter((attachment) => !attachment.isDir && isImagePath(attachment.path))
    .map((attachment) => attachment.path);

  return (
    <div
      className="composer__attachments"
      aria-label={tr("composer.attachCount", { n: String(attachments.length) })}
    >
      {attachments.map((attachment) => (
        <AttachmentCard
          key={attachment.path}
          attachment={attachment}
          variant="chip"
          labels={attachLabels}
          galleryPaths={galleryPaths}
          onRemove={(removed) =>
            setAttachments((previous) =>
              previous.filter((item) => item.path !== removed.path),
            )
          }
          onAddToComposer={(added: Attachment) =>
            setAttachments((previous) => mergeAttachments(previous, [added]))
          }
        />
      ))}
    </div>
  );
}
