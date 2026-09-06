import { describe, expect, it, vi } from "vitest";
import { loadSnapshotDiff } from "./resourceFileChange";
import { buildUnifiedDiff } from "./sessionChanges";
import type { FileChangeReference } from "./acp/fileChanges";

/** 空文件在原始字节协议中的固定 SHA-256。 */
const emptyHash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/** 构造未应用的准备快照，确认 UI 不将其显示成实际修改。 */
const prepared: FileChangeReference = {
  sessionId: "session-test", requestId: "request-test", path: "/repo/new.txt",
  before: null, after: { sizeBytes: 0, sha256: emptyHash }, applied: false,
};

describe("权威文件快照预览", () => {
  it("内联内容不调用工作区或快照读取接口，并保留 BOM/CRLF", async () => {
    const request = vi.fn();
    const patch = await loadSnapshotDiff({ path: "/repo/a", oldText: null, newText: "\uFEFF正文\r\n" }, request);
    expect(request).not.toHaveBeenCalled();
    expect(patch).toContain("@@ -0,0 +1,1 @@\n+\uFEFF正文\r\n");
  });
  it("Prepared 及路径伪造不读取或伪报 Applied", async () => {
    const request = vi.fn();
    await expect(loadSnapshotDiff({ path: prepared.path, reference: prepared }, request)).rejects.toThrow("尚未确认");
    await expect(loadSnapshotDiff({ path: "/other", reference: prepared }, request)).rejects.toThrow("路径");
    expect(request).not.toHaveBeenCalled();
  });
  it("空文件与不存在通过快照读取身份区分", async () => {
    const reference = { ...prepared, applied: true };
    const request = vi.fn(async () => ({ sessionId: reference.sessionId, requestId: reference.requestId,
      side: "after", offset: 0, totalBytes: 0, sha256: emptyHash, data: "", eof: true }));
    await loadSnapshotDiff({ path: reference.path, reference }, request);
    expect(request).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledWith("keencode/session/file-change/read", expect.objectContaining({ side: "after", length: 1 }));
  });
  it("读取错误直接传播，不降级为当前工作区内容", async () => {
    const request = vi.fn(async () => { throw new Error("快照实体不存在"); });
    await expect(loadSnapshotDiff({ path: prepared.path, reference: { ...prepared, applied: true } }, request)).rejects.toThrow("快照实体不存在");
  });
  it("拒绝二进制内联内容", async () => {
    await expect(loadSnapshotDiff({ path: "/a", oldText: "\0", newText: "text" }, vi.fn())).rejects.toThrow("二进制");
  });
});

describe("统一差异保留原始换行事实", () => {
  it("CRLF 到 LF 不能显示为空差异", () => {
    const patch = buildUnifiedDiff("a", "same\r\n", "same\n");
    expect(patch).toContain("-same\r\n+same\n");
    expect(patch).not.toContain("empty diff");
  });
  it("末尾换行增加或删除包含标准 No newline 标记", () => {
    expect(buildUnifiedDiff("a", "same", "same\n")).toContain("-same\n\\ No newline at end of file\n+same\n");
    expect(buildUnifiedDiff("a", "same\n", "same")).toContain("-same\n+same\n\\ No newline at end of file\n");
  });
  it("超多短行不使用无界操作数组，也不因展开参数抛出异常", () => {
    const before = "a\n".repeat(60_000);
    const patch = buildUnifiedDiff("a", before, "b\n");
    expect(patch).toContain("@@ -1,60000 +1,1 @@\n");
    expect(patch.endsWith("-a\n+b\n")).toBe(true);
  });
});
