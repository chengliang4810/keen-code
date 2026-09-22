/** 工具图片只保留可恢复引用，图片字节在展开时读取。 */
const IMAGE_TYPE = /^image\/(?:png|jpeg|webp|gif|bmp|avif|svg\+xml|x-icon)$/u;

export function parseToolImageSrc(src: string): { sessionId: string; artifactId: string; mediaType: string } | null {
  const match = /^keencode-image:([^/]+)\/([a-f0-9]{64})\/([^/]+)$/u.exec(src);
  if (!match) return null;
  try {
    const sessionId = decodeURIComponent(match[1]!);
    const mediaType = decodeURIComponent(match[3]!);
    if (!sessionId || sessionId.length > 128 || /[\x00-\x1f\x7f]/u.test(sessionId) || !IMAGE_TYPE.test(mediaType)) return null;
    return { sessionId, artifactId: match[2]!, mediaType };
  } catch { return null; }
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 仅接受匹配调用身份、明确成功的当前原生工具结果。 */
export function toolResultImageSources(value: unknown, toolCallId: string, sessionId: string): string[] | undefined {
  if (!record(value) || value.toolCallId !== toolCallId || typeof value.isError !== "boolean" || !Array.isArray(value.content)) return undefined;
  if (value.isError) return [];
  return value.content.flatMap((part): string[] => {
    if (!record(part)) return [];
    const source = part.type === "image" && record(part.source) ? part.source
      : part.type === "artifact" && part.materialization === "image" ? part : null;
    if (!source) return [];
    if (source.type === "url" && typeof source.url === "string") {
      try {
        const url = new URL(source.url);
        return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password ? [url.href] : [];
      } catch { return []; }
    }
    const artifact = source.type === "artifact" && record(source.artifact) ? source.artifact : null;
    if (!artifact || typeof artifact.artifactId !== "string" || !/^[a-f0-9]{64}$/u.test(artifact.artifactId) ||
      artifact.sha256 !== artifact.artifactId || !Number.isSafeInteger(artifact.sizeBytes) ||
      Number(artifact.sizeBytes) < 0 || Number(artifact.sizeBytes) > 25 * 1024 * 1024 ||
      typeof artifact.mediaType !== "string" || !IMAGE_TYPE.test(artifact.mediaType)) return [];
    return [`keencode-image:${encodeURIComponent(sessionId)}/${artifact.artifactId}/${encodeURIComponent(artifact.mediaType)}`];
  });
}
