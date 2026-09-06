import { describe, expect, it } from "vitest";
import {
  FILE_CHANGE_CHUNK_BYTES,
  loadFileChangeSnapshot,
  parseFileChangeReference,
  parseFileChangeResourceLink,
  fileChangeUri,
  type FileChangeReadParams,
  type FileChangeReference,
} from "./fileChanges";

const EMPTY_SHA256 =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/** 将原始字节编码为浏览器标准 Base64，测试与 Runtime 的 wire 形状一致。 */
function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

/** 计算完整快照的 SHA-256，避免测试使用只验证格式的伪造摘要。 */
async function sha256(bytes: Uint8Array): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0")
  ).join("");
}

/** 构造一份只含 after 快照的权威引用。 */
async function referenceFor(
  bytes: Uint8Array,
  options: Partial<Pick<FileChangeReference, "sessionId" | "requestId" | "path">> = {},
): Promise<FileChangeReference> {
  return {
    sessionId: options.sessionId ?? "session-1",
    requestId: options.requestId ?? "request-1",
    path: options.path ?? "C:/workspace/file.txt",
    before: null,
    after: { sizeBytes: bytes.byteLength, sha256: await sha256(bytes) },
    applied: true,
  };
}

/** 构造严格的分页响应；单页可以故意小于 request.length 以模拟分块边界。 */
function responseFor(
  reference: FileChangeReference,
  side: "before" | "after",
  offset: number,
  bytes: Uint8Array,
  eof: boolean,
): Record<string, unknown> {
  const snapshot = side === "before" ? reference.before : reference.after;
  if (!snapshot) throw new Error("测试分页不能读取不存在的 before 快照");
  return {
    sessionId: reference.sessionId,
    requestId: reference.requestId,
    side,
    offset,
    totalBytes: snapshot.sizeBytes,
    sha256: snapshot.sha256,
    data: toBase64(bytes),
    eof,
  };
}

describe("ACP 文件变更引用与按需快照读取", () => {
  it("严格绑定身份和 URI，不接受伪造或跨 Session 引用", async () => {
    const bytes = new TextEncoder().encode("正文");
    const reference = await referenceFor(bytes);
    const resource = {
      type: "resource_link",
      name: "file.txt",
      uri: fileChangeUri(reference.sessionId, reference.requestId),
      description: "已应用的持久文件快照",
      _meta: { "keencode/fileChange": reference },
    };
    expect(parseFileChangeReference(reference, "session-1")).toEqual(reference);
    expect(parseFileChangeReference(reference, "session-2")).toBeNull();
    expect(parseFileChangeResourceLink(resource, "session-1")).toEqual(reference);
    expect(parseFileChangeResourceLink({
      ...resource,
      uri: fileChangeUri(reference.sessionId, "other-request"),
    }, "session-1")).toBeNull();
    expect(parseFileChangeResourceLink({
      ...resource,
      _meta: {
        "keencode/fileChange": { ...reference, requestId: "other-request" },
      },
    }, "session-1")).toBeNull();
  });

  it("保留 BOM、CRLF 和空内容，并且 before=null 不发起读取", async () => {
    const bytes = new TextEncoder().encode("\uFEFFbefore\r\nafter\r\n");
    const reference = await referenceFor(bytes);
    const calls: FileChangeReadParams[] = [];
    const request = async (_method: "keencode/session/file-change/read", params: FileChangeReadParams) => {
      calls.push(params);
      return responseFor(reference, "after", params.offset, bytes, true);
    };
    await expect(loadFileChangeSnapshot(reference, "after", request)).resolves.toBe(
      "\uFEFFbefore\r\nafter\r\n",
    );
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({
      offset: 0,
      length: bytes.byteLength,
    });

    const empty = await referenceFor(new Uint8Array());
    const emptyCalls: FileChangeReadParams[] = [];
    await expect(loadFileChangeSnapshot(
      empty,
      "after",
      async (_method, params) => {
        emptyCalls.push(params);
        return responseFor(empty, "after", 0, new Uint8Array(), true);
      },
    )).resolves.toBe("");
    expect(emptyCalls).toEqual([{
      sessionId: "session-1",
      requestId: "request-1",
      side: "after",
      offset: 0,
      length: 1,
    }]);

    let beforeCalls = 0;
    await expect(loadFileChangeSnapshot(
      empty,
      "before",
      async () => {
        beforeCalls += 1;
        return null;
      },
    )).resolves.toBeNull();
    expect(beforeCalls).toBe(0);
  });

  it("跨 UTF-8 字节块拼接后再严格解码", async () => {
    const text = "\uFEFF甲\r\n乙🙂\r\n";
    const bytes = new TextEncoder().encode(text);
    const first = bytes.slice(0, 4);
    const second = bytes.slice(4, 7);
    const third = bytes.slice(7);
    const reference = await referenceFor(bytes);
    const pages = [first, second, third];
    const calls: FileChangeReadParams[] = [];
    await expect(loadFileChangeSnapshot(
      reference,
      "after",
      async (_method, params) => {
        calls.push(params);
        const page = pages[calls.length - 1];
        if (!page) throw new Error("测试请求超过预期页数");
        return responseFor(reference, "after", params.offset, page, calls.length === pages.length);
      },
    )).resolves.toBe(text);
    expect(calls.map(({ offset }) => offset)).toEqual([0, first.length, first.length + second.length]);
  });

  it("拒绝非法 UTF-8、完整 Hash 错误和不前进分页", async () => {
    const invalidBytes = new Uint8Array([0xff, 0xfe]);
    const invalidReference = await referenceFor(invalidBytes);
    await expect(loadFileChangeSnapshot(
      invalidReference,
      "after",
      async (_method, params) => responseFor(invalidReference, "after", params.offset, invalidBytes, true),
    )).rejects.toThrow("不是有效 UTF-8");

    const hashReference: FileChangeReference = {
      ...invalidReference,
      after: { sizeBytes: invalidBytes.byteLength, sha256: "0".repeat(64) },
    };
    await expect(loadFileChangeSnapshot(
      hashReference,
      "after",
      async (_method, params) => responseFor(hashReference, "after", params.offset, invalidBytes, true),
    )).rejects.toThrow("完整 SHA-256");

    const advancingReference = await referenceFor(new Uint8Array([1, 2]));
    let calls = 0;
    await expect(loadFileChangeSnapshot(
      advancingReference,
      "after",
      async (_method, params) => {
        calls += 1;
        return responseFor(advancingReference, "after", params.offset, new Uint8Array(), false);
      },
    )).rejects.toThrow("没有严格前进");
    expect(calls).toBe(1);
  });

  it("取消后不请求下一页，读取失败不自动重试", async () => {
    const bytes = new Uint8Array([1, 2, 3]);
    const reference = await referenceFor(bytes);
    const controller = new AbortController();
    let calls = 0;
    await expect(loadFileChangeSnapshot(
      reference,
      "after",
      async (_method, params) => {
        calls += 1;
        controller.abort();
        return responseFor(reference, "after", params.offset, bytes.slice(0, 1), false);
      },
      controller.signal,
    )).rejects.toMatchObject({ name: "AbortError" });
    expect(calls).toBe(1);

    let failedCalls = 0;
    await expect(loadFileChangeSnapshot(
      reference,
      "after",
      async () => {
        failedCalls += 1;
        throw new Error("读取失败");
      },
    )).rejects.toThrow("读取失败");
    expect(failedCalls).toBe(1);
  });

  it("最大 512 KiB 原始页经过 Base64 与 JSON-RPC 封装仍低于 1 MiB 响应预算", () => {
    const data = toBase64(new Uint8Array(FILE_CHANGE_CHUNK_BYTES));
    expect(FILE_CHANGE_CHUNK_BYTES).toBe(512 * 1024);
    expect(data.length).toBe(Math.ceil(FILE_CHANGE_CHUNK_BYTES / 3) * 4);
    const response = JSON.stringify({
      jsonrpc: "2.0",
      id: "read-1",
      result: {
        sessionId: "session-1",
        requestId: "request-1",
        side: "after",
        offset: 0,
        totalBytes: FILE_CHANGE_CHUNK_BYTES,
        sha256: EMPTY_SHA256,
        data,
        eof: true,
      },
    });
    expect(new TextEncoder().encode(response).byteLength).toBeLessThan(1024 * 1024);
  });
});
