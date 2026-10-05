import { type ReactNode, useEffect, useMemo } from "react";
import {
  WorkerPoolContextProvider,
  useWorkerPool,
  type WorkerInitializationRenderOptions,
} from "@pierre/diffs/react";
import { createDiffsWorkerHighlighterOptions } from "@/lib/diffsHighlighterEngine.js";
import { logger } from "@/logger.js";
import { DEFAULT_CODE_PREVIEW_SETTINGS } from "@/store/index.js";
import { useZCodeStore } from "@/store/StoreProvider.js";

function createDiffsWorker(): Worker {
  return new Worker(new URL("../workers/diffs.worker.ts", import.meta.url), {
    type: "module",
    name: "zcode-diffs-worker",
  });
}

function resolveWorkerPoolSize(): number {
  if (typeof navigator === "undefined") {
    return 2;
  }

  const hardwareConcurrency = navigator.hardwareConcurrency;
  if (!Number.isFinite(hardwareConcurrency) || hardwareConcurrency <= 0) {
    return 2;
  }

  // 代码预览以交互延迟为主；四份 WASM/语法状态常驻的内存代价高于额外并发收益。
  return Math.max(1, Math.min(2, Math.floor(hardwareConcurrency / 2)));
}

function WorkerRenderOptionsSync({
  highlighterOptions,
}: {
  highlighterOptions: WorkerInitializationRenderOptions;
}) {
  const workerPool = useWorkerPool();

  useEffect(() => {
    if (!workerPool) {
      return;
    }

    void workerPool
      .setRenderOptions({
        theme: highlighterOptions.theme,
        lineDiffType: highlighterOptions.lineDiffType,
        maxLineDiffLength: highlighterOptions.maxLineDiffLength,
        tokenizeMaxLineLength: highlighterOptions.tokenizeMaxLineLength,
        useTokenTransformer: highlighterOptions.useTokenTransformer,
      })
      .catch((error: unknown) => {
        logger.warn("[DiffsWorkerPoolProvider] 同步渲染参数失败", {
          error: error instanceof Error ? error.message : String(error),
        });
      });
  }, [highlighterOptions, workerPool]);

  return null;
}

export function DiffsWorkerPoolProvider({ children, enabled = true }: { children: ReactNode; enabled?: boolean }) {
  // 仅在 File/Diff 实际挂载时创建共享池，最后一个预览卸载由上游 Provider 终止所有 Worker。
  const codePreviewSettings = useZCodeStore(
    (state) => state.codePreviewSettings ?? DEFAULT_CODE_PREVIEW_SETTINGS,
  );

  const highlighterOptions = useMemo<WorkerInitializationRenderOptions>(
    () =>
      createDiffsWorkerHighlighterOptions({
        lightTheme: codePreviewSettings.lightTheme,
        darkTheme: codePreviewSettings.darkTheme,
      }),
    [codePreviewSettings.darkTheme, codePreviewSettings.lightTheme],
  );

  const poolSize = useMemo(() => resolveWorkerPoolSize(), []);
  const canUseWorkerPool = enabled && typeof window !== "undefined" && typeof Worker !== "undefined";

  if (!canUseWorkerPool) {
    return <>{children}</>;
  }

  return (
    <WorkerPoolContextProvider
      poolOptions={{
        workerFactory: createDiffsWorker,
        poolSize,
        // 流式代码不断产生内容版本；只缓存少量 AST，避免保留上百份历史 token 树。
        totalASTLRUCacheSize: 16,
      }}
      highlighterOptions={highlighterOptions}
    >
      <WorkerRenderOptionsSync highlighterOptions={highlighterOptions} />
      {children}
    </WorkerPoolContextProvider>
  );
}
