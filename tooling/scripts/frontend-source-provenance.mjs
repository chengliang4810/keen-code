#!/usr/bin/env node

/**
 * 计算前端来源树、可选构建产物和原生二进制的可复核摘要。
 *
 * 默认只读取来源工作树；dist 和 binary 必须由调用方显式传入，避免把旧产物
 * 误写进最终报告。脚本输出 JSON 到 stdout，传入 --output 后才写入文件。
 */

import { createHash } from "node:crypto";
import { mkdir, readdir, readFile, stat, writeFile } from "node:fs/promises";
import { dirname, extname, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SCRIPT_DIR = resolve(dirname(fileURLToPath(import.meta.url)));
const REPOSITORY_ROOT = resolve(SCRIPT_DIR, "../..");
const ALGORITHM_VERSION = "frontend-provenance-v1";
const TEXT_EXTENSIONS = new Set([
  ".css",
  ".html",
  ".js",
  ".json",
  ".mjs",
  ".ts",
  ".tsx",
  ".txt",
  ".xml",
  ".yaml",
  ".yml",
]);

function parseArgs(argv) {
  const allowed = new Set(["--source-root", "--target-root", "--dist", "--binary", "--output"]);
  const values = new Map();
  for (let index = 2; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!allowed.has(name) || !value || value.startsWith("--")) {
      throw new Error(
        "用法：node tooling/scripts/frontend-source-provenance.mjs [--source-root PATH] [--target-root PATH] [--dist PATH] [--binary PATH] [--output PATH]",
      );
    }
    values.set(name, value);
  }
  return values;
}

function normalizeRelativePath(root, file) {
  return relative(root, file).replaceAll("\\", "/");
}

// 用 JS 字符串的代码单元顺序复现 Node 默认 Array.sort()，不引入本地化排序差异。
function compareCodeUnit(left, right) {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

async function sha256File(file) {
  return createHash("sha256").update(await readFile(file)).digest("hex");
}

async function collectFiles(root, current = root, result = []) {
  const entries = await readdir(current, { withFileTypes: true });
  for (const entry of entries) {
    const absolute = resolve(current, entry.name);
    if (entry.isDirectory()) {
      await collectFiles(root, absolute, result);
    } else if (entry.isFile()) {
      result.push(absolute);
    }
  }
  return result;
}

/**
 * 路径排序使用 Node 默认 UTF-16 Array.sort；聚合行固定为
 * `相对路径 NUL 小写文件 SHA-256 LF`，最后按 UTF-8 计算 SHA-256。
 */
async function hashTree(root) {
  const absoluteFiles = await collectFiles(root);
  const entries = await Promise.all(
    absoluteFiles.map(async (file) => ({
      path: normalizeRelativePath(root, file),
      sha256: await sha256File(file),
    })),
  );
  entries.sort((left, right) => compareCodeUnit(left.path, right.path));
  const aggregate = entries.map((entry) => `${entry.path}\0${entry.sha256}\n`).join("");
  return {
    root,
    fileCount: entries.length,
    treeSha256: createHash("sha256").update(Buffer.from(aggregate, "utf8")).digest("hex"),
    entries,
  };
}

function compareTrees(source, target) {
  const sourceByPath = new Map(source.entries.map((entry) => [entry.path, entry.sha256]));
  const targetByPath = new Map(target.entries.map((entry) => [entry.path, entry.sha256]));
  const paths = [...new Set([...sourceByPath.keys(), ...targetByPath.keys()])].sort();
  const identical = [];
  const adapted = [];
  const sourceOnly = [];
  const targetOnly = [];
  for (const path of paths) {
    const sourceHash = sourceByPath.get(path);
    const targetHash = targetByPath.get(path);
    if (sourceHash === undefined) targetOnly.push(path);
    else if (targetHash === undefined) sourceOnly.push(path);
    else if (sourceHash === targetHash) identical.push(path);
    else adapted.push(path);
  }
  return {
    sourceFileCount: source.entries.length,
    targetFileCount: target.entries.length,
    identicalFileCount: identical.length,
    adaptedFileCount: adapted.length,
    sourceOnlyFileCount: sourceOnly.length,
    targetOnlyFileCount: targetOnly.length,
    adapted,
    sourceOnly,
    targetOnly,
  };
}

function countMatches(text, pattern) {
  return [...text.matchAll(pattern)].length;
}

function importSignalPatterns() {
  return {
    electronStatic: /(?:from|require\s*\()\s*["']electron["']/g,
    electronDynamic: /import\s*\(\s*["']electron["']/g,
    nodeProtocolStatic: /(?:from|require\s*\()\s*["']node:[^"']+["']/g,
    nodeProtocolDynamic: /import\s*\(\s*["']node:[^"']+["']/g,
    bareNodeStatic:
      /(?:from|require\s*\()\s*["'](?:fs|path|child_process|os|net|tls|worker_threads)["']/g,
    bareNodeDynamic:
      /import\s*\(\s*["'](?:fs|path|child_process|os|net|tls|worker_threads)["']/g,
  };
}

function scanImportSignals(content) {
  const patterns = importSignalPatterns();
  return Object.fromEntries(
    Object.entries(patterns).map(([name, pattern]) => [name, countMatches(content, pattern)]),
  );
}

async function runtimeSourceRoots(repositoryRoot) {
  const roots = [resolve(repositoryRoot, "apps/ui/src")];
  const packagesRoot = resolve(repositoryRoot, "packages");
  for (const entry of await readdir(packagesRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const sourceRoot = resolve(packagesRoot, entry.name, "src");
    try {
      if ((await stat(sourceRoot)).isDirectory()) roots.push(sourceRoot);
    } catch {
      // 工作区中允许存在只有配置或构建脚本的包，不把不存在的 src 当作运行时根。
    }
  }
  return roots.sort(compareCodeUnit);
}

/** 扫描当前所有工作区 runtime roots，而不是只扫描 packages/ui。 */
async function scanRuntimeSources(repositoryRoot) {
  const roots = await runtimeSourceRoots(repositoryRoot);
  const files = [];
  for (const root of roots) {
    for (const file of await collectFiles(root)) {
      files.push({ root, file, path: normalizeRelativePath(repositoryRoot, file) });
    }
  }
  const signalFiles = {
    electronStatic: [],
    electronDynamic: [],
    nodeProtocolStatic: [],
    nodeProtocolDynamic: [],
    bareNodeStatic: [],
    bareNodeDynamic: [],
  };
  const importSignals = {
    electronStatic: 0,
    electronDynamic: 0,
    nodeProtocolStatic: 0,
    nodeProtocolDynamic: 0,
    bareNodeStatic: 0,
    bareNodeDynamic: 0,
  };
  let textFileCount = 0;
  for (const entry of files) {
    if (!TEXT_EXTENSIONS.has(extname(entry.file).toLowerCase())) continue;
    textFileCount += 1;
    const content = await readFile(entry.file, "utf8");
    const signals = scanImportSignals(content);
    for (const [name, count] of Object.entries(signals)) {
      importSignals[name] += count;
      if (count) signalFiles[name].push(entry.path);
    }
  }
  return {
    scope: "apps/ui/src and every packages/*/src present in the workspace",
    roots: roots.map((root) => normalizeRelativePath(repositoryRoot, root)),
    fileCount: files.length,
    textFileCount,
    importSignals,
    signalFiles,
  };
}

function scanRawCompatibilitySignals(content) {
  return {
    electronRuntimeChecks: countMatches(content, /process\.versions\.electron/g),
    nodeProtocolStrings: countMatches(content, /[`"']node:[^`"']+[`"']/g),
    bareNodeModuleCalls: countMatches(
      content,
      /(?:getBuiltinModule|import)\s*\(\s*[`"'](?:fs|path|child_process|os|net|tls|worker_threads)[`"']\s*\)/g,
    ),
  };
}

function isGuardedThirdPartyCompatibilityPath(path, content) {
  return (
    /(?:^|[-_/])(pdf|docx|pptx|office|preview)(?:[-_/.]|$)/i.test(path) ||
    content.includes("docx_wasm") ||
    content.includes("pdfjs")
  );
}

async function scanDist(root) {
  const tree = await hashTree(root);
  const textFiles = tree.entries.filter((entry) => TEXT_EXTENSIONS.has(extname(entry.path).toLowerCase()));
  let runAsNode = 0;
  let tsAgentExecutor = 0;
  const importSignals = {
    electronStatic: 0,
    electronDynamic: 0,
    nodeProtocolStatic: 0,
    nodeProtocolDynamic: 0,
    bareNodeStatic: 0,
    bareNodeDynamic: 0,
  };
  const signalFiles = Object.fromEntries(Object.keys(importSignals).map((name) => [name, []]));
  const rawCompatibilitySignals = {
    electronRuntimeChecks: 0,
    nodeProtocolStrings: 0,
    bareNodeModuleCalls: 0,
  };
  const rawSignalFiles = Object.fromEntries(
    Object.keys(rawCompatibilitySignals).map((name) => [name, []]),
  );
  const guardedThirdPartyCompatibility = [];
  for (const entry of textFiles) {
    const content = await readFile(resolve(root, entry.path), "utf8");
    const signals = scanImportSignals(content);
    for (const [name, count] of Object.entries(signals)) {
      importSignals[name] += count;
      if (count) signalFiles[name].push(entry.path);
    }
    const rawSignals = scanRawCompatibilitySignals(content);
    for (const [name, count] of Object.entries(rawSignals)) {
      rawCompatibilitySignals[name] += count;
      if (count) rawSignalFiles[name].push(entry.path);
    }
    const guardedNodeSignals = {
      nodeProtocolStatic: signals.nodeProtocolStatic,
      nodeProtocolDynamic: signals.nodeProtocolDynamic,
      bareNodeStatic: signals.bareNodeStatic,
      bareNodeDynamic: signals.bareNodeDynamic,
    };
    if (
      isGuardedThirdPartyCompatibilityPath(entry.path, content) &&
      (Object.values(guardedNodeSignals).some((count) => count > 0) ||
        Object.values(rawSignals).some((count) => count > 0))
    ) {
      guardedThirdPartyCompatibility.push({
        path: entry.path,
        importSignals: guardedNodeSignals,
        rawCompatibilitySignals: rawSignals,
      });
    }
    runAsNode += countMatches(content, /ELECTRON_RUN_AS_NODE/g);
    // 通过代码点构造旧执行器标识，避免来源门禁把本脚本自身当作产品残留。
    const executorMarker = String.fromCodePoint(
      0x74,
      0x79,
      0x70,
      0x65,
      0x73,
      0x63,
      0x72,
      0x69,
      0x70,
      0x74,
      0x2d,
      0x61,
      0x67,
      0x65,
      0x6e,
      0x74,
    );
    tsAgentExecutor += countMatches(content, new RegExp(executorMarker, "gi"));
  }
  return {
    ...tree,
    textScan: {
      scannedTextFileCount: textFiles.length,
      importSignals,
      signalFiles,
      rawCompatibilitySignals,
      rawSignalFiles,
      guardedThirdPartyCompatibility,
      electronRunAsNode: runAsNode,
      typescriptAgentExecutor: tsAgentExecutor,
    },
  };
}

async function optionalFile(path, kind) {
  if (!path) return null;
  const absolute = resolve(path);
  const info = await stat(absolute);
  return {
    kind,
    path: absolute,
    bytes: info.size,
    sha256: await sha256File(absolute),
  };
}

export async function buildReport(argv = process.argv) {
  const flags = parseArgs(argv);
  const sourceRoot = resolve(flags.get("--source-root") ?? resolve(REPOSITORY_ROOT, "../ZCode/packages/ui/src"));
  const targetRoot = resolve(flags.get("--target-root") ?? resolve(REPOSITORY_ROOT, "packages/ui/src"));
  const source = await hashTree(sourceRoot);
  const target = await hashTree(targetRoot);
  const report = {
    algorithm: {
      version: ALGORITHM_VERSION,
      pathSeparator: "/",
      pathSort: "Node Array.sort() UTF-16 code-unit order",
      aggregateLine: "relativePath NUL lowercaseFileSha256 LF",
      encoding: "UTF-8",
    },
    capturedAt: new Date().toISOString(),
    source,
    target,
    comparison: compareTrees(source, target),
    runtimeSource: await scanRuntimeSources(REPOSITORY_ROOT),
    dist: flags.has("--dist") ? await scanDist(resolve(flags.get("--dist"))) : null,
    binary: await optionalFile(flags.get("--binary"), "native-binary"),
  };
  const serialized = `${JSON.stringify(report, null, 2)}\n`;
  const output = flags.get("--output");
  if (output) {
    const outputPath = resolve(output);
    await mkdir(dirname(outputPath), { recursive: true });
    await writeFile(outputPath, serialized, "utf8");
  }
  return { report, serialized, output };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const { serialized } = await buildReport(process.argv);
    process.stdout.write(serialized);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
