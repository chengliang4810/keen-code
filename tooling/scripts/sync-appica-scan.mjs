import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

/**
 * 把 @appica/ui-react 的编译产物同步到 apps/ui/appica-scan，供 Tailwind `@source` 扫描。
 *
 * 背景：Tailwind 的 @source 只扫描 vite 项目根（apps/ui）内部的路径；仓库根的
 * node_modules（pnpm 下还是指向 .pnpm 的符号链接）在项目根之外，显式 @source 也
 * 静默扫描为空，官方组件内部的 utility 类不会生成、控件渲染成无样式状态。
 * 同步一份真实目录到项目根内是当前包管理器无关的可靠做法；该目录已 gitignore。
 */
const require = createRequire(import.meta.url);
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "../..");

const packageJsonPath = require.resolve("@appica/ui-react/package.json");
const distSource = path.join(path.dirname(packageJsonPath), "dist");
const target = path.join(repoRoot, "apps", "ui", "appica-scan");

if (!fs.existsSync(distSource)) {
  console.error(`[sync-appica-scan] 找不到官方编译产物：${distSource}`);
  process.exit(1);
}

fs.rmSync(target, { recursive: true, force: true });
fs.cpSync(distSource, target, { recursive: true });
console.log(
  `[sync-appica-scan] ${path.relative(repoRoot, distSource)} -> ${path.relative(repoRoot, target)}`,
);
