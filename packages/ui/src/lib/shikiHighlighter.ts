import type { BundledLanguage, BundledTheme, HighlighterGeneric, ThemedToken } from "shiki";
import { bundledLanguages, bundledLanguagesInfo, createHighlighter } from "shiki";
import { logger } from "@/logger.js";
import { uiMemoryDiagnosticsRegistry } from "@/lib/memoryDiagnostics.js";

export interface TokenizedCode {
  tokens: ThemedToken[][];
  fg: string;
  bg: string;
}

const bundledLanguageIds = new Set(Object.keys(bundledLanguages));
const bundledLanguageAliases = new Map(
  bundledLanguagesInfo.flatMap((info) =>
    (info.aliases ?? []).map((alias) => [alias, info.id] as const),
  ),
);
const FALLBACK_CODE_LANGUAGE: BundledLanguage = "log";
const PLAIN_TEXT_CODE_LANGUAGES = new Set([
  "",
  "text",
  "txt",
  "plain",
  "plaintext",
  "log",
  "output",
]);

export function shouldUseSyntaxHighlighting(language: string): boolean {
  const candidate = language.trim().toLowerCase();
  if (PLAIN_TEXT_CODE_LANGUAGES.has(candidate)) {
    return false;
  }

  return bundledLanguageIds.has(candidate) || bundledLanguageAliases.has(candidate);
}

function normalizeCodeLanguage(language: string): BundledLanguage {
  const candidate = language.trim().toLowerCase();
  if (!candidate) {
    return FALLBACK_CODE_LANGUAGE;
  }

  const alias = bundledLanguageAliases.get(candidate);
  if (alias && bundledLanguageIds.has(alias)) {
    return alias as BundledLanguage;
  }

  if (bundledLanguageIds.has(candidate)) {
    return candidate as BundledLanguage;
  }

  return FALLBACK_CODE_LANGUAGE;
}

type HighlighterLease = {
  promise: Promise<HighlighterGeneric<BundledLanguage, BundledTheme>>;
  active: number;
  retired: boolean;
};
const highlighterCache = new Map<string, HighlighterLease>();
const tokensCache = new Map<string, { value: TokenizedCode; bytes: number }>();
const pending = new Set<string>();
let tokensBytes = 0;
const MAX_TOKEN_BYTES = 2 * 1024 * 1024;
const MAX_TOKEN_ENTRIES = 64;
const MAX_HIGHLIGHTERS = 4;
const subscribers = new Map<string, Set<(result: TokenizedCode) => void>>();
// token 采用容量与估算字节双上限；高亮器淘汰需等待在途使用结束再 dispose。
uiMemoryDiagnosticsRegistry.register("shiki", () => ({
  tokensCache: tokensCache.size,
  highlighters: highlighterCache.size,
  tokensBytes,
  pending: pending.size,
}));

const getResolvedCodeTheme = (theme?: BundledTheme): BundledTheme => {
  if (theme) {
    return theme;
  }

  if (typeof document !== "undefined" && document.documentElement.classList.contains("dark")) {
    return "github-dark";
  }

  return "github-light";
};

const getCodeTokensCacheKey = (code: string, language: BundledLanguage, theme: BundledTheme) => {
  // 首尾相同但中部不同的代码不能命中同一结果。完整键的内存也计入缓存预算。
  return `${theme}:${language}:${code}`;
};
function disposeRetired(lease: HighlighterLease) {
  if (lease.retired && lease.active === 0) {
    void lease.promise.then((highlighter) => highlighter.dispose(), () => {});
  }
}
const getHighlighter = (
  language: BundledLanguage,
  theme: BundledTheme,
): HighlighterLease => {
  const cacheKey = `${theme}:${language}`;
  const cached = highlighterCache.get(cacheKey);
  if (cached) {
    highlighterCache.delete(cacheKey);
    highlighterCache.set(cacheKey, cached);
    cached.active += 1;
    return cached;
  }

  const highlighterPromise = createHighlighter({
    langs: [language],
    themes: [theme],
  });

  const lease = { promise: highlighterPromise, active: 1, retired: false };
  highlighterCache.set(cacheKey, lease);
  if (highlighterCache.size > MAX_HIGHLIGHTERS) {
    const oldest = highlighterCache.entries().next().value;
    if (oldest) {
      highlighterCache.delete(oldest[0]);
      oldest[1].retired = true;
      disposeRetired(oldest[1]);
    }
  }
  return lease;
};

const createRawCodeTokens = (code: string): TokenizedCode => ({
  bg: "transparent",
  fg: "inherit",
  tokens: code.split("\n").map((line) =>
    line === ""
      ? []
      : [
          {
            color: "inherit",
            content: line,
          } as ThemedToken,
        ],
  ),
});

// 带缓存的异步高亮入口；React 组件只应在 effect 中调用。
export const highlightCode = (
  code: string,
  language: string,
  theme?: BundledTheme,
  // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-callbacks)
  callback?: (result: TokenizedCode) => void,
): TokenizedCode | null => {
  if (!shouldUseSyntaxHighlighting(language)) {
    // 文本/日志代码块没有语法高亮收益，却会在聊天流式渲染和历史恢复时进入
    // Shiki 的异步状态机。之前修掉了 render 阶段 setState，但这条纯文本路径仍可能把
    // CodeViewer 拖进 React #185；这里直接返回 raw tokens，避免启动高亮副作用。
    return createRawCodeTokens(code);
  }

  const resolvedTheme = getResolvedCodeTheme(theme);
  const resolvedLanguage = normalizeCodeLanguage(language);
  const tokensCacheKey = getCodeTokensCacheKey(code, resolvedLanguage, resolvedTheme);

  const cached = tokensCache.get(tokensCacheKey);
  if (cached) {
    tokensCache.delete(tokensCacheKey);
    tokensCache.set(tokensCacheKey, cached);
    // 缓存命中时也需要通知 effect，但不能同步触发 setState。
    // 历史消息恢复时大量代码块会在同一次提交后挂载；同步 callback 会把 cache-hit 变成嵌套更新，
    // 和 Streamdown 的重渲染叠在一起时容易触发 React #185。推迟到微任务后再交给幂等 setter。
    if (callback) {
      queueMicrotask(() => callback(cached.value));
    }
    return cached.value;
  }

  if (callback) {
    if (!subscribers.has(tokensCacheKey)) {
      subscribers.set(tokensCacheKey, new Set());
    }
    subscribers.get(tokensCacheKey)?.add(callback);
  }

  // 多个预览等待同一内容时只 tokenize 一次，避免恢复历史时重复 CPU 工作。
  if (pending.has(tokensCacheKey)) return null;
  pending.add(tokensCacheKey);
  const lease = getHighlighter(resolvedLanguage, resolvedTheme);
  lease.promise
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then)
    .then((highlighter) => {
      const availableLangs = highlighter.getLoadedLanguages();
      const langToUse = availableLangs.includes(resolvedLanguage)
        ? resolvedLanguage
        : FALLBACK_CODE_LANGUAGE;

      const result = highlighter.codeToTokens(code, {
        lang: langToUse,
        theme: resolvedTheme,
      });

      const tokenized: TokenizedCode = {
        bg: "transparent",
        fg: result.fg ?? "inherit",
        tokens: result.tokens,
      };

      const bytes = tokensCacheKey.length * 2 + tokenized.tokens.reduce(
        (sum, line) => sum + 24 + line.reduce((total, token) => total + 96 + token.content.length * 2, 0), 0,
      );
      if (bytes <= MAX_TOKEN_BYTES) {
        tokensCache.set(tokensCacheKey, { value: tokenized, bytes });
        tokensBytes += bytes;
        while (tokensBytes > MAX_TOKEN_BYTES || tokensCache.size > MAX_TOKEN_ENTRIES) {
          const oldest = tokensCache.entries().next().value;
          if (!oldest) break;
          tokensBytes -= oldest[1].bytes;
          tokensCache.delete(oldest[0]);
        }
      }

      const subs = subscribers.get(tokensCacheKey);
      if (subs) {
        for (const sub of subs) {
          sub(tokenized);
        }
      }
      subscribers.delete(tokensCacheKey);
    })
    // oxlint-disable-next-line eslint-plugin-promise(prefer-await-to-then), eslint-plugin-promise(prefer-await-to-callbacks)
    .catch((error) => {
      // Shiki 加载或 tokenize 失败，组件会停留在无高亮的 rawTokens 状态。
      logger.error(
        `[ShikiHighlighter] 代码高亮失败: language=${resolvedLanguage}, theme=${resolvedTheme}`,
        error,
      );
      subscribers.delete(tokensCacheKey);
      const key = `${resolvedTheme}:${resolvedLanguage}`;
      if (highlighterCache.get(key) === lease) {
        highlighterCache.delete(key);
        lease.retired = true;
      }
    })
    .finally(() => {
      pending.delete(tokensCacheKey);
      lease.active -= 1;
      disposeRetired(lease);
    });

  return null;
};
