import { describe, expect, it } from "vitest";
import { parseToolImageSrc, toolResultImageSources } from "./toolImages";

const artifact = { artifactId: "a".repeat(64), sha256: "a".repeat(64), sizeBytes: 800, mediaType: "image/png" };
const image = { type: "image", source: { type: "artifact", artifact } };
const result = (content: unknown[]) => ({ toolCallId: "read-image", isError: false, content });

describe("工具图片引用", () => {
  it("保留当前快照来源，支持多图和大结果图片引用，正文与二进制不复制到消息", () => {
    const sources = toolResultImageSources(result([{ type: "text", text: "Read image" }, image,
      { type: "artifact", materialization: "image", artifact }]), "read-image", "session-1")!;
    expect(sources).toHaveLength(2);
    expect(parseToolImageSrc(sources[0]!)).toEqual({ sessionId: "session-1", artifactId: artifact.artifactId, mediaType: "image/png" });
  });
  it("拒绝错误身份、失败结果、非法摘要、超大引用及危险 URL", () => {
    expect(toolResultImageSources(result([image]), "another-call", "session-1")).toBeUndefined();
    expect(toolResultImageSources({ ...result([image]), isError: true }, "read-image", "s")).toEqual([]);
    for (const patch of [{ artifactId: "../secret" }, { sha256: "b".repeat(64) }, { mediaType: "text/html" }, { sizeBytes: 26 * 1024 * 1024 }]) {
      expect(toolResultImageSources(result([{ type: "image", source: { type: "artifact", artifact: { ...artifact, ...patch } } }]), "read-image", "s")).toEqual([]);
    }
    for (const url of ["javascript:alert(1)", "file:///private/secret", "https://user:pass@example.test/image.png"]) {
      expect(toolResultImageSources(result([{ type: "image", source: { type: "url", url } }]), "read-image", "s")).toEqual([]);
    }
    expect(parseToolImageSrc(`keencode-image:%XX/${artifact.artifactId}/image%2Fpng`)).toBeNull();
  });
});
