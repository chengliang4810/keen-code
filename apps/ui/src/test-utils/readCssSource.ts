import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const LOCAL_IMPORT_PATTERN =
  /@import\s+(?:url\(\s*)?(["'])(\.\.?\/[^"']+)\1\s*\)?\s*;/g;

/** 稳定真实源码的原始文本缓存，键统一为规范化本地路径。 */
const RAW_SOURCE_CACHE = new Map<string, string>();
/** 已递归展开本地导入的 CSS 文本缓存，避免同一入口重复读取整条导入链。 */
const EXPANDED_CSS_CACHE = new Map<string, string>();

/** 源码 fixture 的读取选项。 */
export interface ReadSourceOptions {
  /** 是否缓存稳定源码；测试中会被改写的临时文件必须设为 false。 */
  cache?: boolean;
  /** 是否递归展开本地 CSS 导入；默认按 .css 扩展名自动判断。 */
  expandCssImports?: boolean;
}

/** 将 URL 或文件系统路径统一为不含相对段的本地文件 URL。 */
function normalizeSourceUrl(location: URL | string): URL {
  const sourceUrl =
    location instanceof URL
      ? location
      : location.startsWith("file:")
        ? new URL(location)
        : pathToFileURL(resolve(location));
  if (sourceUrl.protocol !== "file:") {
    throw new Error(`源码 fixture 只支持本地文件: ${sourceUrl.href}`);
  }
  return pathToFileURL(resolve(fileURLToPath(sourceUrl)));
}

/** 生成跨 URL/路径调用一致的规范化本地路径缓存键。 */
function sourceCacheKey(sourceUrl: URL): string {
  return fileURLToPath(sourceUrl);
}

/** 读取单个文件的原始文本，并按选项复用稳定源码缓存。 */
function readRawSource(sourceUrl: URL, cache: boolean): string {
  const cacheKey = sourceCacheKey(sourceUrl);
  if (cache) {
    const cached = RAW_SOURCE_CACHE.get(cacheKey);
    if (cached !== undefined) return cached;
  }

  const source = readFileSync(sourceUrl, "utf8");
  if (cache) RAW_SOURCE_CACHE.set(cacheKey, source);
  return source;
}

/** 递归展开本地 CSS 导入，并在进入缓存前阻断完整循环链。 */
function expandCssSource(
  entryUrl: URL,
  cache: boolean,
  ancestors: readonly string[],
): string {
  const cacheKey = sourceCacheKey(entryUrl);
  const cycleStart = ancestors.indexOf(cacheKey);
  if (cycleStart >= 0) {
    const cycle = [...ancestors.slice(cycleStart), cacheKey].join(" -> ");
    throw new Error(`检测到 CSS @import 循环: ${cycle}`);
  }
  if (cache) {
    const cached = EXPANDED_CSS_CACHE.get(cacheKey);
    if (cached !== undefined) return cached;
  }

  const nextAncestors = [...ancestors, cacheKey];
  const expanded = readRawSource(entryUrl, cache).replace(
    new RegExp(LOCAL_IMPORT_PATTERN.source, LOCAL_IMPORT_PATTERN.flags),
    (_statement, _quote: string, importPath: string) =>
      expandCssSource(
        normalizeSourceUrl(new URL(importPath, entryUrl)),
        cache,
        nextAncestors,
      ),
  );
  if (cache) EXPANDED_CSS_CACHE.set(cacheKey, expanded);
  return expanded;
}

/**
 * 统一读取真实源码；同一路径的 URL 与字符串调用共享缓存，CSS 默认展开本地导入。
 */
export function readSource(
  location: URL | string,
  options: ReadSourceOptions = {},
): string {
  const sourceUrl = normalizeSourceUrl(location);
  const cache = options.cache ?? true;
  const expandCssImports =
    options.expandCssImports ??
    fileURLToPath(sourceUrl).toLowerCase().endsWith(".css");
  return expandCssImports
    ? expandCssSource(sourceUrl, cache, [])
    : readRawSource(sourceUrl, cache);
}

/** 按入口声明顺序递归展开本地 CSS 导入，兼容既有 CSS 契约测试调用。 */
export function readCssSource(
  entry: URL | string,
  options: Omit<ReadSourceOptions, "expandCssImports"> = {},
): string {
  return readSource(entry, { ...options, expandCssImports: true });
}
