#!/usr/bin/env node

/**
 * 校验 Tauri bundle 的许可证资源。
 *
 * 该脚本先校验 tauri.conf.json 的资源映射，再在构建输出中寻找完整的
 * resources 目录，并以源文件 SHA-256 确认安装包资源没有被替换或截断。
 */

import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

// 安装包必须携带项目自身的 MIT 许可证，不依赖已移除的第三方汇总文件。
const REQUIRED_RESOURCES = ["LICENSE"];
// 首版单个平台最终安装包的硬上限；按 50 MB（50,000,000 字节）执行。
export const MAX_BUNDLE_SIZE_BYTES = 50_000_000;
// 允许进入发布体积门禁的实际安装或分发文件类型；展开的 .app 目录单独统计。
const INSTALLER_BUNDLE_KINDS = new Set(["msi", "nsis", "dmg", "deb", "rpm", "appimage", "tar", "zip"]);
const SCRIPT_DIRECTORY = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = resolve(SCRIPT_DIRECTORY, "..");

// 计算文件摘要，用于源文件和 Tauri bundle 内容的字节级比对。
export function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

// Tauri 的 tauri-build 会先把映射资源暂存到 target/{debug,release} 根目录。
// 这些目录不是最终 macOS .app 的 Resources 目录，但在 Windows 安装包生成前
// 就是 bundler 的输入，因此也应纳入发布前的字节摘要校验。
function hasRequiredResources(directory) {
  return REQUIRED_RESOURCES.every((resource) => {
    const path = join(directory, resource);
    try {
      return existsSync(path) && lstatSync(path).isFile();
    } catch {
      return false;
    }
  });
}

// 将配置资源目标规范化为跨平台的相对路径。
function normalizeTarget(target) {
  const value = String(target ?? "").replaceAll("\\", "/");
  if (!value || value.startsWith("/") || value.split("/").includes("..")) return null;
  return value.replace(/^\.\//, "");
}

// 解析 Tauri resources 数组或映射，并返回源路径到目标路径的稳定列表。
export function configuredResourceMappings(config, configPath) {
  const resources = config.bundle?.resources;
  if (!resources) throw new Error("tauri.conf.json 缺少 bundle.resources");
  const configDirectory = dirname(resolve(configPath));
  if (Array.isArray(resources)) {
    return resources.map((source) => ({
      source: resolve(configDirectory, String(source)),
      target: normalizeTarget(String(source).split(/[\\/]/).pop()),
    }));
  }
  if (typeof resources === "object") {
    return Object.entries(resources).map(([source, target]) => ({
      source: resolve(configDirectory, source),
      target: normalizeTarget(target),
    }));
  }
  throw new Error("tauri.conf.json 的 bundle.resources 类型无效");
}

// 校验配置显式携带项目自身的根 MIT 许可证。
export function validateBundleConfiguration(config, configPath = join(REPOSITORY_ROOT, "src-tauri/tauri.conf.json")) {
  if (config.bundle?.license !== "MIT") throw new Error("Tauri bundle.license 必须为 MIT");
  const licenseFile = config.bundle?.licenseFile;
  if (resolve(dirname(resolve(configPath)), String(licenseFile)) !== join(REPOSITORY_ROOT, "LICENSE")) {
    throw new Error("Tauri bundle.licenseFile 必须指向仓库根 LICENSE");
  }
  const mappings = configuredResourceMappings(config, configPath);
  const failures = [];
  for (const resource of REQUIRED_RESOURCES) {
    const expectedSource = join(REPOSITORY_ROOT, resource);
    const match = mappings.find((item) => item.source === expectedSource && item.target === resource);
    if (!match) failures.push(`${resource} 未映射到同名 bundle 资源`);
  }
  const targets = new Map();
  for (const mapping of mappings) {
    if (!mapping.target) failures.push(`${mapping.source} 的 bundle 目标路径无效`);
    else if (targets.has(mapping.target)) {
      failures.push(
        `bundle 目标路径冲突：${mapping.target} 同时映射自 ${targets.get(mapping.target)} 和 ${mapping.source}`,
      );
    } else {
      targets.set(mapping.target, mapping.source);
    }
  }
  if (failures.length) throw new Error(failures.join("；"));
  return mappings;
}

// 在 Tauri target 目录中查找 resources 或 macOS .app/Contents/Resources 目录。
export function findResourceDirectories(targetRoot, includeNestedResourceRoots = false) {
  const result = [];
  if (!existsSync(targetRoot)) return result;
  const pending = [{ path: resolve(targetRoot), depth: 0 }];
  const visited = new Set();
  while (pending.length) {
    const current = pending.pop();
    let realPath;
    try {
      realPath = resolve(current.path);
      if (visited.has(realPath)) continue;
      visited.add(realPath);
      if (!lstatSync(realPath).isDirectory()) continue;
    } catch {
      continue;
    }
    const normalized = realPath.replaceAll("\\", "/");
    const directoryName = normalized.split("/").at(-1) ?? "";
    // 只把包含法律文件的通用目录视为候选；构建输出中的 resources/ 目录
    // 也可能只放图标等运行时资源，不能因为目录名相同就把它误判为法律资源根。
    // macOS 的 Contents/Resources 是固定 bundle 根，即使文件缺失也要保留候选，
    // 让后续校验给出明确的缺失报告。
    if (
      normalized.endsWith("/Contents/Resources") ||
      (normalized.endsWith("/resources") && hasRequiredResources(realPath)) ||
      (includeNestedResourceRoots
        ? hasRequiredResources(realPath)
        : current.depth <= 1 && ["debug", "release"].includes(directoryName) && hasRequiredResources(realPath))
    ) {
      result.push(realPath);
    }
    if (current.depth >= 10) continue;
    let entries;
    try {
      entries = readdirSync(realPath, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
      pending.push({ path: join(realPath, entry.name), depth: current.depth + 1 });
    }
  }
  return [...new Set(result)].sort();
}

// 只把 Tauri 生成的最终发行物视为成品；普通 target/debug/*.exe 不是可验证的 bundle。
export function classifyBundleArtifact(path) {
  const normalized = resolve(path).replaceAll("\\", "/");
  const name = normalized.split("/").at(-1) ?? "";
  if (/\.msi$/i.test(name)) return "msi";
  if (/\.exe$/i.test(name) && /\/bundle\/nsis\//i.test(normalized)) return "nsis";
  if (/\.app$/i.test(name)) return "app";
  if (/\.dmg$/i.test(name)) return "dmg";
  if (/\.deb$/i.test(name)) return "deb";
  if (/\.rpm$/i.test(name)) return "rpm";
  if (/\.AppImage$/i.test(name)) return "appimage";
  if (/(?:\.tar\.gz|\.tgz|\.tar)$/i.test(name)) return "tar";
  if (/\.zip$/i.test(name)) return "zip";
  return null;
}

// 在 target 目录递归列出最终 MSI、NSIS、macOS、Linux 和归档 bundle。
export function findBundleArtifacts(targetRoot) {
  const result = [];
  if (!existsSync(targetRoot)) return result;
  const pending = [{ path: resolve(targetRoot), depth: 0 }];
  const visited = new Set();
  while (pending.length) {
    const current = pending.pop();
    let realPath;
    let info;
    try {
      realPath = resolve(current.path);
      if (visited.has(realPath)) continue;
      visited.add(realPath);
      info = lstatSync(realPath);
    } catch {
      continue;
    }
    if (info.isFile()) {
      const kind = classifyBundleArtifact(realPath);
      if (kind) result.push({ kind, path: realPath });
      continue;
    }
    if (!info.isDirectory() || current.depth >= 12) continue;
    const kind = classifyBundleArtifact(realPath);
    if (kind === "app") result.push({ kind, path: realPath });
    let entries;
    try {
      entries = readdirSync(realPath, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      pending.push({ path: join(realPath, entry.name), depth: current.depth + 1 });
    }
  }
  return result.sort((left, right) => left.path.localeCompare(right.path));
}

// 递归计算展开目录的实际文件字节数；不跟随符号链接，范围严格限制在已发现的 bundle 内。
function measureExpandedBundleBytes(path) {
  const info = lstatSync(path);
  if (info.isFile()) return statSync(path).size;
  if (!info.isDirectory()) throw new Error(`bundle 成品不是文件或目录：${path}`);
  let total = 0;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) continue;
    total += measureExpandedBundleBytes(join(path, entry.name));
  }
  return total;
}

// 规范化成品声明，并确认声明类型、文件后缀/目录分类和实际 lstat 类型完全一致。
function normalizeBundleArtifact(artifact) {
  const path = typeof artifact === "string" ? artifact : artifact?.path;
  if (typeof path !== "string" || path.length === 0) throw new Error("bundle 成品路径无效");
  const resolvedPath = resolve(path);
  const detectedKind = classifyBundleArtifact(resolvedPath);
  const declaredKind = typeof artifact === "string" ? detectedKind : artifact?.kind;
  if (declaredKind !== detectedKind) {
    throw new Error(`bundle 成品类型与路径分类不一致：声明 ${declaredKind ?? "<empty>"}，实际 ${detectedKind ?? "<unknown>"}`);
  }
  if (declaredKind !== "app" && !INSTALLER_BUNDLE_KINDS.has(declaredKind)) {
    throw new Error(`bundle 成品类型不支持：${declaredKind ?? "<empty>"}`);
  }
  let info;
  try {
    info = lstatSync(resolvedPath);
  } catch {
    throw new Error(`bundle 成品不存在：${resolvedPath}`);
  }
  if (declaredKind === "app" ? !info.isDirectory() : !info.isFile()) {
    throw new Error(`bundle 成品类型与文件系统类型不一致：${resolvedPath}`);
  }
  return { kind: declaredKind, path: resolvedPath };
}

// 返回最终 bundle 的大小；实际安装包使用 stat，.app 展开目录按普通文件累计。
export function bundleArtifactSizeBytes(artifact) {
  const normalized = normalizeBundleArtifact(artifact);
  return normalized.kind === "app"
    ? measureExpandedBundleBytes(normalized.path)
    : statSync(normalized.path).size;
}

// 对实际安装或分发文件执行 50 MB 硬门禁；.app 只作为独立展开信息，不能替代安装包。
export function validateBundleSizes(artifacts, maximumBytes = MAX_BUNDLE_SIZE_BYTES) {
  if (!Array.isArray(artifacts) || artifacts.length === 0) {
    throw new Error("未找到实际安装/分发包成品；空目录不能通过体积校验");
  }
  if (!Number.isSafeInteger(maximumBytes) || maximumBytes < 0) {
    throw new Error("安装包体积上限必须是非负安全整数");
  }
  const reports = [];
  const expandedBytes = [];
  const failures = [];
  for (const artifact of artifacts) {
    const path = typeof artifact === "string" ? artifact : artifact?.path;
    try {
      const normalized = normalizeBundleArtifact(artifact);
      if (normalized.kind === "app") {
        expandedBytes.push({ path: normalized.path, bytes: measureExpandedBundleBytes(normalized.path) });
        continue;
      }
      const bytes = statSync(normalized.path).size;
      const report = { kind: normalized.kind, path: normalized.path, bytes };
      reports.push(report);
      if (bytes === 0) {
        failures.push(`${relative(REPOSITORY_ROOT, normalized.path)} 大小为 0 字节，不能作为安装包发布`);
      } else if (bytes > maximumBytes) {
        failures.push(
          `${relative(REPOSITORY_ROOT, normalized.path)} 超过 ${maximumBytes} 字节上限（实际 ${bytes} 字节）`,
        );
      }
    } catch (error) {
      failures.push(`${path ?? "<empty>"}：${error.message}`);
    }
  }
  if (failures.length > 0) throw new Error(`最终 bundle 体积校验失败：\n${failures.join("\n")}`);
  if (reports.length === 0) {
    throw new Error(
      expandedBytes.length > 0
        ? "未找到实际安装/分发包成品；仅有 .app 展开目录不能通过体积校验"
        : "未找到实际安装/分发包成品；空目录不能通过体积校验",
    );
  }
  return { artifacts: reports, expandedBytes };
}

// updater archive 中的最终成品通常没有 target/release/bundle 目录前缀，
// 仅凭外层目录无法复用 classifyBundleArtifact，需要按文件名识别安装器。
export function classifyNestedBundleArtifact(path) {
  const normalized = resolve(path).replaceAll("\\", "/");
  const name = normalized.split("/").at(-1) ?? "";
  if (/\.msi$/i.test(name)) return "msi";
  if (/(?:-setup|_setup|installer|setup)\.exe$/i.test(name)) return "nsis";
  if (/\.app$/i.test(name)) return "app";
  if (/\.AppImage$/i.test(name)) return "appimage";
  if (/(?:\.tar\.gz|\.tgz|\.tar)$/i.test(name)) return "tar";
  if (/\.zip$/i.test(name)) return "zip";
  return null;
}

// 递归查找 updater archive 中嵌套的最终成品；签名文件和普通运行时文件会被忽略。
export function findNestedBundleArtifacts(root) {
  const result = [];
  if (!existsSync(root)) return result;
  const pending = [{ path: resolve(root), depth: 0 }];
  const visited = new Set();
  while (pending.length) {
    const current = pending.pop();
    let realPath;
    let info;
    try {
      realPath = resolve(current.path);
      if (visited.has(realPath)) continue;
      visited.add(realPath);
      info = lstatSync(realPath);
    } catch {
      continue;
    }
    const kind = classifyNestedBundleArtifact(realPath);
    if (kind === "app" && info.isDirectory()) {
      result.push({ kind, path: realPath });
      continue;
    }
    if (info.isFile() && kind) {
      result.push({ kind, path: realPath });
      continue;
    }
    if (!info.isDirectory() || current.depth >= 10) continue;
    let entries;
    try {
      entries = readdirSync(realPath, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      pending.push({ path: join(realPath, entry.name), depth: current.depth + 1 });
    }
  }
  return result.sort((left, right) => left.path.localeCompare(right.path));
}

// 检查资源目录中项目许可证是否与源文件逐字节一致。
export function validateResourceDirectory(resourceDirectory, repositoryRoot = REPOSITORY_ROOT) {
  const failures = [];
  for (const resource of REQUIRED_RESOURCES) {
    const source = join(repositoryRoot, resource);
    const bundled = join(resourceDirectory, resource);
    if (!existsSync(bundled)) {
      failures.push(`${relative(repositoryRoot, resourceDirectory)} 缺少 ${resource}`);
      continue;
    }
    if (sha256File(source) !== sha256File(bundled)) {
      failures.push(`${relative(repositoryRoot, resourceDirectory)} 中的 ${resource} 与源文件摘要不一致`);
    }
  }
  return failures;
}

// 在临时目录中运行外部 bundle 工具；参数全部作为 argv 传递，避免把成品路径拼进 shell。
function runBundleTool(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    env: options.env,
    encoding: "utf8",
    stdio: "ignore",
    timeout: options.timeout ?? 180000,
    windowsHide: true,
  });
  if (result.error) throw new Error(`${command} 启动失败：${result.error.message}`);
  if (result.status !== 0) throw new Error(`${command} 执行失败（退出码 ${result.status ?? "unknown"}）`);
}

// 仅回收本脚本在系统临时目录创建的独占子目录；清理失败必须显式报错。
function removeTemporaryDirectory(path) {
  const target = resolve(path);
  const info = lstatSync(target);
  if (!info.isDirectory() || info.isSymbolicLink()
    || dirname(realpathSync(target)) !== realpathSync(tmpdir())
    || !/^keencode-(msi|nsis|dmg|archive)-[A-Za-z0-9]+$/.test(basename(target))) {
    throw new Error(`拒绝清理非本次验包临时目录：${target}`);
  }
  rmSync(target, { recursive: true, force: false, maxRetries: 3, retryDelay: 100 });
}

// 校验解包或挂载目录中的所有候选资源目录，不能只因找到一个目录便忽略损坏副本。
function validateExtractedResources(root, label, repositoryRoot = REPOSITORY_ROOT) {
  const directories = findResourceDirectories(root, true);
  if (directories.length === 0) throw new Error(`${label} 解包后未找到完整法律资源目录`);
  const failures = directories.flatMap((directory) =>
    validateResourceDirectory(directory, repositoryRoot).map((failure) => `${label}: ${failure}`),
  );
  if (failures.length > 0) throw new Error(failures.join("\n"));
  return directories;
}

// updater archive 可能只包含 NSIS/MSI 安装器；递归找到嵌套成品并复用同一校验入口。
export function validateExtractedBundle(root, label, repositoryRoot, verificationContext) {
  const directories = findResourceDirectories(root, true);
  const nestedArtifacts = findNestedBundleArtifacts(root);
  if (directories.length === 0 && nestedArtifacts.length === 0) {
    throw new Error(`${label} 解包后未找到完整法律资源目录或嵌套最终成品`);
  }
  const failures = directories.flatMap((directory) =>
    validateResourceDirectory(directory, repositoryRoot).map((failure) => `${label}: ${failure}`),
  );
  if (failures.length > 0) throw new Error(failures.join("\n"));
  for (const nestedArtifact of nestedArtifacts) {
    verifyBundleArtifact(nestedArtifact, repositoryRoot, verificationContext);
  }
  return [...directories, ...nestedArtifacts.map((artifact) => artifact.path)];
}

// 用 MSI administrative install 解包成品，直接检查安装文件而不是只检查 target/release 暂存目录。
function verifyMsiArtifact(artifact, repositoryRoot) {
  if (process.platform !== "win32") throw new Error("MSI 成品只能在 Windows 上校验");
  const extractionRoot = mkdtempSync(join(tmpdir(), "keencode-msi-"));
  try {
    runBundleTool("msiexec.exe", ["/a", artifact, "/qn", `TARGETDIR=${extractionRoot}`]);
    return validateExtractedResources(extractionRoot, relative(repositoryRoot, artifact), repositoryRoot);
  } finally {
    removeTemporaryDirectory(extractionRoot);
  }
}

// NSIS 安装器会写注册表；只能调用完整 7-Zip 解包，绝不运行安装器或卸载程序。
// 可用 KEENCODE_7ZIP 指定已安装的完整 7z 可执行文件；精简版 7za 不支持 NSIS。
export function nsisExtractionInvocation(artifact, extractionRoot) {
  const installed = join(process.env.ProgramFiles ?? "C:/Program Files", "7-Zip", "7z.exe");
  const command = process.env.KEENCODE_7ZIP || (process.platform === "win32" && existsSync(installed) ? installed : "7z");
  return { command, args: ["x", "-y", "-bd", `-o${extractionRoot}`, "--", resolve(artifact)] };
}

// 成品只作为解包器的数据参数；注入运行入口便于离线测试确认没有执行安装脚本。
export function verifyNsisArtifact(artifact, repositoryRoot, extract = runBundleTool) {
  const extractionRoot = mkdtempSync(join(tmpdir(), "keencode-nsis-"));
  try {
    const { command, args } = nsisExtractionInvocation(artifact, extractionRoot);
    try {
      extract(command, args);
    } catch (error) {
      throw new Error(`NSIS 只读解包失败，请提供支持 NSIS 的完整 7-Zip（PATH 或 KEENCODE_7ZIP）；不会回退为安装执行：${error.message}`);
    }
    return validateExtractedResources(extractionRoot, relative(repositoryRoot, artifact), repositoryRoot);
  } finally {
    removeTemporaryDirectory(extractionRoot);
  }
}

// macOS .app 已是目录 bundle，直接检查 Contents/Resources 中的实际资源。
function verifyAppArtifact(artifact, repositoryRoot) {
  const resources = join(artifact, "Contents", "Resources");
  const failures = validateResourceDirectory(resources, repositoryRoot);
  if (failures.length > 0) throw new Error(failures.join("\n"));
  return [resources];
}

// 在 macOS 上只读挂载 DMG，再校验其中的 .app；不会把磁盘映像安装到用户目录。
function verifyDmgArtifact(artifact, repositoryRoot) {
  if (process.platform !== "darwin") throw new Error("DMG 成品只能在 macOS 上校验");
  const extractionRoot = mkdtempSync(join(tmpdir(), "keencode-dmg-"));
  const mountPoint = join(extractionRoot, "mount");
  mkdirSync(mountPoint);
  const attached = spawnSync("hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mountPoint, artifact], {
    encoding: "utf8",
    stdio: "ignore",
    timeout: 180000,
  });
  if (attached.error || attached.status !== 0) {
    removeTemporaryDirectory(extractionRoot);
    throw new Error(`${relative(repositoryRoot, artifact)} 挂载失败`);
  }
  try {
    return validateExtractedResources(mountPoint, relative(repositoryRoot, artifact), repositoryRoot);
  } finally {
    spawnSync("hdiutil", ["detach", mountPoint, "-force"], { encoding: "utf8", stdio: "ignore", timeout: 60000 });
    removeTemporaryDirectory(extractionRoot);
  }
}

// 解包 Debian、RPM、AppImage 和归档成品；每个工具都写入临时目录后再做字节摘要校验。
function verifyArchiveArtifact(artifact, kind, repositoryRoot, verificationContext) {
  const extractionRoot = mkdtempSync(join(tmpdir(), "keencode-archive-"));
  try {
    if (kind === "deb") {
      runBundleTool("dpkg-deb", ["-x", artifact, extractionRoot]);
    } else if (kind === "rpm") {
      runBundleTool("rpm", ["--root", extractionRoot, "--initdb"]);
      runBundleTool("rpm", ["--root", extractionRoot, "--install", "--nodeps", "--noscripts", artifact]);
    } else if (kind === "appimage") {
      runBundleTool(artifact, ["--appimage-extract"], { cwd: extractionRoot });
    } else if (kind === "tar") {
      runBundleTool("tar", ["-xf", artifact, "-C", extractionRoot]);
    } else if (kind === "zip") {
      runBundleTool(process.platform === "win32" ? "tar.exe" : "unzip", process.platform === "win32"
        ? ["-xf", artifact, "-C", extractionRoot]
        : ["-q", artifact, "-d", extractionRoot]);
    } else {
      throw new Error(`不支持的 bundle 类型：${kind}`);
    }
    return validateExtractedBundle(
      extractionRoot,
      relative(repositoryRoot, artifact),
      repositoryRoot,
      verificationContext,
    );
  } finally {
    removeTemporaryDirectory(extractionRoot);
  }
}

// 校验单个最终成品；MSI 和 NSIS 必须各自完成解包/安装后再比对源文件摘要。
export function verifyBundleArtifact(artifact, repositoryRoot = REPOSITORY_ROOT, verificationContext = null) {
  const value = typeof artifact === "string" ? { kind: classifyBundleArtifact(artifact), path: resolve(artifact) } : artifact;
  if (!value?.kind || !value.path) throw new Error("bundle 成品路径或类型无效");
  // 对象来源可能来自跨平台归档树，必须在进入 Windows 工具前转换为本机绝对路径；保留显式 kind，不能重新按文件名分类嵌套 NSIS。
  const normalizedValue = { ...value, path: resolve(value.path) };
  if (!existsSync(normalizedValue.path) || !lstatSync(normalizedValue.path).isFile() && normalizedValue.kind !== "app") {
    throw new Error(`bundle 成品不存在：${normalizedValue.path}`);
  }
  const key = `${normalizedValue.kind}:${normalizedValue.kind === "app" ? normalizedValue.path : sha256File(normalizedValue.path)}`;
  const previous = verificationContext?.verifiedPayloads?.get(key);
  if (previous) return previous;
  let result;
  switch (normalizedValue.kind) {
    case "msi":
      result = verifyMsiArtifact(normalizedValue.path, repositoryRoot);
      break;
    case "nsis":
      result = verifyNsisArtifact(normalizedValue.path, repositoryRoot);
      break;
    case "app":
      result = verifyAppArtifact(normalizedValue.path, repositoryRoot);
      break;
    case "dmg":
      result = verifyDmgArtifact(normalizedValue.path, repositoryRoot);
      break;
    case "deb":
    case "rpm":
    case "appimage":
    case "tar":
    case "zip":
      result = verifyArchiveArtifact(normalizedValue.path, normalizedValue.kind, repositoryRoot, verificationContext);
      break;
    default:
      throw new Error(`不支持的 bundle 类型：${normalizedValue.kind}`);
  }
  verificationContext?.verifiedPayloads?.set(key, result);
  return result;
}

// 校验配置和最终发行成品；仅有 target/release 暂存资源而没有真实 bundle 时必须失败。
export function verifyBundle(targetRoot = join(REPOSITORY_ROOT, "src-tauri/target"), configPath = join(REPOSITORY_ROOT, "src-tauri/tauri.conf.json")) {
  const config = JSON.parse(readFileSync(configPath, "utf8"));
  validateBundleConfiguration(config, configPath);
  const artifacts = findBundleArtifacts(targetRoot);
  if (artifacts.length === 0) {
    throw new Error(`未在 ${targetRoot} 找到最终 Tauri bundle 成品；未找到实际安装/分发包成品，空目录不能通过体积校验`);
  }
  validateBundleSizes(artifacts);
  if (process.platform === "win32") {
    const kinds = new Set(artifacts.map((artifact) => artifact.kind));
    for (const required of ["msi", "nsis"]) {
      if (!kinds.has(required)) throw new Error(`Windows bundle 缺少最终 ${required.toUpperCase()} 成品`);
    }
  }
  const failures = [];
  const validArtifacts = [];
  const verificationContext = { verifiedPayloads: new Map() };
  const orderedArtifacts = [...artifacts].sort((left, right) => {
    const leftArchive = ["dmg", "deb", "rpm", "appimage", "tar", "zip"].includes(left.kind);
    const rightArchive = ["dmg", "deb", "rpm", "appimage", "tar", "zip"].includes(right.kind);
    return Number(leftArchive) - Number(rightArchive) || left.path.localeCompare(right.path);
  });
  for (const artifact of orderedArtifacts) {
    try {
      verifyBundleArtifact(artifact, REPOSITORY_ROOT, verificationContext);
      validArtifacts.push(artifact.path);
    } catch (error) {
      failures.push(`${relative(REPOSITORY_ROOT, artifact.path)}：${error.message}`);
    }
  }
  if (failures.length > 0) throw new Error(`最终 bundle 许可证资源校验失败：\n${failures.join("\n")}`);
  return validArtifacts.sort();
}

// 解析 CLI 参数并执行发布前资源验证。
function main() {
  const args = process.argv.slice(2);
  const targetIndex = args.indexOf("--target");
  const configIndex = args.indexOf("--config");
  const targetRoot = targetIndex >= 0 ? resolve(args[targetIndex + 1]) : join(REPOSITORY_ROOT, "src-tauri/target");
  const configPath = configIndex >= 0 ? resolve(args[configIndex + 1]) : join(REPOSITORY_ROOT, "src-tauri/tauri.conf.json");
  const directories = verifyBundle(targetRoot, configPath);
  console.log(`Tauri 许可证资源校验通过：${directories.map((path) => relative(REPOSITORY_ROOT, path)).join(", ")}`);
}

const invokedScript = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedScript === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(`Tauri 许可证资源校验失败：${error.message}`);
    process.exitCode = 1;
  }
}
