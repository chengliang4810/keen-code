import type { Attachment } from "./attachments";

export interface DraftProjectLike {
  id?: string | null;
  path?: string | null;
}

/** 项目身份优先使用持久 id；无 id 时退回到规范化路径。 */
export type DraftProjectIdentity = string | null;

export interface DraftNavigationSnapshot {
  text: string;
  attachments: Attachment[];
  projectIdentity: DraftProjectIdentity;
  /** 用于阻止旧的异步附件结果写入另一个新草稿。 */
  draftKey: number;
}

export interface DraftNavigationLocation {
  sessionId: string | null;
  draftKey: number;
  viewEpoch: number;
}

export type DraftAttachmentUpdateTarget = "current" | "snapshot" | null;

/** 判断异步附件完成后应更新当前 composer、待恢复快照，还是丢弃结果。 */
export function draftAttachmentUpdateTarget(
  request: DraftNavigationLocation,
  current: DraftNavigationLocation,
  snapshot: DraftNavigationSnapshot | null,
): DraftAttachmentUpdateTarget {
  if (
    request.sessionId === null &&
    current.sessionId === null &&
    request.draftKey === current.draftKey
  ) {
    return "current";
  }
  if (
    request.sessionId !== null &&
    request.sessionId === current.sessionId &&
    request.draftKey === current.draftKey &&
    request.viewEpoch === current.viewEpoch
  ) {
    return "current";
  }
  if (request.sessionId === null && snapshot?.draftKey === request.draftKey) {
    return "snapshot";
  }
  return null;
}

function normalizeProjectPath(path: string): string {
  return path.trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

export function draftProjectIdentity(
  project: DraftProjectLike | null | undefined,
): DraftProjectIdentity {
  const id = project?.id?.trim();
  if (id) return `id:${id}`;

  const path = project?.path ? normalizeProjectPath(project.path) : "";
  return path ? `path:${path}` : null;
}

export function hasDraftContent(
  text: string,
  attachments: readonly Attachment[],
): boolean {
  return text.trim().length > 0 || attachments.length > 0;
}

export function snapshotDraftNavigation(
  text: string,
  attachments: readonly Attachment[],
  project: DraftProjectLike | null | undefined,
  draftKey: number,
): DraftNavigationSnapshot {
  return {
    text,
    attachments: attachments.map((attachment) => ({ ...attachment })),
    projectIdentity: draftProjectIdentity(project),
    draftKey,
  };
}

/** 返回匹配目标项目的副本；项目不匹配时返回 null。 */
export function restoreDraftNavigation(
  snapshot: DraftNavigationSnapshot | null,
  project: DraftProjectLike | null | undefined,
): DraftNavigationSnapshot | null {
  if (!snapshot || snapshot.projectIdentity !== draftProjectIdentity(project)) {
    return null;
  }
  return {
    ...snapshot,
    attachments: snapshot.attachments.map((attachment) => ({ ...attachment })),
  };
}

/** 将异步完成的附件合并回指定草稿快照，而不修改原快照。 */
export function mergeDraftNavigationAttachments(
  snapshot: DraftNavigationSnapshot,
  attachments: readonly Attachment[],
): DraftNavigationSnapshot {
  const byPath = new Map(snapshot.attachments.map((attachment) => [attachment.path, attachment]));
  for (const attachment of attachments) {
    if (!attachment.path) continue;
    byPath.set(attachment.path, { ...attachment });
  }
  return {
    ...snapshot,
    attachments: Array.from(byPath.values()),
  };
}
