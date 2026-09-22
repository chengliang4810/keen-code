import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, "..");
const SOURCE_ROOT = path.join(REPO_ROOT, "src");

const CSS_TOKEN_ALLOWLIST = new Set([
  "src/styles/tokens.css",
  "src/styles/skins.css",
  "src/styles/code-preview.css",
  "src/styles/tailwind.css",
  "src/styles/ui-governance.css",
  "src/components/lobe-chat/lobe-chat.css",
]);

const CSS_DIRECTORY_ALLOWLIST = ["src/styles/harness/"];

const INLINE_STYLE_ALLOWLIST = [
  "src/components/icons.tsx",
  "src/components/ImageLightbox.tsx",
  "src/components/ImageUi.tsx",
  "src/components/ResourceViewer.tsx",
  "src/components/TerminalPanel.tsx",
  "src/components/VideoUi.tsx",
  "src/components/VirtualList.tsx",
  "src/components/WallpaperFocusEditor.tsx",
  "src/components/lobe-chat/ConversationThread.tsx",
  "src/features/app/MainStage.tsx",
  "src/features/app/ResourceAside.tsx",
  "src/features/app/Sidebar.tsx",
];

const RAW_COLOR_ALLOWLIST = new Set([
  "src/components/ImageLightbox.tsx",
  "src/components/TerminalPanel.tsx",
]);

const COLOR_PATTERN = /#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/;

/** 扫描结果使用稳定的规则 ID，便于 CI 和设计审查引用。 */
export function inspectSource(relativePath, content) {
  const violations = [];
  const normalizedPath = relativePath.replaceAll("\\", "/");
  const extension = path.extname(normalizedPath).toLowerCase();
  const isTest = /(?:\.test|\.spec)\.[^.]+$/.test(normalizedPath);

  if ((extension === ".tsx" || extension === ".jsx") && !isTest) {
    inspectNativeControls(normalizedPath, content, violations);
    inspectInlineStyles(normalizedPath, content, violations);
    inspectAppicaSizes(normalizedPath, content, violations);
    if (!RAW_COLOR_ALLOWLIST.has(normalizedPath)) {
      inspectRawColors(normalizedPath, content, violations, "DSG002");
    }
  }

  if (extension === ".css" && !isCssAllowlisted(normalizedPath)) {
    const withoutComments = content.replace(/\/\*[\s\S]*?\*\//g, "");
    inspectRawColors(normalizedPath, withoutComments, violations, "DSG004");
  }

  return violations;
}

const APPICA_SIZE_RULES = new Map([
  ["Autocomplete", { prop: "size" }],
  ["Avatar", { prop: "size" }],
  ["Badge", { prop: "size" }],
  ["Button", { prop: "size" }],
  ["ButtonGroup", { prop: "size" }],
  ["Calendar", { prop: "size" }],
  ["Chip", { prop: "size" }],
  ["ChipGroup", { prop: "size" }],
  ["ColorPicker", { prop: "size" }],
  ["ColorPickerEyeDropper", { prop: "size" }],
  ["ColorSwatch", { prop: "size" }],
  ["ColorSwatchPicker", { prop: "size" }],
  ["Combobox", { prop: "size" }],
  ["ContextMenu", { prop: "size" }],
  ["CopyButton", { prop: "size" }],
  ["DateField", { prop: "size" }],
  ["DatePicker", { prop: "size" }],
  ["DropdownMenu", { prop: "size" }],
  ["Input", { prop: "inputSize" }],
  ["Kbd", { prop: "size" }],
  ["KbdGroup", { prop: "size" }],
  ["Menubar", { prop: "size" }],
  ["Navigation", { prop: "size" }],
  ["NavigationLink", { prop: "size" }],
  ["NavigationMenu", { prop: "size" }],
  ["NumberField", { prop: "size" }],
  ["OTPField", { prop: "size" }],
  ["Pagination", { prop: "size" }],
  ["Select", { prop: "size" }],
  ["Switch", { prop: "size" }],
  ["Table", { prop: "size" }],
  ["Tabs", { prop: "size" }],
  ["TabsList", { prop: "size" }],
  ["TabsTrigger", { prop: "size" }],
  ["Textarea", { prop: "inputSize" }],
  ["Thumbnail", { prop: "size" }],
  ["TimeField", { prop: "size" }],
]);

/** 所有 Appica 尺寸属性必须显式使用 md，避免同类控件出现不一致的视觉规格。 */
function inspectAppicaSizes(relativePath, content, violations) {
  const sourceFile = ts.createSourceFile(
    relativePath,
    content,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const appicaComponents = new Map();

  const collectImports = (node) => {
    if (
      ts.isImportDeclaration(node) &&
      ts.isStringLiteral(node.moduleSpecifier) &&
      node.moduleSpecifier.text.startsWith("@appica/ui-react/")
    ) {
      const bindings = node.importClause?.namedBindings;
      if (bindings && ts.isNamedImports(bindings)) {
        for (const element of bindings.elements) {
          const importedName = element.propertyName?.text ?? element.name.text;
          const rule = APPICA_SIZE_RULES.get(importedName);
          if (rule) appicaComponents.set(element.name.text, rule);
        }
      }
    }
    ts.forEachChild(node, collectImports);
  };
  collectImports(sourceFile);

  const inspectNode = (node) => {
    if (ts.isJsxSelfClosingElement(node) || ts.isJsxElement(node)) {
      const opening = ts.isJsxSelfClosingElement(node) ? node : node.openingElement;
      const tagName = opening.tagName.getText(sourceFile);
      const sizeRule = appicaComponents.get(tagName);
      if (sizeRule) {
        for (const attribute of opening.attributes.properties) {
          if (!ts.isJsxAttribute(attribute)) continue;
          const attributeName = attribute.name.text;
          if (attributeName !== sizeRule.prop) continue;

          const value = attribute.initializer;
          const isMd =
            value &&
            ((ts.isStringLiteral(value) && value.text === "md") ||
              (ts.isJsxExpression(value) &&
                value.expression &&
                ts.isStringLiteral(value.expression) &&
                value.expression.text === "md"));
          if (isMd) continue;

          const line = sourceFile.getLineAndCharacterOfPosition(attribute.getStart(sourceFile)).line + 1;
          violations.push({
            rule: "DSG005",
            file: relativePath,
            line,
            message: `Appica <${tagName}> 的 ${attributeName} 必须使用 md。`,
          });
        }

        const hasSizeAttribute = [...opening.attributes.properties].some(
          (attribute) => ts.isJsxAttribute(attribute) && attribute.name.text === sizeRule.prop,
        );
        if (!hasSizeAttribute) {
          const line = sourceFile.getLineAndCharacterOfPosition(opening.getStart(sourceFile)).line + 1;
          violations.push({
            rule: "DSG005",
            file: relativePath,
            line,
            message: `Appica <${tagName}> 必须显式传入 ${sizeRule.prop}="md"；不得依赖默认尺寸。`,
          });
        }
      }
    }
    ts.forEachChild(node, inspectNode);
  };
  inspectNode(sourceFile);
}

function inspectNativeControls(relativePath, content, violations) {
  // JSX 原生控件标签必须是小写；大写的 Button/Input 是 React 组件。
  const nativeControl = /<(button|select|dialog)\b[\s\S]*?>/g;
  for (const match of content.matchAll(nativeControl)) {
    const tag = match[0];
    const line = lineNumberAt(content, match.index ?? 0);
    violations.push({
      rule: "DSG001",
      file: relativePath,
      line,
      message: `业务 TSX 使用原生可见 <${match[1].toLowerCase()}>，请复用 src/components/ui 或 @appica/ui-react。`,
    });
    if (tag.includes("data-design-system-allow")) violations.at(-1).allowed = true;
  }

  const inputs = /<input\b[\s\S]*?>/g;
  for (const match of content.matchAll(inputs)) {
    const tag = match[0];
    if (/\bhidden\b|type\s*=\s*["']hidden["']/i.test(tag)) continue;
    violations.push({
      rule: "DSG001",
      file: relativePath,
      line: lineNumberAt(content, match.index ?? 0),
      message: "可见原生 <input> 只允许作为隐藏文件选择宿主；请复用 @appica/ui-react/input。",
    });
  }
}

function inspectInlineStyles(relativePath, content, violations) {
  const inlineStyle = /style=\{\{([\s\S]*?)\}\s*(?:as\s+CSSProperties)?\}/g;
  for (const match of content.matchAll(inlineStyle)) {
    const body = match[1] ?? "";
    const propertyNames = [...body.matchAll(/(?:^|,)\s*(?:['"]([^'"]+)['"]|([A-Za-z][\w-]*))\s*:/g)]
      .map((item) => item[1] ?? item[2] ?? "");
    const onlyCustomProperties = propertyNames.length > 0 && propertyNames.every((name) => name.startsWith("--"));
    if (onlyCustomProperties || INLINE_STYLE_ALLOWLIST.includes(relativePath)) continue;
    violations.push({
      rule: "DSG003",
      file: relativePath,
      line: lineNumberAt(content, match.index ?? 0),
      message: "业务 inline style 只能承载动态 CSS custom property；固定视觉值请下沉到 CSS/语义 token。",
    });
  }
}

function inspectRawColors(relativePath, content, violations, rule) {
  for (const [index, lineText] of content.split(/\r?\n/).entries()) {
    if (!COLOR_PATTERN.test(lineText)) continue;
    violations.push({
      rule,
      file: relativePath,
      line: index + 1,
      message: rule === "DSG004"
        ? "CSS 业务样式不得直接写主题颜色，请引用语义 token；令牌、皮肤和第三方样式使用显式 allowlist。"
        : "业务 TSX 不得直接写主题颜色，请使用语义 token 或受控 CSS custom property。",
    });
  }
}

function isCssAllowlisted(relativePath) {
  return CSS_TOKEN_ALLOWLIST.has(relativePath) || CSS_DIRECTORY_ALLOWLIST.some((prefix) => relativePath.startsWith(prefix));
}

function lineNumberAt(content, offset) {
  return content.slice(0, offset).split(/\r?\n/).length;
}

async function listFiles(directory) {
  const entries = await fs.readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if (["node_modules", "dist", "out"].includes(entry.name)) continue;
      files.push(...await listFiles(absolute));
    } else {
      files.push(absolute);
    }
  }
  return files;
}

export async function scanDesignSystem(sourceRoot = SOURCE_ROOT) {
  const violations = [];
  for (const file of await listFiles(sourceRoot)) {
    const extension = path.extname(file).toLowerCase();
    if (![".css", ".tsx", ".jsx"].includes(extension)) continue;
    const relative = path.relative(REPO_ROOT, file).replaceAll("\\", "/");
    const content = await fs.readFile(file, "utf8");
    violations.push(...inspectSource(relative, content));
  }
  return violations.filter((violation) => !violation.allowed);
}

export async function main() {
  const violations = await scanDesignSystem();
  if (violations.length === 0) {
    console.log("Design-system gate passed: no unapproved native controls, Appica sizes, raw colors, or inline visual styles.");
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
