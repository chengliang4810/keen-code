import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "../..");
const scanDir = path.join(repoRoot, "apps", "ui", "appica-scan");
const tailwindCss = fs.readFileSync(
  path.join(repoRoot, "apps", "ui", "src", "styles", "tailwind.css"),
  "utf8",
);

test("appica scan target exists inside the ui project root", () => {
  assert.ok(
    fs.existsSync(path.join(scanDir, "components", "switch", "switch.js")),
    "apps/ui/appica-scan 缺少官方编译产物；运行 pnpm install 或 node tooling/scripts/sync-appica-scan.mjs",
  );
});

test("tailwind @source scans the synced directory and never node_modules", () => {
  const sources = [...tailwindCss.matchAll(/@source\s+"([^"]+)"/g)].map((match) => match[1]);
  assert.ok(
    sources.includes("../../appica-scan"),
    `tailwind.css 必须 @source 同步目录，当前：${sources.join(", ")}`,
  );
  for (const source of sources) {
    assert.ok(
      !source.includes("node_modules"),
      `@source 指向 node_modules 会在 vite 项目根外静默扫描为空：${source}`,
    );
  }
});
