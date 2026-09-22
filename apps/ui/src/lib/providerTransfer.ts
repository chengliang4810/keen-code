/**
 * 模型供应商配置的导出文件名与导入前端预检规则。
 * 后端始终做最终校验；这里只负责把常见结构错误翻译成本地化提示。
 */

/** 导出文档结构，与后端 `keencode/providers-export` schema 对齐。 */
export interface ProviderExportDocument {
  schema: string;
  version: number;
  providers: Array<Record<string, unknown>>;
}

/** 从导出 JSON 提取供应商标识；结构未知时返回空数组。 */
export function providerExportIds(text: string): string[] {
  try {
    const parsed: unknown = JSON.parse(text);
    if (
      parsed &&
      typeof parsed === "object" &&
      Array.isArray((parsed as ProviderExportDocument).providers)
    ) {
      return (parsed as ProviderExportDocument).providers
        .map((provider) =>
          provider && typeof provider === "object"
            ? String((provider as { id?: unknown }).id ?? "")
            : "",
        )
        .filter(Boolean);
    }
  } catch {
    // 非 JSON 内容按无标识处理。
  }
  return [];
}

/** 导出文件名：使用供应商名称，名称不可用时回退固定前缀。 */
export function providerExportFilename(providerName: string): string {
  const base = providerName
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9\u4e00-\u9fff]+/gi, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  const suffix = base || "all";
  return `keencode-providers-${suffix}.json`;
}

/** 后端接受的导入 schema：导出文档与完整供应商配置文件。 */
const ACCEPTED_SCHEMAS = ["keencode/providers-export", "keencode/providers"] as const;

/** 导入文本的前端结构预检结果；ok 为 false 时携带错误码。 */
export type ProviderImportCheck =
  | { ok: true; count: number }
  | { ok: false; error: "json" | "schema" | "empty" };

/** 前端快速判断导入文本是否具备可提交结构；详细校验由后端完成。 */
export function checkProviderImportText(text: string): ProviderImportCheck {
  const trimmed = text.trim();
  if (!trimmed) return { ok: false, error: "json" };
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return { ok: false, error: "json" };
  }
  const schema = (parsed as { schema?: unknown } | null)?.schema;
  if (
    !parsed ||
    typeof parsed !== "object" ||
    !ACCEPTED_SCHEMAS.includes(schema as (typeof ACCEPTED_SCHEMAS)[number])
  ) {
    return { ok: false, error: "schema" };
  }
  const providers = (parsed as ProviderExportDocument).providers;
  if (!Array.isArray(providers) || providers.length === 0) {
    return { ok: false, error: "empty" };
  }
  return { ok: true, count: providers.length };
}
