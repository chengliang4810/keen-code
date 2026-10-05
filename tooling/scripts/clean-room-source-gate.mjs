import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { extname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_DIR = resolve(fileURLToPath(import.meta.url), "..");
export const REPO_ROOT = resolve(SCRIPT_DIR, "../..");

/** 只扫描可读的源码、文档和配置；图片、字体及构建产物不参与来源审查。 */
const SCANNABLE_EXTENSIONS = new Set([
  ".c", ".cc", ".cfg", ".conf", ".cpp", ".css", ".go", ".h", ".hpp", ".html",
  ".ini", ".java", ".js", ".json", ".jsonc", ".jsx", ".lock", ".md", ".mdx", ".mjs",
  ".ps1", ".py", ".rs", ".scss", ".sh", ".sql", ".toml", ".ts", ".tsx", ".txt",
  ".vue", ".xml", ".yaml", ".yml",
]);
const SCANNABLE_FILENAMES = new Set([
  ".gitattributes", ".gitignore", ".npmrc", "cargo.lock", "dockerfile", "license",
  "makefile",
]);
const MAX_REPORTED_FINDINGS = 100;
const GIT_FILE_LIST_MAX_BYTES = 16 * 1024 * 1024;

/**
 * 来源门禁聚焦前端、文档、依赖清单和门禁脚本。Rust Agent/桌面历史实现有自己
 * 的产品命名和兼容测试，不属于本次 ZCode UI 来源审查，也不能通过修改 Rust
 * 来制造“清洁”结果。
 */
export const SOURCE_SCAN_SCOPE = Object.freeze([
  "packages/",
  "src/",
  "apps/ui/",
  "docs/",
  "third-party/",
  "tooling/scripts/",
  ".github/",
  "DESIGN.md",
  "AGENTS.md",
  "THIRD_PARTY_NOTICES.md",
  "package.json",
  "pnpm-lock.yaml",
  "pnpm-workspace.yaml",
  "tsconfig.base.json",
]);

function textFromCodePoints(points) {
  return String.fromCodePoint(...points);
}

function boundedPattern(value) {
  const escaped = value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?<![A-Za-z0-9])${escaped}(?![A-Za-z0-9])`, "i");
}

/** 旧来源词使用代码点构造，避免门禁源码反过来触发自身检查。 */
export const FORBIDDEN_SOURCE_TEXT = Object.freeze({
  legacyTheme: textFromCodePoints([0x53, 0x79, 0x6e, 0x61, 0x72, 0x61]),
  legacyHarness: textFromCodePoints([
    0x64, 0x65, 0x65, 0x70, 0x73, 0x65, 0x65, 0x6b, 0x2d, 0x68, 0x61, 0x72, 0x6e, 0x65, 0x73, 0x73,
  ]),
  legacyUiLibrary: textFromCodePoints([0x41, 0x70, 0x70, 0x69, 0x63, 0x61]),
});

export const SOURCE_RULE_IDS = Object.freeze({
  legacyTheme: "source-legacy-theme",
  legacyHarness: "source-legacy-harness",
  legacyUiLibrary: "source-legacy-ui-library",
  staleSourcePath: "source-stale-path",
  workflowRuntime: "workflow-runtime-boundary",
  browserRuntime: "browser-runtime-boundary",
});

const RULES = Object.freeze([
  {
    id: SOURCE_RULE_IDS.legacyTheme,
    value: FORBIDDEN_SOURCE_TEXT.legacyTheme,
    message: "检测到已退役的前端来源名称；当前权威基线必须使用 ZCode 清单。",
  },
  {
    id: SOURCE_RULE_IDS.legacyHarness,
    value: FORBIDDEN_SOURCE_TEXT.legacyHarness,
    message: "检测到已退役的主题来源名称或路径；请迁移到 third-party/zcode/。",
  },
  {
    id: SOURCE_RULE_IDS.legacyUiLibrary,
    value: FORBIDDEN_SOURCE_TEXT.legacyUiLibrary,
    message: "检测到已退役的控件规范引用；请使用 DESIGN.md 与 ZCode token 规则。",
  },
]);

const WORKFLOW_SOURCE_PATH = /(^|\/)(workflow|workflows|workflow-runtime|workflow-engine)(\/|[-_.])/i;
const FORBIDDEN_WORKFLOW_RUNTIME =
  /\b(?:import\s+(?:[^;]*?\s+from\s+)?["'](?:electron|node:[^"']+)["']|require\s*\(\s*["'](?:electron|node:[^"']+|child_process)["']\s*\))/i;
const BROWSER_RUNTIME_PATH = /^(?:apps\/ui\/src|packages\/[^/]+\/src)\/.+\.[cm]?[jt]sx?$/;
const FORBIDDEN_BROWSER_RUNTIME =
  /\b(?:import\s+(?:[^;]*?\s+from\s+)?["'](?:electron|node:[^"']+|child_process|fs|os|path|net|tls|worker_threads)["']|(?:import|require)\s*\(\s*["'](?:electron|node:[^"']+|child_process|fs|os|path|net|tls|worker_threads)["']\s*\))/i;

/** 只有来源历史/许可证文件可记录退役来源，业务源码与普通文档仍需清理。 */
export const PROVENANCE_ALLOWLIST = Object.freeze([
  { path: "THIRD_PARTY_NOTICES.md", rules: Object.values(SOURCE_RULE_IDS) },
  { path: "docs/source-history.md", rules: Object.values(SOURCE_RULE_IDS) },
  { prefix: "third-party/zcode/", rules: Object.values(SOURCE_RULE_IDS) },
]);

function normalizePath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//, "");
}

function isAllowlisted(relativePath, ruleId) {
  return PROVENANCE_ALLOWLIST.some((entry) => {
    const pathMatches = entry.path === relativePath || (entry.prefix && relativePath.startsWith(entry.prefix));
    return pathMatches && entry.rules.includes(ruleId);
  });
}

function isScannablePath(filePath) {
  const normalized = normalizePath(filePath);
  const name = normalized.split("/").at(-1)?.toLowerCase() ?? "";
  return SCANNABLE_EXTENSIONS.has(extname(name).toLowerCase()) || SCANNABLE_FILENAMES.has(name);
}

export { isScannablePath };

function makeFinding(file, rule, line, column, excerpt, message) {
  return { file, rule, line, column, excerpt, message };
}

/** 扫描一段文本，供脚本和 focused tests 共用。 */
export function scanText(file, content) {
  const relativePath = normalizePath(file);
  const findings = [];
  for (const rule of RULES) {
    if (isAllowlisted(relativePath, rule.id)) continue;
    const matcher = boundedPattern(rule.value);
    const lines = content.split(/\r?\n/);
    lines.forEach((lineText, index) => {
      const match = matcher.exec(lineText);
      if (!match) return;
      findings.push(
        makeFinding(relativePath, rule.id, index + 1, match.index + 1, lineText.trim().slice(0, 240), rule.message),
      );
    });
  }
  if (
    /^(?:packages|src|apps\/ui)\//.test(relativePath) &&
    WORKFLOW_SOURCE_PATH.test(relativePath) &&
    FORBIDDEN_WORKFLOW_RUNTIME.test(content)
  ) {
    const lines = content.split(/\r?\n/);
    lines.forEach((lineText, index) => {
      if (!FORBIDDEN_WORKFLOW_RUNTIME.test(lineText)) return;
      findings.push(makeFinding(
        relativePath,
        SOURCE_RULE_IDS.workflowRuntime,
        index + 1,
        1,
        lineText.trim().slice(0, 240),
        "工作流源码不得依赖 Node/Electron/JavaScript runtime；执行边界必须落在 Rust JSON WorkflowDefinitionV1。",
      ));
    });
  } else if (
    BROWSER_RUNTIME_PATH.test(relativePath) &&
    !/(?:\.d\.ts$|\.(?:test|spec)\.[cm]?[jt]sx?$|\/(?:__tests__|__fixtures__)\/)/.test(relativePath) &&
    FORBIDDEN_BROWSER_RUNTIME.test(content)
  ) {
    content.split(/\r?\n/).forEach((lineText, index) => {
      const match = FORBIDDEN_BROWSER_RUNTIME.exec(lineText);
      if (!match) return;
      findings.push(makeFinding(
        relativePath, SOURCE_RULE_IDS.browserRuntime, index + 1, match.index + 1,
        lineText.trim().slice(0, 240),
        "浏览器源码只能保留协议与展示实现；Node/Electron 宿主必须由 Rust 能力替代。",
      ));
    });
  }
  return findings;
}

/** 文件路径本身也属于来源边界，避免残留旧目录绕过文本扫描。 */
export function scanPath(file) {
  const relativePath = normalizePath(file);
  const findings = [];
  const staleSegment = boundedPattern(FORBIDDEN_SOURCE_TEXT.legacyTheme);
  if (!isAllowlisted(relativePath, SOURCE_RULE_IDS.staleSourcePath) && staleSegment.test(relativePath)) {
    findings.push(makeFinding(
      relativePath,
      SOURCE_RULE_IDS.staleSourcePath,
      1,
      1,
      relativePath,
      "路径仍包含已退役来源目录；删除或迁移到明确的来源清单路径。",
    ));
  }
  return findings;
}

function gitFiles(root) {
  try {
    const output = execFileSync("git", ["-C", root, "ls-files", "--cached", "--others", "--exclude-standard"], {
      encoding: "utf8",
      maxBuffer: GIT_FILE_LIST_MAX_BYTES,
    });
    return output.split(/\r?\n/).filter(Boolean);
  } catch {
    return [];
  }
}

function isInSourceScope(relativePath) {
  return SOURCE_SCAN_SCOPE.some((prefix) => relativePath === prefix || relativePath.startsWith(prefix));
}

/** 扫描当前 Git 清单，包含未跟踪文件但排除 .gitignore 内容。 */
export function scanRepository(root = REPO_ROOT) {
  const findings = [];
  for (const file of gitFiles(root)) {
    const relativePath = normalizePath(file);
    if (!isInSourceScope(relativePath)) continue;
    if (!isScannablePath(relativePath)) continue;
    findings.push(...scanPath(relativePath));
    const absolutePath = resolve(root, file);
    if (!existsSync(absolutePath) || !statSync(absolutePath).isFile()) continue;
    let content;
    try {
      content = readFileSync(absolutePath, "utf8");
    } catch {
      continue;
    }
    findings.push(...scanText(relativePath, content));
    if (findings.length >= MAX_REPORTED_FINDINGS) return findings.slice(0, MAX_REPORTED_FINDINGS);
  }
  return findings;
}

export function formatFinding(finding) {
  return `${finding.file}:${finding.line}:${finding.column} ${finding.rule} ${finding.message} (${finding.excerpt})`;
}

export async function main() {
  const findings = scanRepository(REPO_ROOT);
  if (findings.length === 0) {
    console.log("Clean-room source gate passed: no retired source references or stale source paths.");
    return 0;
  }
  for (const finding of findings) console.error(formatFinding(finding));
  console.error(`Clean-room source gate failed: ${findings.length} finding(s).`);
  return 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  process.exitCode = await main();
}
