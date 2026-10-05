import assert from "node:assert/strict";
import test from "node:test";
import {
  FORBIDDEN_SOURCE_TEXT,
  SOURCE_RULE_IDS,
  scanPath,
  scanText,
} from "./clean-room-source-gate.mjs";

test("业务源码拒绝退役来源名称", () => {
  const findings = scanText("packages/ui/src/feature.tsx", `const source = "${FORBIDDEN_SOURCE_TEXT.legacyTheme}";`);
  assert.equal(findings.length, 1);
  assert.equal(findings[0].rule, SOURCE_RULE_IDS.legacyTheme);
});

test("第三方来源声明路径按规则级 allowlist 保留历史归属", () => {
  const findings = scanText(
    "third-party/zcode/SOURCE-MAPPING.md",
    `${FORBIDDEN_SOURCE_TEXT.legacyTheme} ${FORBIDDEN_SOURCE_TEXT.legacyHarness} ${FORBIDDEN_SOURCE_TEXT.legacyUiLibrary}`,
  );
  assert.deepEqual(findings, []);
});

test("旧来源目录名仍会被路径门禁发现", () => {
  const stalePath = `third-party/${FORBIDDEN_SOURCE_TEXT.legacyTheme}/NOTICE.md`;
  const findings = scanPath(stalePath);
  assert.equal(findings.length, 1);
  assert.equal(findings[0].rule, SOURCE_RULE_IDS.staleSourcePath);
});

test("普通文档不能借文件名或目录范围绕过来源检查", () => {
  const findings = scanText(
    "docs/architecture.md",
    `historical ${FORBIDDEN_SOURCE_TEXT.legacyHarness} reference`,
  );
  assert.equal(findings[0].rule, SOURCE_RULE_IDS.legacyHarness);
});

test("工作流源码拒绝 Node/Electron runtime，前端构建脚本不在此规则范围", () => {
  const findings = scanText(
    "packages/ui/src/workflows/runtime.ts",
    'import { app } from "electron";\nconst child = require("child_process");',
  );
  assert.equal(findings.length, 2);
  assert.ok(findings.every((finding) => finding.rule === SOURCE_RULE_IDS.workflowRuntime));
  assert.deepEqual(scanText("tooling/scripts/native-live-e2e.mjs", "#!/usr/bin/env node"), []);
});

test("浏览器服务契约不得混入 Node 宿主，构建工具和测试不受影响", () => {
  const host = 'import { readFile } from "node:fs/promises";';
  const findings = scanText("packages/services/src/file/fileService.ts", host);
  assert.equal(findings.length, 1);
  assert.equal(findings[0].rule, SOURCE_RULE_IDS.browserRuntime);
  assert.deepEqual(scanText("packages/ui/vite/cmapsPlugin.ts", host), []);
  assert.deepEqual(scanText("packages/services/src/file/fileService.test.ts", host), []);
  assert.equal(scanText("apps/ui/src/host.ts", 'const runtime = import("electron");').length, 1);
});
