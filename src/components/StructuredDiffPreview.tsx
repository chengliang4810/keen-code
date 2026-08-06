import { lazy, Suspense, useMemo } from "react";

const PierrePatchDiff = lazy(async () => {
  const module = await import("@pierre/diffs/react");
  return { default: module.PatchDiff };
});

export interface StructuredDiffPreviewProps {
  patch: string;
}

/**
 * @pierre/diffs 验证容器：只在选中变更文件时加载高亮器与 Diff UI。
 */
export function StructuredDiffPreview({ patch }: StructuredDiffPreviewProps) {
  const themeType: "light" | "dark" =
    document.documentElement.dataset.theme === "light" ? "light" : "dark";
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
    <div className="rp-structured-diff">
      <Suspense fallback={<div className="rp-preview__msg">正在加载 Diff…</div>}>
        <PierrePatchDiff patch={patch} options={options} disableWorkerPool />
      </Suspense>
    </div>
  );
}
