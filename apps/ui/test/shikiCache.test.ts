import { beforeEach, expect, test, vi } from "vitest";

const probe = vi.hoisted(() => ({
  created: 0, tokenized: 0, disposed: 0, fail: false,
  counters: undefined as undefined | (() => Record<string, number>),
}));
vi.mock("@/logger.js", () => ({ logger: { error: vi.fn() } }));
vi.mock("@/lib/memoryDiagnostics.js", () => ({ uiMemoryDiagnosticsRegistry: {
  register: (_name: string, read: () => Record<string, number>) => { probe.counters = read; },
} }));
// UI 与根测试包可以解析到不同 Shiki 安装路径，按 UI 的实际入口 mock。
vi.mock("../../../packages/ui/node_modules/shiki/dist/index.mjs", () => ({
  bundledLanguages: Object.fromEntries(["rust", "typescript", "java", "python", "go", "c", "css", "html"].map((name) => [name, true])),
  bundledLanguagesInfo: [],
  createHighlighter: async ({ langs }: { langs: string[] }) => {
    probe.created += 1;
    if (probe.fail) { probe.fail = false; throw new Error("fixture failure"); }
    return { getLoadedLanguages: () => langs, dispose: () => { probe.disposed += 1; },
      codeToTokens: (content: string) => { probe.tokenized += 1; return { tokens: [[{ content }]] }; } };
  },
}));
beforeEach(() => { vi.resetModules(); Object.assign(probe, { created: 0, tokenized: 0, disposed: 0, fail: false }); });

test("相同内容合并在途高亮，首尾相同但中部不同的代码不会串结果", async () => {
  const { highlightCode } = await import("@/lib/shikiHighlighter.js");
  const code = `${"a".repeat(100)}let a=1;${"z".repeat(100)}`;
  const results: string[] = [];
  const done = (result: { tokens: { content: string }[][] }) => results.push(result.tokens[0][0].content);
  highlightCode(code, "rust", "github-light", (result) => done(result));
  highlightCode(code, "rust", "github-light", (result) => done(result));
  await vi.waitFor(() => expect(results).toEqual([code, code]));
  expect(probe.tokenized).toBe(1);
  const changed = code.replace("let a=1;", "let b=2;");
  await new Promise<void>((resolve) => highlightCode(changed, "rust", "github-light", (result) => {
    expect(result.tokens[0][0].content).toBe(changed); resolve();
  }));
});

test("大量内容和语言切换淘汰 token 与闲置高亮器", async () => {
  const { highlightCode } = await import("@/lib/shikiHighlighter.js");
  const languages = ["rust", "typescript", "java", "python", "go", "c", "css", "html"];
  for (let i = 0; i < 100; i++) {
    await new Promise<void>((resolve) => highlightCode(`${i}:${"x".repeat(20000)}`, languages[i % languages.length], "github-light", () => resolve()));
  }
  await vi.waitFor(() => expect(probe.counters?.().pending).toBe(0));
  expect(probe.counters?.().tokensCache).toBeLessThanOrEqual(64);
  expect(probe.counters?.().tokensBytes).toBeLessThanOrEqual(2 * 1024 * 1024);
  expect(probe.counters?.().highlighters).toBeLessThanOrEqual(4);
  expect(probe.disposed).toBeGreaterThan(0);
});

test("一次加载失败不会永久缓存 rejected promise", async () => {
  const { highlightCode } = await import("@/lib/shikiHighlighter.js");
  probe.fail = true;
  highlightCode("retry", "rust", "github-light");
  await vi.waitFor(() => expect(probe.counters?.().pending).toBe(0));
  await new Promise<void>((resolve) => highlightCode("retry", "rust", "github-light", () => resolve()));
  expect(probe.created).toBe(2);
});
