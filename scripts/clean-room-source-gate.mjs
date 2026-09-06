import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, extname, isAbsolute, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** 需要扫描的源码、文档与配置扩展名；不读取图片、字体和其他二进制资源。 */
const SCANNABLE_EXTENSIONS = new Set([
  ".bat",
  ".c",
  ".cc",
  ".cfg",
  ".cjs",
  ".cmd",
  ".conf",
  ".cpp",
  ".cs",
  ".css",
  ".go",
  ".h",
  ".hpp",
  ".html",
  ".ini",
  ".java",
  ".js",
  ".json",
  ".jsonc",
  ".jsx",
  ".lock",
  ".md",
  ".mdx",
  ".mjs",
  ".plist",
  ".properties",
  ".ps1",
  ".py",
  ".rs",
  ".scss",
  ".sh",
  ".sql",
  ".toml",
  ".ts",
  ".tsx",
  ".txt",
  ".vue",
  ".xml",
  ".yaml",
  ".yml",
]);

/** 没有扩展名但属于版本化源码或配置的精确文件名。 */
const SCANNABLE_FILENAMES = new Set([
  ".gitattributes",
  ".gitignore",
  ".npmrc",
  "cargo.lock",
  "dockerfile",
  "license",
  "makefile",
]);

/** 单次失败最多输出的命中数，避免 CI 日志被重复问题淹没。 */
const MAX_REPORTED_FINDINGS = 100;

/** Git 文件清单允许的最大输出，覆盖大型仓库且避免子进程无界缓存。 */
const GIT_FILE_LIST_MAX_BYTES = 16 * 1024 * 1024;

/** 从不可读的代码点构造扫描词，避免门禁源码本身携带被禁止的明文。 */
function textFromCodePoints(codePoints) {
  return String.fromCodePoint(...codePoints);
}

/** 对运行时扫描词执行正则转义，防止词中的标点改变检测语义。 */
function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 构造具备字母数字边界的大小写不敏感扫描规则。 */
function boundedPattern(value) {
  return new RegExp(
    `(?<![A-Za-z0-9])${escapeRegExp(value)}(?![A-Za-z0-9])`,
    "i",
  );
}

/** 构造允许指定分隔符的复合来源名称规则。 */
function compoundPattern(first, separator, second) {
  return new RegExp(
    `(?<![A-Za-z0-9])${escapeRegExp(first)}${separator}${escapeRegExp(second)}(?![A-Za-z0-9])`,
    "i",
  );
}

/** 门禁需要识别的来源词与测试词；字段名使用中性内部标识。 */
export const FORBIDDEN_SOURCE_TEXT = Object.freeze({
  legacyRuntime: textFromCodePoints([0x50, 0x65, 0x72, 0x69]),
  researchProject: textFromCodePoints([
    0x6f,
    0x70,
    0x65,
    0x6e,
    0x63,
    0x6c,
    0x61,
    0x75,
    0x64,
    0x65,
  ]),
  productWord: textFromCodePoints([
    0x63,
    0x6c,
    0x61,
    0x75,
    0x64,
    0x65,
  ]),
  productAction: textFromCodePoints([0x63, 0x6f, 0x64, 0x65]),
  /** 外部插件集合标识中的复数名词。 */
  pluginCollection: textFromCodePoints([
    0x70,
    0x6c,
    0x75,
    0x67,
    0x69,
    0x6e,
    0x73,
  ]),
  /** 外部组织标识中的复数名称。 */
  externalOrganization: textFromCodePoints([
    0x61,
    0x6e,
    0x74,
    0x68,
    0x72,
    0x6f,
    0x70,
    0x69,
    0x63,
    0x73,
  ]),
  /** 外部官方仓库标识中的限定词。 */
  officialQualifier: textFromCodePoints([
    0x6f,
    0x66,
    0x66,
    0x69,
    0x63,
    0x69,
    0x61,
    0x6c,
  ]),
  /** 已明确排除的编辑器来源标识。 */
  prohibitedEditor: textFromCodePoints([0x7a, 0x63, 0x6f, 0x64, 0x65]),
  /** 可以单独合法引用的开源编码代理名称。 */
  openSourceAgent: textFromCodePoints([0x63, 0x6f, 0x64, 0x65, 0x78]),
  /** 已明确排除的剪贴板组件名称片段。 */
  clipboardComponent: textFromCodePoints([
    0x63,
    0x6c,
    0x69,
    0x70,
    0x62,
    0x6f,
    0x61,
    0x72,
    0x64,
  ]),
  /** 表示外观模仿的描述词。 */
  imitationStyle: textFromCodePoints([0x73, 0x74, 0x79, 0x6c, 0x65]),
  /** 表示相似实现的描述词。 */
  imitationLike: textFromCodePoints([0x6c, 0x69, 0x6b, 0x65]),
  /** 表示受外部实现启发的描述词。 */
  imitationInspired: textFromCodePoints([
    0x69,
    0x6e,
    0x73,
    0x70,
    0x69,
    0x72,
    0x65,
    0x64,
  ]),
  pathReference: textFromCodePoints([0x72, 0x65, 0x66, 0x73]),
  pathSpike: textFromCodePoints([
    0x73,
    0x70,
    0x69,
    0x6b,
    0x65,
    0x2d,
    0x70,
    0x72,
    0x6f,
    0x6a,
  ]),
  uiBetter: textFromCodePoints([
    0x62,
    0x65,
    0x74,
    0x74,
    0x65,
    0x72,
  ]),
  uiName: textFromCodePoints([0x75, 0x69]),
  uiCursor: textFromCodePoints([
    0x63,
    0x75,
    0x72,
    0x73,
    0x6f,
    0x72,
  ]),
  uiStyle: textFromCodePoints([
    0x73,
    0x74,
    0x79,
    0x6c,
    0x65,
  ]),
  uiChat: textFromCodePoints([
    0x63,
    0x68,
    0x61,
    0x74,
    0x67,
    0x70,
    0x74,
  ]),
});

/** 对外稳定的规则标识；其值在运行时保持原有报告兼容性。 */
export const SOURCE_RULE_IDS = Object.freeze({
  legacyRuntime: `source-${FORBIDDEN_SOURCE_TEXT.legacyRuntime.toLowerCase()}`,
  researchProject: `source-${FORBIDDEN_SOURCE_TEXT.researchProject}`,
  externalProduct: [
    "source",
    FORBIDDEN_SOURCE_TEXT.productWord,
    FORBIDDEN_SOURCE_TEXT.productAction,
  ].join("-"),
  proprietaryPluginDirectory: "source-proprietary-plugin-directory",
  proprietaryPluginRepository: "source-proprietary-plugin-repository",
  prohibitedEditor: "source-proprietary-editor",
  externalClipboard: "source-external-agent-clipboard",
  openAgentImitation: "source-open-agent-imitation",
  localReferencePath: "local-reference-path",
  externalUiBetter: [
    "external",
    FORBIDDEN_SOURCE_TEXT.uiName,
    `${FORBIDDEN_SOURCE_TEXT.uiBetter}-${FORBIDDEN_SOURCE_TEXT.uiName}`,
  ].join("-"),
  externalUiCursor: [
    "external",
    FORBIDDEN_SOURCE_TEXT.uiName,
    `${FORBIDDEN_SOURCE_TEXT.uiCursor}-${FORBIDDEN_SOURCE_TEXT.uiStyle}`,
  ].join("-"),
  externalUiReference: "external-ui-reference-copy",
});

/** 禁止来源规则；边界与分隔符语义与历史门禁保持一致。 */
const FORBIDDEN_RULES = Object.freeze([
  {
    id: SOURCE_RULE_IDS.legacyRuntime,
    description: "旧运行时或来源名称",
    pattern: boundedPattern(FORBIDDEN_SOURCE_TEXT.legacyRuntime),
  },
  {
    id: SOURCE_RULE_IDS.researchProject,
    description: "研究来源项目名称",
    pattern: compoundPattern(
      FORBIDDEN_SOURCE_TEXT.researchProject.slice(0, 4),
      "[-_ ]?",
      FORBIDDEN_SOURCE_TEXT.productWord,
    ),
  },
  {
    id: SOURCE_RULE_IDS.externalProduct,
    description: "外部产品名称",
    pattern: compoundPattern(
      FORBIDDEN_SOURCE_TEXT.productWord,
      "[-_ ]+",
      FORBIDDEN_SOURCE_TEXT.productAction,
    ),
  },
  {
    id: SOURCE_RULE_IDS.proprietaryPluginDirectory,
    description: "外部专有插件目录标识",
    pattern: compoundPattern(
      FORBIDDEN_SOURCE_TEXT.productWord,
      "_",
      FORBIDDEN_SOURCE_TEXT.pluginCollection,
    ),
  },
  {
    id: SOURCE_RULE_IDS.proprietaryPluginRepository,
    description: "外部专有插件仓库标识",
    pattern: new RegExp(
      `(?<![A-Za-z0-9])(?:${escapeRegExp(`${FORBIDDEN_SOURCE_TEXT.externalOrganization}/${FORBIDDEN_SOURCE_TEXT.pluginCollection}-${FORBIDDEN_SOURCE_TEXT.officialQualifier}`)}|${escapeRegExp(`${FORBIDDEN_SOURCE_TEXT.productWord}-${FORBIDDEN_SOURCE_TEXT.pluginCollection}-${FORBIDDEN_SOURCE_TEXT.officialQualifier}`)})(?![A-Za-z0-9])`,
      "i",
    ),
  },
  {
    id: SOURCE_RULE_IDS.prohibitedEditor,
    description: "已排除的编辑器来源标识",
    pattern: boundedPattern(FORBIDDEN_SOURCE_TEXT.prohibitedEditor),
  },
  {
    id: SOURCE_RULE_IDS.externalClipboard,
    description: "外部代理剪贴板组件标识",
    pattern: compoundPattern(
      FORBIDDEN_SOURCE_TEXT.openSourceAgent,
      "-",
      FORBIDDEN_SOURCE_TEXT.clipboardComponent,
    ),
  },
  {
    id: SOURCE_RULE_IDS.openAgentImitation,
    description: "外部开源代理的模仿性描述",
    pattern: new RegExp(
      `(?<![A-Za-z0-9])${escapeRegExp(FORBIDDEN_SOURCE_TEXT.openSourceAgent)}[-_ ]+(?:${[
        FORBIDDEN_SOURCE_TEXT.imitationStyle,
        FORBIDDEN_SOURCE_TEXT.imitationLike,
        FORBIDDEN_SOURCE_TEXT.imitationInspired,
      ]
        .map(escapeRegExp)
        .join("|")})(?![A-Za-z0-9])`,
      "i",
    ),
  },
  {
    id: SOURCE_RULE_IDS.localReferencePath,
    description: "研究用本地参考目录",
    pattern: new RegExp(
      `(?<![A-Za-z0-9.])\\.(?:${escapeRegExp(FORBIDDEN_SOURCE_TEXT.pathReference)}|${escapeRegExp(FORBIDDEN_SOURCE_TEXT.pathSpike)})(?![A-Za-z0-9])`,
      "i",
    ),
  },
  {
    id: SOURCE_RULE_IDS.externalUiBetter,
    description: "外部 UI 项目参考名称",
    pattern: compoundPattern(
      FORBIDDEN_SOURCE_TEXT.uiBetter,
      "[-_ ]",
      FORBIDDEN_SOURCE_TEXT.uiName,
    ),
  },
  {
    id: SOURCE_RULE_IDS.externalUiCursor,
    description: "外部产品 UI 风格参考",
    pattern: new RegExp(
      `(?<![A-Za-z0-9])(?:${escapeRegExp(FORBIDDEN_SOURCE_TEXT.uiChat)}\\s*\\/\\s*)?${escapeRegExp(FORBIDDEN_SOURCE_TEXT.uiCursor)}[-\\s]+${escapeRegExp(FORBIDDEN_SOURCE_TEXT.uiStyle)}(?![A-Za-z0-9])`,
      "i",
    ),
  },
  {
    id: SOURCE_RULE_IDS.externalUiReference,
    description: "外部产品 UI 参考描述",
    pattern: new RegExp(
      `(?:reference\\s*:\\s*${escapeRegExp(FORBIDDEN_SOURCE_TEXT.uiChat)}(?:\\s+settings)?|(?:product|floating[-\\s]+panel)\\s+reference|(?:外部产品|竞品)(?:的)?(?:界面|ui|样式|设计)(?:参考|对齐|复刻))`,
      "i",
    ),
  },
]);

/** 法定归属文本可以出现的来源名称规则，不包含本地路径或 UI 模仿措辞。 */
const LEGAL_ATTRIBUTION_RULE_IDS = new Set([
  SOURCE_RULE_IDS.legacyRuntime,
  SOURCE_RULE_IDS.researchProject,
  SOURCE_RULE_IDS.externalProduct,
  SOURCE_RULE_IDS.externalUiBetter,
]);

/**
 * 精确到仓库相对路径、规则与用途的允许列表。
 * 这里不会按目录或文件名模式跳过扫描；未列出的规则仍会在同一文件中失败。
 */
const EXACT_ALLOWLIST = new Map([
  [
    ".gitignore",
    {
      ruleIds: new Set([SOURCE_RULE_IDS.localReferencePath]),
      purpose: "保留研究目录的忽略规则，防止其进入版本控制",
    },
  ],
  [
    "LICENSE",
    {
      ruleIds: LEGAL_ATTRIBUTION_RULE_IDS,
      purpose: "根许可证可能必须保留法定来源或作者名称",
    },
  ],
]);

/** 将 Git 或 Windows 路径统一为稳定的仓库相对路径格式。 */
export function normalizeRepositoryPath(filePath) {
  return filePath.replaceAll("\\", "/").replace(/^\.\//, "");
}

/** 判断一个版本化路径是否属于需要读取的源码、文档或配置。 */
export function isScannablePath(filePath) {
  const normalizedPath = normalizeRepositoryPath(filePath);
  const fileName = basename(normalizedPath).toLowerCase();
  return (
    SCANNABLE_FILENAMES.has(fileName) ||
    SCANNABLE_EXTENSIONS.has(extname(fileName))
  );
}

/** 判断某条规则在指定精确路径上是否因明确用途而允许。 */
function isAllowedOccurrence(filePath, ruleId) {
  const allowance = EXACT_ALLOWLIST.get(filePath);
  return (
    typeof allowance?.purpose === "string" &&
    allowance.purpose.trim().length > 0 &&
    allowance.ruleIds.has(ruleId)
  );
}

/** 生成适合终端显示的单行证据，避免把超长源码整行写入日志。 */
function summarizeEvidence(value) {
  const compact = value.trim().replace(/\s+/g, " ");
  return compact.length > 180 ? `${compact.slice(0, 177)}...` : compact;
}

/** 对一个值执行所有规则，并将未获允许的首次命中加入结果。 */
function scanValue({ filePath, value, line, location }, findings) {
  for (const rule of FORBIDDEN_RULES) {
    const match = rule.pattern.exec(value);
    if (!match || isAllowedOccurrence(filePath, rule.id)) {
      continue;
    }
    findings.push({
      path: filePath,
      line,
      column: match.index + 1,
      location,
      ruleId: rule.id,
      description: rule.description,
      evidence: summarizeEvidence(value),
    });
  }
}

/** 扫描任意版本化路径本身，二进制资源也不能通过文件名携带禁止来源标识。 */
export function scanPath(filePath) {
  const normalizedPath = normalizeRepositoryPath(filePath);
  const findings = [];
  scanValue(
    {
      filePath: normalizedPath,
      value: normalizedPath,
      line: 0,
      location: "path",
    },
    findings,
  );
  return findings;
}

/** 只扫描文本内容；路径由调用方独立扫描，避免仓库遍历时重复报告。 */
function scanContent(filePath, content) {
  const findings = [];
  const lines = content.split(/\r\n|\n|\r/);
  for (const [index, line] of lines.entries()) {
    scanValue(
      {
        filePath,
        value: line,
        line: index + 1,
        location: "content",
      },
      findings,
    );
  }
  return findings;
}

/** 扫描一个仓库相对路径及其文本内容，返回可定位的禁止来源命中。 */
export function scanText(filePath, content) {
  const normalizedPath = normalizeRepositoryPath(filePath);
  return [
    ...scanPath(normalizedPath),
    ...scanContent(normalizedPath, content),
  ];
}

/** 从 Git 获取已版本化及未忽略待纳入文件，保证本地与 CI 使用同一来源清单。 */
export function listRepositoryFiles(repositoryRoot) {
  const output = execFileSync(
    "git",
    ["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    {
      cwd: repositoryRoot,
      encoding: "buffer",
      maxBuffer: GIT_FILE_LIST_MAX_BYTES,
      windowsHide: true,
    },
  );
  return output
    .toString("utf8")
    .split("\0")
    .filter(Boolean)
    .map(normalizeRepositoryPath)
    .sort();
}

/** 确认 Git 返回的路径解析后仍位于仓库根目录内。 */
function resolveRepositoryFile(repositoryRoot, filePath) {
  const absoluteRoot = resolve(repositoryRoot);
  const absolutePath = resolve(absoluteRoot, filePath);
  const relativePath = relative(absoluteRoot, absolutePath);
  if (
    relativePath === "" ||
    relativePath === ".." ||
    relativePath.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) ||
    isAbsolute(relativePath)
  ) {
    throw new Error(`Git 返回了仓库外路径：${filePath}`);
  }
  return absolutePath;
}

/** 扫描仓库当前工作树中的源码、文档和配置，并返回统计与全部命中。 */
export function scanRepository(repositoryRoot = process.cwd()) {
  const files = listRepositoryFiles(repositoryRoot);
  const findings = [];
  let scannedFiles = 0;

  for (const filePath of files) {
    findings.push(...scanPath(filePath));
    if (!isScannablePath(filePath)) {
      continue;
    }
    const absolutePath = resolveRepositoryFile(repositoryRoot, filePath);
    if (!existsSync(absolutePath) || !statSync(absolutePath).isFile()) {
      continue;
    }
    scannedFiles += 1;
    findings.push(
      ...scanContent(filePath, readFileSync(absolutePath, "utf8")),
    );
  }

  return { scannedPaths: files.length, scannedFiles, findings };
}

/** 将门禁结果格式化为稳定、可定位且有输出上限的中文报告。 */
export function formatGateReport(result) {
  if (result.findings.length === 0) {
    return `Clean-room 来源门禁通过：已检查 ${result.scannedPaths} 个版本化路径，并扫描 ${result.scannedFiles} 个源码、文档和配置文件。`;
  }

  const lines = [
    `Clean-room 来源门禁失败：发现 ${result.findings.length} 处禁止来源名称或参考描述。`,
  ];
  for (const finding of result.findings.slice(0, MAX_REPORTED_FINDINGS)) {
    const location =
      finding.location === "path"
        ? finding.path
        : `${finding.path}:${finding.line}:${finding.column}`;
    lines.push(`${location} [${finding.ruleId}] ${finding.description}`);
    lines.push(`  ${finding.evidence}`);
  }
  if (result.findings.length > MAX_REPORTED_FINDINGS) {
    lines.push(
      `另有 ${result.findings.length - MAX_REPORTED_FINDINGS} 处命中未展开。`,
    );
  }
  return lines.join("\n");
}

/**
 * 执行命令行门禁；成功为 0，命中为 1，扫描异常为 2。
 * package.json 的 check:clean-room 与 test 脚本调用本入口，CI 和发布检查复用 test 门禁。
 */
function main() {
  try {
    const result = scanRepository();
    const report = formatGateReport(result);
    if (result.findings.length > 0) {
      process.stderr.write(`${report}\n`);
      process.exitCode = 1;
      return;
    }
    process.stdout.write(`${report}\n`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`Clean-room 来源门禁无法执行：${message}\n`);
    process.exitCode = 2;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  main();
}
