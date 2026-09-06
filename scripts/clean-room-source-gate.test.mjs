import assert from "node:assert/strict";
import test from "node:test";
import {
  FORBIDDEN_SOURCE_TEXT,
  SOURCE_RULE_IDS,
  formatGateReport,
  isScannablePath,
  normalizeRepositoryPath,
  scanPath,
  scanText,
} from "./clean-room-source-gate.mjs";

// 覆盖 clean-room 政策明确禁止的来源词、本地参考目录和历史 UI 参考措辞。
test("detects forbidden source names, research paths, and UI references", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const rules = SOURCE_RULE_IDS;
  const cases = [
    [
      "src/runtime.rs",
      `use ${source.legacyRuntime} as the runtime;`,
      rules.legacyRuntime,
    ],
    [
      "docs/runtime.md",
      `research ${source.researchProject} first`,
      rules.researchProject,
    ],
    [
      "src/plugin.rs",
      `${source.productWord} ${source.productAction} compatibility`,
      rules.externalProduct,
    ],
    [
      "docs/notes.md",
      `copied from .${source.pathReference}/source`,
      rules.localReferencePath,
    ],
    [
      "src/theme.css",
      `press scale (${source.uiBetter}-${source.uiName}: 0.96)`,
      rules.externalUiBetter,
    ],
    [
      "src/code.tsx",
      `${source.uiCursor}-style soft chrome`,
      rules.externalUiCursor,
    ],
    [
      "src/theme.css",
      ["Tuned to match product", " reference"].join(""),
      rules.externalUiReference,
    ],
    [
      "src/theme.css",
      ["Radius aligned to floating-panel", " reference"].join(""),
      rules.externalUiReference,
    ],
    [
      "src/theme.css",
      `Light chrome (reference: ${source.uiChat} settings)`,
      rules.externalUiReference,
    ],
  ];

  for (const [path, content, expectedRule] of cases) {
    const findings = scanText(path, content);
    assert.ok(
      findings.some((finding) => finding.ruleId === expectedRule),
      `${path} should match ${expectedRule}`,
    );
  }
});

// 覆盖明确排除的专有来源标识，以及针对开源代理的模仿性描述。
test("detects proprietary identifiers and open-agent imitation descriptions", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const rules = SOURCE_RULE_IDS;
  const capitalizedEditor = `${source.prohibitedEditor[0].toUpperCase()}${source.prohibitedEditor.slice(1)}`;
  const capitalizedAgent = `${source.openSourceAgent[0].toUpperCase()}${source.openSourceAgent.slice(1)}`;
  const cases = [
    [
      "config/plugins.json",
      `${source.productWord}_${source.pluginCollection}`,
      rules.proprietaryPluginDirectory,
    ],
    [
      "docs/sources.md",
      `${source.externalOrganization}/${source.pluginCollection}-${source.officialQualifier}`,
      rules.proprietaryPluginRepository,
    ],
    [
      "docs/sources.md",
      `${source.productWord}-${source.pluginCollection}-${source.officialQualifier}`,
      rules.proprietaryPluginRepository,
    ],
    ["src/editor.ts", capitalizedEditor, rules.prohibitedEditor],
    [
      "src/clipboard.ts",
      `${source.openSourceAgent}-${source.clipboardComponent}`,
      rules.externalClipboard,
    ],
    [
      "docs/design.md",
      `${capitalizedAgent}-${source.imitationStyle}`,
      rules.openAgentImitation,
    ],
    [
      "docs/design.md",
      `${capitalizedAgent}_${source.imitationLike}`,
      rules.openAgentImitation,
    ],
    [
      "docs/design.md",
      `${capitalizedAgent} ${source.imitationInspired}`,
      rules.openAgentImitation,
    ],
  ];

  for (const [path, content, expectedRule] of cases) {
    const findings = scanText(path, content);
    assert.ok(
      findings.some((finding) => finding.ruleId === expectedRule),
      `${path} should match ${expectedRule}`,
    );
  }
});

// 字母数字邻接会使来源词成为更长标识的一部分，此时不能误报。
test("requires alphanumeric boundaries for proprietary and imitation rules", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const identifiers = [
    `${source.productWord}_${source.pluginCollection}`,
    `${source.externalOrganization}/${source.pluginCollection}-${source.officialQualifier}`,
    `${source.productWord}-${source.pluginCollection}-${source.officialQualifier}`,
    source.prohibitedEditor,
    `${source.openSourceAgent}-${source.clipboardComponent}`,
    `${source.openSourceAgent}-${source.imitationStyle}`,
    `${source.openSourceAgent}_${source.imitationLike}`,
    `${source.openSourceAgent} ${source.imitationInspired}`,
  ];

  for (const identifier of identifiers) {
    for (const neighbor of ["x", "7"]) {
      assert.deepEqual(
        scanText("src/boundaries.ts", `${neighbor}${identifier}`),
        [],
        `leading adjacent alphanumeric must not match: ${identifier}`,
      );
      assert.deepEqual(
        scanText("src/boundaries.ts", `${identifier}${neighbor}`),
        [],
        `trailing adjacent alphanumeric must not match: ${identifier}`,
      );
    }
  }
});

// 开源代理名称及其普通仓库引用本身合法，只有专有组件名或模仿描述才禁止。
test("allows ordinary open-source agent references", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const content = [
    `OpenAI ${source.openSourceAgent} is open source.`,
    `https://github.com/openai/${source.openSourceAgent}`,
    `run ${source.openSourceAgent} in a local checkout`,
  ].join("\n");

  assert.deepEqual(scanText("docs/open-source-agent.md", content), []);
});

// 验证合法词汇边界：公开协议、通用类型和模型 ID 均不得误报。
test("does not flag protocols, generic types, model ids, or color names", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const content = [
    'const sample = "experimental/indigo";',
    "use std::io::Cursor;",
    "let mut cursor = 0;",
    "function merge(...values) { return values; }",
    "button { cursor: pointer; }",
    "OpenAI Responses API and Anthropic Messages API are supported.",
    `model = "${source.productWord}-sonnet-4-20250514"`,
    `model = "${source.productWord}-opus-4-1"`,
  ].join("\n");

  assert.deepEqual(scanText("src/legal-examples.rs", content), []);
  assert.deepEqual(scanText("experimental/indigo/config.ts", "ok"), []);
});

// 验证许可证例外仅允许法定名称，不能掩盖同一文件中的本地研究路径。
test("allows legal attribution only at exact root legal paths", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const legalNames = [
    source.legacyRuntime,
    source.researchProject,
    `${source.productWord} ${source.productAction}`,
    `${source.uiBetter}-${source.uiName}`,
  ].join("\n");

  assert.deepEqual(scanText("LICENSE", legalNames), []);
  assert.ok(scanText("README.md", legalNames).length > 0);
  assert.ok(scanText("docs/LICENSE", legalNames).length > 0);
  assert.deepEqual(
    scanText("LICENSE", `.${source.pathReference}/source`).map(
      (finding) => finding.ruleId,
    ),
    [SOURCE_RULE_IDS.localReferencePath],
  );
});

// 验证研究目录只允许出现在根忽略文件，其他路径上的同名配置仍会失败。
test("allows research directory names only in the root ignore file", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  assert.deepEqual(
    scanText(
      ".gitignore",
      `.${source.pathReference}\n.${source.pathSpike}`,
    ),
    [],
  );
  assert.deepEqual(
    scanText("config/.gitignore", `.${source.pathReference}`).map(
      (finding) => finding.ruleId,
    ),
    [SOURCE_RULE_IDS.localReferencePath],
  );
});

// 验证扫描范围覆盖常用源码、文档和配置，并排除二进制资源。
test("selects versioned source, documentation, and configuration files", () => {
  assert.equal(isScannablePath("crates/runtime/src/lib.rs"), true);
  assert.equal(isScannablePath("docs/architecture.md"), true);
  assert.equal(isScannablePath(".github/workflows/test.yml"), true);
  assert.equal(isScannablePath("Cargo.lock"), true);
  assert.equal(isScannablePath("public/logo.png"), false);
  assert.equal(normalizeRepositoryPath("src\\lib\\agent.ts"), "src/lib/agent.ts");
});

// 二进制内容不读取，但其版本化路径仍须经过来源名称门禁。
test("scans forbidden source names in binary asset paths", () => {
  const source = FORBIDDEN_SOURCE_TEXT;
  const binaryPath = `public/${source.researchProject}-reference.png`;
  assert.equal(isScannablePath(binaryPath), false);
  assert.deepEqual(
    scanPath(binaryPath).map(
      (finding) => finding.ruleId,
    ),
    [SOURCE_RULE_IDS.researchProject],
  );
});

// 验证报告包含稳定路径、行列与规则标识，便于在本地和 CI 中定位。
test("formats a deterministic actionable failure report", () => {
  const findings = scanText(
    "src/runtime.rs",
    `safe\n${FORBIDDEN_SOURCE_TEXT.legacyRuntime} runtime`,
  );
  const report = formatGateReport({ scannedFiles: 1, findings });

  assert.ok(report.includes(`src/runtime.rs:2:1 [${SOURCE_RULE_IDS.legacyRuntime}]`));
  assert.match(report, /发现 1 处/);
});

// 成功报告同时展示路径覆盖面与实际读取的文本文件数。
test("formats path and text coverage in a successful report", () => {
  const report = formatGateReport({
    scannedPaths: 2,
    scannedFiles: 1,
    findings: [],
  });

  assert.match(report, /已检查 2 个版本化路径/);
  assert.match(report, /扫描 1 个源码、文档和配置文件/);
});
