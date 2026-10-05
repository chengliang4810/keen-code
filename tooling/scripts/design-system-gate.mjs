import fs from "node:fs/promises";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(SCRIPT_DIR, "../..");

/** 迁移期间允许扫描的 UI 根；实际源树收敛后由 packages/ui/src 作为唯一根。 */
export const SOURCE_ROOTS = ["packages/ui/src", "src", "apps/ui/src"];
export const SOURCE_ROOT = path.join(REPO_ROOT, "packages/ui/src");
export const DESIGN_BASELINE_MANIFEST = path.join(
  REPO_ROOT,
  "third-party/zcode/design-baseline.json",
);

/** 语义 token 入口路径供样式工具和来源审查引用；设计门禁仍检查新增内容。 */
export const CSS_TOKEN_ALLOWLIST = new Set([
  "packages/ui/src/styles.css",
  "src/styles.css",
  "apps/ui/src/index.css",
]);

/**
 * 来源组件保留原 DOM，但不按目录跳过门禁。固定 ZCode 源码中的合法原生控件、
 * inline 布局、颜色和字号由 design-baseline.json 按文件 SHA256/行特征放行；
 * 同一目录下新增的 KeenCode 代码仍然完整检查。
 */
export const SOURCE_RULE_ALLOWLIST = Object.freeze([]);

/** 内容渲染可以使用独立字号；外围控件仍须使用 text-ui-*。 */
const TYPOGRAPHY_CONTENT_PATHS = [
  "/code",
  "/diff",
  "/terminal",
  "code-",
  "-code.",
  "diff-",
  "-diff.",
  "terminal-",
  "-terminal.",
];

const COLOR_PATTERN = /#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/;
const BUILTIN_TEXT_SCALE_PATTERN = /\btext-(?:2?xs|sm|base|lg|xl|2xl|3xl|4xl|5xl|6xl|7xl|8xl|9xl)\b/;
const ARBITRARY_TEXT_SCALE_PATTERN = /\btext-\[[^\]]+\]/;

export const DESIGN_RULE_IDS = Object.freeze({
  nativeControl: "DSG001",
  rawColor: "DSG002",
  inlineStyle: "DSG003",
  cssColor: "DSG004",
  typographyScale: "DSG006",
});

function normalizePath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//, "");
}

function sha256(value) {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

function lineFeatureHash(lines, lineNumber) {
  const index = Math.max(0, lineNumber - 1);
  const context = lines.slice(Math.max(0, index - 1), index + 2).join("\n");
  return sha256(context);
}

function annotateLineFeatures(violations, content) {
  const lines = content.split(/\r?\n/);
  return violations.map((violation) => ({
    ...violation,
    lineHash: sha256(lines[(violation.line ?? 1) - 1] ?? ""),
    featureHash: lineFeatureHash(lines, violation.line ?? 1),
  }));
}

let designBaseline;
function loadDesignBaseline() {
  if (designBaseline !== undefined) return designBaseline;
  try {
    designBaseline = JSON.parse(readFileSync(DESIGN_BASELINE_MANIFEST, "utf8"));
  } catch {
    designBaseline = null;
  }
  return designBaseline;
}

function isBaselineViolation(relativePath, content, violation) {
  const manifest = loadDesignBaseline();
  const entry = manifest?.files?.[relativePath];
  if (!entry || !entry.rules?.includes(violation.rule)) return false;
  if (entry.sha256 === sha256(content)) return true;
  return entry.lineFeatures?.[violation.rule]?.some(
    (feature) => feature.featureHash === violation.featureHash && feature.lineHash === violation.lineHash,
  ) ?? false;
}

function lineNumberAt(content, offset) {
  return content.slice(0, offset).split(/\r?\n/).length;
}

function isTestPath(relativePath) {
  return /(?:\.test|\.spec)\.[^.]+$/.test(relativePath);
}

function isContentTypographyPath(relativePath) {
  const lower = `/${relativePath.toLowerCase()}`;
  return TYPOGRAPHY_CONTENT_PATHS.some((fragment) => lower.includes(fragment));
}

function isRuleAllowed(relativePath, rule) {
  return SOURCE_RULE_ALLOWLIST.some(
    (entry) => relativePath.startsWith(entry.prefix) && entry.rules.includes(rule),
  );
}

function pushViolation(violations, rule, file, line, message) {
  violations.push({ rule, file, line, message });
}

/** 扫描单个源文件；输出稳定规则 ID，便于 CI 和验收矩阵引用。 */
export function inspectSource(relativePath, content) {
  const normalizedPath = normalizePath(relativePath);
  const extension = path.extname(normalizedPath).toLowerCase();
  const violations = [];
  const isTest = isTestPath(normalizedPath);

  if ((extension === ".tsx" || extension === ".jsx") && !isTest) {
    if (!isRuleAllowed(normalizedPath, DESIGN_RULE_IDS.nativeControl)) {
      inspectNativeControls(normalizedPath, content, violations);
    }
    if (!isRuleAllowed(normalizedPath, DESIGN_RULE_IDS.inlineStyle)) {
      inspectInlineStyles(normalizedPath, content, violations);
    }
    if (!isContentTypographyPath(normalizedPath)) {
      inspectTypography(normalizedPath, content, violations);
    }
    inspectRawColors(normalizedPath, content, violations, DESIGN_RULE_IDS.rawColor);
  }

  if (extension === ".css") {
    const withoutComments = content.replace(/\/\*[\s\S]*?\*\//g, "");
    inspectRawColors(normalizedPath, withoutComments, violations, DESIGN_RULE_IDS.cssColor);
    inspectCssTypography(normalizedPath, withoutComments, violations);
  }

  return annotateLineFeatures(violations, content);
}

function inspectNativeControls(relativePath, content, violations) {
  // JSX 大写标签是组件引用；大小写敏感才能区分 Button 与原生 button。
  const visibleControl = /<(button|select|dialog)\b[\s\S]*?>/g;
  for (const match of content.matchAll(visibleControl)) {
    const tag = match[0];
    if (tag.includes("data-design-system-allow")) continue;
    pushViolation(
      violations,
      DESIGN_RULE_IDS.nativeControl,
      relativePath,
      lineNumberAt(content, match.index ?? 0),
      `业务 TSX 使用可见原生 <${match[1].toLowerCase()}>；请复用 packages/ui/src/components/ui 中的控件。`,
    );
  }

  const inputs = /<input\b[\s\S]*?>/g;
  for (const match of content.matchAll(inputs)) {
    const tag = match[0];
    if (/\bhidden\b|type\s*=\s*["']hidden["']/i.test(tag)) continue;
    pushViolation(
      violations,
      DESIGN_RULE_IDS.nativeControl,
      relativePath,
      lineNumberAt(content, match.index ?? 0),
      "可见原生 <input> 只允许作为隐藏文件选择宿主；请复用 UI 控件。",
    );
  }
}

function inspectInlineStyles(relativePath, content, violations) {
  const inlineStyle = /style=\{\{([\s\S]*?)\}\s*(?:as\s+CSSProperties)?\}/g;
  for (const match of content.matchAll(inlineStyle)) {
    const body = match[1] ?? "";
    const propertyNames = [
      ...body.matchAll(/(?:^|,)\s*(?:['"]([^'"]+)['"]|([A-Za-z][\w-]*))\s*:/g),
    ].map((item) => item[1] ?? item[2] ?? "");
    const onlyCustomProperties = propertyNames.length > 0 && propertyNames.every((name) => name.startsWith("--"));
    if (onlyCustomProperties) continue;
    pushViolation(
      violations,
      DESIGN_RULE_IDS.inlineStyle,
      relativePath,
      lineNumberAt(content, match.index ?? 0),
      "业务 inline style 只能承载动态 CSS custom property；固定视觉值请下沉到语义 CSS token。",
    );
  }
}

function inspectRawColors(relativePath, content, violations, rule) {
  for (const [index, lineText] of content.split(/\r?\n/).entries()) {
    if (!COLOR_PATTERN.test(lineText)) continue;
    pushViolation(
      violations,
      rule,
      relativePath,
      index + 1,
      rule === DESIGN_RULE_IDS.cssColor
        ? "业务 CSS 不得直接写主题颜色，请引用 --color-* 语义 token；token 入口使用显式 allowlist。"
        : "业务 TSX 不得直接写主题颜色，请使用 --color-* 语义 token 或受控 CSS custom property。",
    );
  }
}

function inspectTypography(relativePath, content, violations) {
  for (const [index, lineText] of content.split(/\r?\n/).entries()) {
    if (BUILTIN_TEXT_SCALE_PATTERN.test(lineText) || ARBITRARY_TEXT_SCALE_PATTERN.test(lineText)) {
      pushViolation(
        violations,
        DESIGN_RULE_IDS.typographyScale,
        relativePath,
        index + 1,
        "应用界面必须使用 text-ui-*；代码、Diff、终端内容才可使用独立数字字号。",
      );
    }
  }
}

function inspectCssTypography(relativePath, content, violations) {
  for (const [index, lineText] of content.split(/\r?\n/).entries()) {
    if (!/\bfont-size\s*:/.test(lineText)) continue;
    if (/var\(--(?:text-ui|ui-font-size|diffs-|code-)/.test(lineText)) continue;
    pushViolation(
      violations,
      DESIGN_RULE_IDS.typographyScale,
      relativePath,
      index + 1,
      "业务 CSS 字号必须引用 text-ui-* 或受控内容字号 token。",
    );
  }
}

async function listFiles(directory) {
  const entries = await fs.readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if (["node_modules", "dist", "out", ".git"].includes(entry.name)) continue;
      files.push(...await listFiles(absolute));
    } else {
      files.push(absolute);
    }
  }
  return files;
}

/** 扫描一个或多个 UI 根；不存在的迁移暂存根会被跳过。 */
export async function scanDesignSystem(sourceRoots = SOURCE_ROOTS) {
  const roots = Array.isArray(sourceRoots) ? sourceRoots : [sourceRoots];
  const violations = [];
  const visited = new Set();
  for (const root of roots) {
    const absoluteRoot = path.isAbsolute(root) ? root : path.join(REPO_ROOT, root);
    if (!(await fs.stat(absoluteRoot).catch(() => null))) continue;
    for (const file of await listFiles(absoluteRoot)) {
      if (visited.has(file)) continue;
      visited.add(file);
      const extension = path.extname(file).toLowerCase();
      if (![".css", ".tsx", ".jsx"].includes(extension)) continue;
      const relative = normalizePath(path.relative(REPO_ROOT, file));
      const content = await fs.readFile(file, "utf8");
      violations.push(
        ...inspectSource(relative, content).filter(
          (violation) => !isBaselineViolation(relative, content, violation),
        ),
      );
    }
  }
  return violations;
}

export async function main() {
  const violations = await scanDesignSystem();
  if (violations.length === 0) {
    console.log("Design-system gate passed: text-ui scale, semantic tokens, source DOM and inline-style rules are satisfied.");
    return 0;
  }
  for (const violation of violations) {
    console.error(`${violation.file}:${violation.line} ${violation.rule} ${violation.message}`);
  }
  console.error(`Design-system gate failed: ${violations.length} violation(s).`);
  return 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.exitCode = await main();
}
