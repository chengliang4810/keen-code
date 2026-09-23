/**
 * 聊天代码块的 Shiki 高亮器。
 *
 * 模式对齐 ZCode `packages/ui/src/lib/shikiHighlighter.ts`(Apache-2.0)的
 * 经验证做法,并针对 KeenCode 简化:
 * - `shiki` 经动态 import 进入独立异步 chunk:代码高亮只在消息 settle 后
 *   发生,不应占用首屏与冷启动预算;
 * - 单一 highlighter 实例,语言按需 `loadLanguage`;
 * - 双主题(github-light / github-dark)以 `defaultColor: false` 输出
 *   `--shiki-light/--shiki-dark` CSS 变量,明暗切换只走 CSS,不需要按主题
 *   重跑高亮;
 * - 纯文本/未知语言快速路径返回 null(调用方渲染原始行);
 * - 回调统一延后到 microtask:缓存命中时同步 setState 会与流式更新叠加,
 *   是 React #185 的已知诱因。
 */

import type {
  BundledLanguage,
  BundledTheme,
  HighlighterGeneric,
} from "shiki";

export interface ChatCodeToken {
  content: string;
  /** `--shiki-light`/`--shiki-dark` 等自定义属性,由 CSS 解析为最终颜色。 */
  style?: Record<string, string>;
}

export type ChatCodeLine = ChatCodeToken[];

const LIGHT_THEME: BundledTheme = "github-light";
const DARK_THEME: BundledTheme = "github-dark";
const PLAIN_LANGUAGES = new Set(["", "text", "plaintext", "txt"]);

/** 从围栏 className/文件名提取的原始语言拼写。 */
function rawLanguage(language: string): string {
  return language.trim().toLowerCase().replace(/^language-/, "");
}

/** 纯文本(无需异步加载 shiki 即可判定)的快速路径。 */
export function isPlainChatCodeLanguage(language: string): boolean {
  return PLAIN_LANGUAGES.has(rawLanguage(language));
}

let highlighterPromise: Promise<
  HighlighterGeneric<BundledLanguage, BundledTheme>
> | null = null;
const loadedLanguages = new Set<string>();
/** 按 语言+内容指纹 缓存;聊天消息文本稳定,数量有限,不做淘汰。 */
const linesCache = new Map<string, Promise<ChatCodeLine[] | null>>();

async function getHighlighter() {
  highlighterPromise ??= (async () => {
    const { createHighlighter } = await import("shiki");
    return createHighlighter({
      themes: [LIGHT_THEME, DARK_THEME],
      langs: [],
    });
  })();
  return highlighterPromise;
}

/** 解析 shiki 支持的语言(别名由 shiki 内部归一);未知语言返回 null → 纯文本。 */
async function resolveSupportedLanguage(
  language: string,
): Promise<string | null> {
  const raw = rawLanguage(language);
  if (!raw || PLAIN_LANGUAGES.has(raw)) return null;
  const { bundledLanguages, bundledLanguagesAlias } = await import("shiki");
  if (raw in bundledLanguages) return raw;
  if (raw in bundledLanguagesAlias) return raw;
  return null;
}

async function computeLines(
  code: string,
  language: string,
): Promise<ChatCodeLine[] | null> {
  try {
    const resolved = await resolveSupportedLanguage(language);
    if (!resolved) return null;
    const highlighter = await getHighlighter();
    if (!loadedLanguages.has(resolved)) {
      await highlighter.loadLanguage(resolved as BundledLanguage);
      loadedLanguages.add(resolved);
    }
    const result = highlighter.codeToTokens(code, {
      lang: resolved as BundledLanguage,
      themes: { light: LIGHT_THEME, dark: DARK_THEME },
      defaultColor: false,
    });
    return result.tokens.map((line) =>
      line.length === 0
        ? [{ content: "" }]
        : line.map((token) => ({
            content: token.content,
            style: token.htmlStyle as Record<string, string> | undefined,
          })),
    );
  } catch {
    // 未知语法或 wasm 加载失败:降级为纯文本,不打断消息渲染。
    return null;
  }
}

function tokenCacheKey(language: string, code: string): string {
  const head = code.slice(0, 100);
  const tail = code.length > 200 ? code.slice(-100) : "";
  return `${rawLanguage(language)}:${code.length}:${head}:${tail}`;
}

/**
 * 高亮代码为逐行 token 矩阵;`null` 表示按纯文本渲染。
 * 回调总是异步(microtask 或更晚),调用方从 effect 里 setState 即可。
 */
export function highlightChatCodeLines(
  code: string,
  language: string | undefined,
  callback: (lines: ChatCodeLine[] | null) => void,
): void {
  const lang = language ?? "";
  if (isPlainChatCodeLanguage(lang)) {
    queueMicrotask(() => callback(null));
    return;
  }
  const key = tokenCacheKey(lang, code);
  const cached = linesCache.get(key);
  if (cached) {
    queueMicrotask(() => {
      void cached.then((lines) => callback(lines));
    });
    return;
  }
  const promise = computeLines(code, lang);
  linesCache.set(key, promise);
  void promise.then((lines) => callback(lines));
}
