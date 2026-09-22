import { lazy, memo, Suspense, useMemo } from "react";
import { createT, type Locale } from "@/i18n";

const PierrePatchDiff = lazy(async () => {
  const module = await import("@pierre/diffs/react");
  return { default: module.PatchDiff };
});

export interface StructuredDiffPreviewProps {
  /** 统一差异文本。 */
  patch: string;
  /** 当前界面语言。 */
  locale: Locale;
}

/** Pierre 结构化渲染允许的最大 UTF-8 字节数。 */
export const STRUCTURED_DIFF_MAX_BYTES = 256 * 1024;
/** Pierre 结构化渲染允许的最大行数。 */
export const STRUCTURED_DIFF_MAX_LINES = 4_000;
/** 大型 Diff 原生文本预览允许的最大 UTF-8 字节数。 */
export const PLAIN_DIFF_PREVIEW_MAX_BYTES = 128 * 1024;
/** 大型 Diff 原生文本预览允许的最大行数。 */
export const PLAIN_DIFF_PREVIEW_MAX_LINES = 2_000;

/** 根据差异规模生成的渲染计划。 */
export interface StructuredDiffPreviewPlan {
  /** 是否使用 Pierre 结构化渲染器。 */
  usePierre: boolean;
  /** 降级时交给原生 pre 的文本。 */
  plainText: string;
  /** 原生文本是否因预览上限而截断。 */
  truncated: boolean;
}

/** 返回一个 Unicode 码点的 UTF-8 字节宽度。 */
function utf8CodePointWidth(codePoint: number): number {
  if (codePoint <= 0x7f) return 1;
  if (codePoint <= 0x7ff) return 2;
  if (codePoint <= 0xffff) return 3;
  return 4;
}

/** 判断差异是否处于 Pierre 的安全渲染边界内。 */
function fitsStructuredDiffLimits(patch: string): boolean {
  if (patch.length === 0) return true;
  let bytes = 0;
  let lines = 1;
  for (let index = 0; index < patch.length; ) {
    const codePoint = patch.codePointAt(index) ?? 0;
    bytes += utf8CodePointWidth(codePoint);
    if (bytes > STRUCTURED_DIFF_MAX_BYTES) return false;
    if (codePoint === 10) {
      lines += 1;
      if (lines > STRUCTURED_DIFF_MAX_LINES) return false;
    }
    index += codePoint > 0xffff ? 2 : 1;
  }
  return true;
}

/** 截取大型差异的轻量文本预览，同时约束 UTF-8 字节数与行数。 */
function slicePlainDiffPreview(patch: string): string {
  if (patch.length === 0) return "";
  let bytes = 0;
  let lines = 1;
  let end = 0;
  for (let index = 0; index < patch.length; ) {
    const codePoint = patch.codePointAt(index) ?? 0;
    if (codePoint === 10 && lines >= PLAIN_DIFF_PREVIEW_MAX_LINES) break;
    const nextBytes = bytes + utf8CodePointWidth(codePoint);
    if (nextBytes > PLAIN_DIFF_PREVIEW_MAX_BYTES) break;
    const width = codePoint > 0xffff ? 2 : 1;
    bytes = nextBytes;
    if (codePoint === 10) lines += 1;
    index += width;
    end = index;
  }
  return patch.slice(0, end);
}

/** 为统一差异选择结构化渲染或有界原生文本预览。 */
export function createStructuredDiffPreviewPlan(
  patch: string,
): StructuredDiffPreviewPlan {
  if (fitsStructuredDiffLimits(patch)) {
    return { usePierre: true, plainText: patch, truncated: false };
  }
  const plainText = slicePlainDiffPreview(patch);
  return {
    usePierre: false,
    plainText,
    truncated: plainText.length < patch.length,
  };
}

/** Pierre 渲染器属性。 */
interface PierreDiffProps {
  /** 统一差异文本。 */
  patch: string;
  /** 结构化渲染器加载中的本地化文案。 */
  loadingLabel: string;
  /** 跟随当前应用主题的渲染模式。 */
  themeType: "light" | "dark";
}

/** 仅在安全规模内挂载 Pierre 及其高亮渲染。 */
const PierreDiff = memo(function PierreDiff({
  patch,
  loadingLabel,
  themeType,
}: PierreDiffProps) {
  const options = useMemo(
    () => ({
      diffStyle: "unified" as const,
      diffIndicators: "none" as const,
      hunkSeparators: "line-info" as const,
      collapsedContextThreshold: 8,
      expansionLineCount: 20,
      overflow: "scroll" as const,
      stickyHeader: false,
      themeType,
    }),
    [themeType],
  );

  return (
    <Suspense fallback={<div className="rp-preview__msg">{loadingLabel}</div>}>
      <PierrePatchDiff patch={patch} options={options} disableWorkerPool />
    </Suspense>
  );
});

/**
 * @pierre/diffs 验证容器：仅为适中差异加载高亮器，大型差异使用原生文本。
 */
export function StructuredDiffPreview({
  patch,
  locale,
}: StructuredDiffPreviewProps) {
  const plan = useMemo(() => createStructuredDiffPreviewPlan(patch), [patch]);
  const tr = useMemo(() => createT(locale), [locale]);
  const themeType: "light" | "dark" =
    document.documentElement.dataset.theme === "light" ? "light" : "dark";

  if (!plan.usePierre) {
    return (
      <div className="rp-structured-diff rp-structured-diff--plain">
        <div className="rp-structured-diff__large-note" role="status">
          {tr("changes.largeDiffFallback", {
            lines: PLAIN_DIFF_PREVIEW_MAX_LINES,
            size: PLAIN_DIFF_PREVIEW_MAX_BYTES / 1024,
          })}
        </div>
        <pre className="rp-structured-diff__plain-text">{plan.plainText}</pre>
      </div>
    );
  }

  return (
    <div className="rp-structured-diff">
      <PierreDiff
        patch={patch}
        loadingLabel={tr("changes.loadingDiff")}
        themeType={themeType}
      />
    </div>
  );
}
