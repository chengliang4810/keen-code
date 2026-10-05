import path from "node:path";
import { defineConfig } from "vitest/config";

const root = path.resolve(import.meta.dirname);

export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(root, "packages/ui/src"),
      "@app": path.resolve(root, "apps/ui/src"),
    },
  },
  test: {
    // 只纳入当前产品的契约测试目录和已审核的 services 连接边界测试，
    // 避免递归执行复制源码中的历史测试或未适配的宿主测试。
    include: [
      "apps/ui/test/**/*.test.{ts,tsx}",
      "packages/ui/test/**/*.test.{ts,tsx}",
      "packages/services/src/zcode-agent/zcodeAgentConnectionScope.test.ts",
    ],
  },
});
