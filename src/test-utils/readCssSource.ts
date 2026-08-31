import { readFileSync } from "node:fs";

const LOCAL_IMPORT_PATTERN = /@import\s+(?:url\()?(["'])(\.\.?\/[^"']+)\1\)?\s*;/g;

/** 按入口声明顺序递归展开本地 CSS 导入，供源码契约测试验证最终层叠内容。 */
export function readCssSource(entryUrl: URL): string {
  const source = readFileSync(entryUrl, "utf8");
  return source.replace(
    LOCAL_IMPORT_PATTERN,
    (_statement, _quote: string, importPath: string) =>
      readCssSource(new URL(importPath, entryUrl)),
  );
}
