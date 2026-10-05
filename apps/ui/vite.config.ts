import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");

const PIERRE_DIFFS_WORKER_ENTRY = "@pierre/diffs/worker/worker.js";

function keepPierreDiffsWorkerSideEffects(): Plugin {
  return {
    name: "zcode-keep-pierre-diffs-worker-side-effects",
    enforce: "post",
    async resolveId(source, importer) {
      if (source !== PIERRE_DIFFS_WORKER_ENTRY || importer == null) {
        return null;
      }

      const resolved = await this.resolve(source, importer, { skipSelf: true });
      return resolved == null ? null : { ...resolved, moduleSideEffects: true };
    },
    transform(code, id) {
      const normalizedId = id.replaceAll("\\", "/");
      if (!normalizedId.endsWith("/@pierre/diffs/dist/worker/worker.js")) {
        return null;
      }

      return { code, moduleSideEffects: true };
    },
  };
}

export default defineConfig({
  root: here,
  plugins: [react(), tailwindcss(), keepPierreDiffsWorkerSideEffects()],
  worker: {
    // Vite 为 Worker 创建独立的插件链；副作用标记必须在该链中重复注册。
    plugins: () => [keepPierreDiffsWorkerSideEffects()],
  },
  resolve: {
    alias: {
      "@": path.resolve(repoRoot, "packages/ui/src"),
      "@app": path.resolve(here, "src"),
    },
  },
  server: {
    host: "127.0.0.1",
    port: 1421,
    strictPort: true,
  },
  build: {
    outDir: path.resolve(here, "dist"),
    emptyOutDir: true,
    sourcemap: false,
  },
});
