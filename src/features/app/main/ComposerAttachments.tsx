import type { Dispatch, SetStateAction } from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { ComposerController } from "@/hooks/useComposerController";
import { AttachmentCard } from "@/components/AttachmentCard";
import {
  isImageAttachment,
  mergeAttachments,
  type Attachment,
} from "@/lib/attachments";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

export interface ComposerAttachmentsProps {
  tr: Translator;
  attachments: Attachment[];
  attachLabels: ComposerController["attachmentLabels"];
  setAttachments: SetState<Attachment[]>;
  retryAttachment?: ComposerController["retryAttachment"];
}

export function ComposerAttachments({
  tr,
  attachments,
  attachLabels,
  setAttachments,
  retryAttachment,
}: ComposerAttachmentsProps) {
  if (attachments.length === 0) return null;
  // 与 ZCode 附件网格一致：图片优先，普通文件保持各自的添加顺序，
  // 让预览区域的媒体入口和连续查看顺序稳定，不受混合粘贴顺序影响。
  const orderedAttachments = [...attachments].sort((left, right) => {
    const leftIsImage = isImageAttachment(left);
    const rightIsImage = isImageAttachment(right);
    return Number(rightIsImage) - Number(leftIsImage);
  });
  const galleryPaths = orderedAttachments
    .filter(isImageAttachment)
    .map((attachment) => attachment.previewUrl ?? attachment.path);

  return (
    <div
      className="composer__attachments"
      aria-label={tr("composer.attachCount", { n: String(attachments.length) })}
    >
      {orderedAttachments.map((attachment) => (
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
          onRetry={retryAttachment}
        />
      ))}
    </div>
  );
}
