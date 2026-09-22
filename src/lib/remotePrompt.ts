/** 标准 ACP Prompt 内容块；Web Host 附件使用 resource_link，不伪造本机路径。 */
export type SessionPromptContentBlock =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "resource_link";
      name: string;
      uri: string;
      mimeType?: string;
      size?: number;
    };

export interface RemotePromptAttachment {
  resourceId: string;
  fileName: string;
  contentType: string;
  size: number;
  /** 同源预览地址由上传响应返回；Prompt URI 仍由 resourceId canonical 化。 */
  previewUrl?: string;
}

/**
 * 构造 Web Host 使用的标准 Prompt blocks。
 * Host 只接受自己签发的资源 ID，uri 由这里统一生成 canonical 形式。
 */
export function buildRemotePrompt(
  text: string,
  attachments: readonly RemotePromptAttachment[],
): SessionPromptContentBlock[] {
  return [
    ...(text.trim() ? [{ type: "text" as const, text: text.trim() }] : []),
    ...attachments.map((attachment) => ({
      type: "resource_link" as const,
      name: attachment.fileName,
      uri: `/api/resources/${attachment.resourceId}`,
      mimeType: attachment.contentType,
      size: attachment.size,
    })),
  ];
}
