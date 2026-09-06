/**
 * ACP 文件变更引用与按需快照读取。
 *
 * 大文件正文不进入 SessionUpdate；Runtime 只发送当前 Session、请求和快照摘要，
 * UI 在用户打开 Diff 时通过专用 Client 请求按原始字节分块读取，绝不读取工作区文件。
 */

/** 单份文件快照允许引用的最大原始字节数。 */
export const MAX_FILE_CHANGE_SNAPSHOT_BYTES = 64 * 1024 * 1024;
/**
 * 单页按需读取允许的最大原始字节数。
 *
 * ACP 默认响应预算为 1 MiB；512 KiB 原始字节经过 Base64 编码后仍为
 * 约 683 KiB，给 JSON 元数据和 JSON-RPC 封装保留了足够空间。
 */
export const FILE_CHANGE_CHUNK_BYTES = 512 * 1024;

/** Session、请求等标识允许的最大 UTF-8 字节数。 */
const MAX_FILE_CHANGE_ID_BYTES = 128;
/** 文件路径允许的最大 UTF-8 字节数；与 ACP Rust 协议边界保持一致。 */
const MAX_FILE_CHANGE_PATH_BYTES = 32 * 1024;
/** 空快照的 SHA-256 固定值；空文件不能携带任意伪造摘要。 */
const EMPTY_FILE_CHANGE_SHA256 =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/** 文件快照读取最多允许的页数，防止异常响应造成无限循环。 */
const MAX_FILE_CHANGE_PAGES =
  Math.ceil(MAX_FILE_CHANGE_SNAPSHOT_BYTES / FILE_CHANGE_CHUNK_BYTES);
/** 单页 Base64 编码允许的最大字符数。 */
const MAX_FILE_CHANGE_BASE64_CHARS =
  Math.ceil(FILE_CHANGE_CHUNK_BYTES / 3) * 4;

/** 文件快照的固定大小和 SHA-256 摘要。 */
export interface FileChangeSnapshot {
  /** 包含 BOM、原始换行和其他原始字节的完整长度。 */
  sizeBytes: number;
  /** 完整原始字节的小写 SHA-256。 */
  sha256: string;
}

/** Runtime 通过标准 ACP resource_link 元数据发送的精确文件变更引用。 */
export interface FileChangeReference {
  /** 该引用所属的根 Session。 */
  sessionId: string;
  /** 形成该快照的工具请求标识。 */
  requestId: string;
  /** 实际变更文件路径。 */
  path: string;
  /** 变更前快照；null 明确表示文件原先不存在。 */
  before: FileChangeSnapshot | null;
  /** 变更后快照；始终存在，即使文件为空。 */
  after: FileChangeSnapshot;
  /** 文件变更是否已经实际应用到工作区。 */
  applied: boolean;
}

/** 可按需读取的文件快照侧。 */
export type FileChangeSide = "before" | "after";

/** 文件快照读取请求的严格参数。 */
export interface FileChangeReadParams {
  /** 该引用所属的根 Session。 */
  sessionId: string;
  /** 形成该快照的工具请求标识。 */
  requestId: string;
  /** 要读取的快照侧。 */
  side: FileChangeSide;
  /** 按原始字节计数的下一页偏移。 */
  offset: number;
  /** 本次最多读取的原始字节数；客户端固定使用单页上限。 */
  length: number;
}

/** 供 ACP Client 使用的单次文件快照读取函数。 */
export type FileChangeReadRequest = (
  method: "keencode/session/file-change/read",
  params: FileChangeReadParams,
) => Promise<unknown>;

/** 判断值是否为普通 JSON 对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 判断对象是否精确包含指定键集合。 */
function hasExactKeys(
  value: Record<string, unknown>,
  keys: readonly string[],
): boolean {
  if (Object.keys(value).length !== keys.length) return false;
  return keys.every((key) => Object.hasOwn(value, key));
}

/** 计算 UTF-8 字节数；无效的 UTF-16 代理由调用方的 URI 检查拒绝。 */
function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** 判断字符串是否只包含有限、非空且不含控制字符的文本。 */
function isBoundedText(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.trim().length > 0 &&
    utf8ByteLength(value) <= maxBytes &&
    ![...value].some((character) => /[\u0000-\u001F\u007F-\u009F]/u.test(character));
}

/** 判断 SHA-256 是否使用固定的小写十六进制表示。 */
function isSha256(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
}

/** 判断字节数量是否为安全整数且位于快照边界内。 */
function isSnapshotSize(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) &&
    value >= 0 && value <= MAX_FILE_CHANGE_SNAPSHOT_BYTES;
}

/** 返回标准文件变更 URI；调用方必须先通过 descriptor 文本校验。 */
export function fileChangeUri(sessionId: string, requestId: string): string {
  return `keencode://sessions/${encodeURIComponent(sessionId)}/file-changes/${encodeURIComponent(requestId)}`;
}

/** 安全生成标准 URI，避免 lone surrogate 让 encodeURIComponent 抛出未分类异常。 */
function expectedFileChangeUri(
  sessionId: string,
  requestId: string,
): string | null {
  try {
    return fileChangeUri(sessionId, requestId);
  } catch {
    return null;
  }
}

/** 严格解析一个文件快照摘要。 */
function parseFileChangeSnapshot(value: unknown): FileChangeSnapshot | null {
  if (!isRecord(value) || !hasExactKeys(value, ["sizeBytes", "sha256"]) ||
    !isSnapshotSize(value.sizeBytes) || !isSha256(value.sha256) ||
    (value.sizeBytes === 0 && value.sha256 !== EMPTY_FILE_CHANGE_SHA256)) {
    return null;
  }
  return {
    sizeBytes: value.sizeBytes,
    sha256: value.sha256,
  };
}

/**
 * 严格解析 `keencode/fileChange` 引用。
 *
 * `expectedSessionId` 用于把解析结果绑定到当前 Session；不匹配时返回 null，
 * 由调用方丢弃引用而不是把跨 Session 数据投影成文件 Diff。
 */
export function parseFileChangeReference(
  value: unknown,
  expectedSessionId?: string,
): FileChangeReference | null {
  if (!isRecord(value) || !hasExactKeys(value, [
    "sessionId",
    "requestId",
    "path",
    "before",
    "after",
    "applied",
  ]) ||
    !isBoundedText(value.sessionId, MAX_FILE_CHANGE_ID_BYTES) ||
    !isBoundedText(value.requestId, MAX_FILE_CHANGE_ID_BYTES) ||
    !isBoundedText(value.path, MAX_FILE_CHANGE_PATH_BYTES) ||
    (expectedSessionId !== undefined && value.sessionId !== expectedSessionId) ||
    (value.before !== null && parseFileChangeSnapshot(value.before) === null) ||
    parseFileChangeSnapshot(value.after) === null ||
    typeof value.applied !== "boolean") {
    return null;
  }

  const before = value.before === null ? null : parseFileChangeSnapshot(value.before);
  const after = parseFileChangeSnapshot(value.after);
  if (before === undefined || after === null) return null;

  // URI 编码必须可执行且与 descriptor 身份保持可重建的一致性。
  if (!expectedFileChangeUri(value.sessionId, value.requestId)) return null;

  return {
    sessionId: value.sessionId,
    requestId: value.requestId,
    path: value.path,
    before,
    after,
    applied: value.applied,
  };
}

/**
 * 严格解析标准 ACP `resource_link` 中的 `keencode/fileChange` 元数据。
 * 资源链接必须使用与引用身份完全一致的标准 URI。
 */
export function parseFileChangeResourceLink(
  value: unknown,
  expectedSessionId?: string,
): FileChangeReference | null {
  if (!isStandardResourceLink(value) ||
    !isRecord(value._meta)) {
    return null;
  }
  if (!Object.hasOwn(value._meta, "keencode/fileChange")) return null;
  const reference = parseFileChangeReference(
    value._meta["keencode/fileChange"],
    expectedSessionId,
  );
  if (!reference || value.uri !== fileChangeUri(reference.sessionId, reference.requestId)) {
    return null;
  }
  return reference;
}

/** 按 ACP ResourceLink Schema 校验必需字段和正式可选描述字段，不拒绝合法服务端输出。 */
export function isStandardResourceLink(value: unknown): value is Record<string, unknown> & {
  type: "resource_link"; name: string; uri: string;
} {
  if (!isRecord(value) || value.type !== "resource_link" ||
    !isBoundedText(value.name, MAX_FILE_CHANGE_PATH_BYTES) ||
    !isBoundedText(value.uri, MAX_FILE_CHANGE_PATH_BYTES) ||
    Object.keys(value).some((key) => ![
      "type", "name", "uri", "_meta", "description", "mimeType", "size", "title", "annotations",
    ].includes(key)) || (value._meta !== undefined && !isRecord(value._meta))) return false;
  for (const key of ["description", "mimeType", "title"]) {
    if (value[key] != null && typeof value[key] !== "string") return false;
  }
  if (value.size != null && (typeof value.size !== "number" ||
    !Number.isSafeInteger(value.size) || value.size < 0)) return false;
  const annotations = value.annotations;
  if (annotations != null) {
    if (!isRecord(annotations) || Object.keys(annotations).some((key) =>
      !["audience", "lastModified", "priority", "_meta"].includes(key))) return false;
    if (annotations.audience != null && (!Array.isArray(annotations.audience) ||
      annotations.audience.some((role) => role !== "user" && role !== "assistant"))) return false;
    if (annotations.lastModified != null && typeof annotations.lastModified !== "string") return false;
    if (annotations.priority != null && (typeof annotations.priority !== "number" ||
      !Number.isFinite(annotations.priority) || annotations.priority < 0 || annotations.priority > 1)) return false;
    if (annotations._meta != null && !isRecord(annotations._meta)) return false;
  }
  return true;
}

/** 判断 Base64 是否是无空白、规范填充的标准编码。 */
function isCanonicalBase64(value: string): boolean {
  if (value.length > MAX_FILE_CHANGE_BASE64_CHARS ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)) {
    return false;
  }
  try {
    const bytes = atob(value);
    let binary = "";
    for (let index = 0; index < bytes.length; index += 1) {
      binary += bytes[index];
    }
    return btoa(binary) === value;
  } catch {
    return false;
  }
}

/** 解码单页 Base64，不接受宽松的浏览器容错形式。 */
function decodeBase64(value: string): Uint8Array | null {
  if (!isCanonicalBase64(value)) return null;
  try {
    const binary = atob(value);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  } catch {
    return null;
  }
}

/** 严格解析单页读取响应的形状。 */
function parseFileChangeReadPage(value: unknown): {
  sessionId: string;
  requestId: string;
  side: FileChangeSide;
  offset: number;
  totalBytes: number;
  sha256: string;
  data: string;
  eof: boolean;
} | null {
  if (!isRecord(value) || !hasExactKeys(value, [
    "sessionId",
    "requestId",
    "side",
    "offset",
    "totalBytes",
    "sha256",
    "data",
    "eof",
  ]) ||
    !isBoundedText(value.sessionId, MAX_FILE_CHANGE_ID_BYTES) ||
    !isBoundedText(value.requestId, MAX_FILE_CHANGE_ID_BYTES) ||
    (value.side !== "before" && value.side !== "after") ||
    !isSnapshotSize(value.offset) || !isSnapshotSize(value.totalBytes) ||
    !isSha256(value.sha256) || typeof value.data !== "string" ||
    !isCanonicalBase64(value.data) || typeof value.eof !== "boolean") {
    return null;
  }
  return {
    sessionId: value.sessionId,
    requestId: value.requestId,
    side: value.side,
    offset: value.offset,
    totalBytes: value.totalBytes,
    sha256: value.sha256,
    data: value.data,
    eof: value.eof,
  };
}

/** 创建稳定的 AbortError，兼容浏览器和 Node 测试运行时。 */
function abortError(): Error {
  if (typeof DOMException === "function") {
    return new DOMException("文件快照读取已取消", "AbortError");
  }
  const error = new Error("文件快照读取已取消");
  error.name = "AbortError";
  return error;
}

/** 在每次请求前后检查取消信号，禁止取消后继续翻页。 */
function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw abortError();
}

/** 将已验证的字节块拼接为一份完整原始快照。 */
function concatenateChunks(chunks: readonly Uint8Array[], totalBytes: number): Uint8Array {
  const bytes = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

/** 计算 Web Crypto SHA-256 的小写十六进制结果。 */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0")
  ).join("");
}

/**
 * 按需读取并严格验证一侧文件快照，返回保留 BOM 和原始换行语义的文本。
 * before 为 null 时不发起任何请求；非法 UTF-8 或完整摘要不匹配均明确失败。
 */
export async function loadFileChangeSnapshot(
  reference: FileChangeReference,
  side: FileChangeSide,
  requestFn: FileChangeReadRequest,
  signal?: AbortSignal,
): Promise<string | null> {
  const parsedReference = parseFileChangeReference(reference, reference.sessionId);
  if (!parsedReference) throw new Error("文件变更引用无效");
  const snapshot = side === "before" ? parsedReference.before : parsedReference.after;
  if (!snapshot) return null;

  const chunks: Uint8Array[] = [];
  let offset = 0;
  let pageCount = 0;
  while (true) {
    throwIfAborted(signal);
    if (pageCount >= MAX_FILE_CHANGE_PAGES) {
      throw new Error("文件快照分页超过安全上限");
    }
    pageCount += 1;
    // Runtime 要求 length 为正数；空快照仍发起一次 length=1 请求来核验
    // Session/request 身份、总长度和空内容 SHA-256，而不会读取任何正文。
    const length = Math.min(
      FILE_CHANGE_CHUNK_BYTES,
      Math.max(1, snapshot.sizeBytes - offset),
    );
    const response = await requestFn("keencode/session/file-change/read", {
      sessionId: parsedReference.sessionId,
      requestId: parsedReference.requestId,
      side,
      offset,
      length,
    });
    throwIfAborted(signal);

    const page = parseFileChangeReadPage(response);
    if (!page || page.sessionId !== parsedReference.sessionId ||
      page.requestId !== parsedReference.requestId || page.side !== side ||
      page.offset !== offset || page.totalBytes !== snapshot.sizeBytes ||
      page.sha256 !== snapshot.sha256) {
      throw new Error("文件快照分页身份、偏移、长度或摘要不匹配");
    }
    const bytes = decodeBase64(page.data);
    if (!bytes || bytes.byteLength > FILE_CHANGE_CHUNK_BYTES) {
      throw new Error("文件快照分页 Base64 或字节上限无效");
    }
    const nextOffset = offset + bytes.byteLength;
    if (!Number.isSafeInteger(nextOffset) || nextOffset > snapshot.sizeBytes) {
      throw new Error("文件快照分页超出声明长度");
    }
    if (page.eof) {
      if (nextOffset !== snapshot.sizeBytes) {
        throw new Error("文件快照提前结束或结束偏移不匹配");
      }
      chunks.push(bytes);
      break;
    }
    if (bytes.byteLength === 0 || nextOffset <= offset || nextOffset >= snapshot.sizeBytes) {
      throw new Error("文件快照分页没有严格前进");
    }
    chunks.push(bytes);
    offset = nextOffset;
  }

  const bytes = concatenateChunks(chunks, snapshot.sizeBytes);
  if (bytes.byteLength !== snapshot.sizeBytes || await sha256Hex(bytes) !== snapshot.sha256) {
    throw new Error("文件快照完整 SHA-256 校验失败");
  }
  throwIfAborted(signal);
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw new Error("文件快照不是有效 UTF-8 文本");
  }
}
