import { describe, expect, it } from "vitest";
import { readSource } from "@/test-utils/readCssSource";
import {
  applyUiFontSizeToDocument,
  DEFAULT_UI_FONT_SIZE,
  isUiFontSize,
  loadUiFontSize,
  MAX_UI_FONT_SIZE,
  MIN_UI_FONT_SIZE,
  normalizeUiFontSize,
  parseUiFontSize,
  saveUiFontSize,
  UI_FONT_SIZE_STORAGE_KEY,
  type UiFontSizeStorage,
} from "./uiFontSize";

function memoryStorage(initial: Record<string, string> = {}): UiFontSizeStorage & {
  data: Record<string, string>;
} {
  const data = { ...initial };
  return {
    data,
    getItem(key) {
      return key in data ? data[key]! : null;
    },
    setItem(key, value) {
      data[key] = value;
    },
  };
}

describe("界面字号偏好", () => {
  it("默认使用设计基线 14px", () => {
    expect(DEFAULT_UI_FONT_SIZE).toBe(14);
    expect(MIN_UI_FONT_SIZE).toBe(12);
    expect(MAX_UI_FONT_SIZE).toBe(20);
    expect(parseUiFontSize(null)).toBe(14);
    expect(loadUiFontSize(memoryStorage())).toBe(14);
  });

  it("接受范围内的整数像素值", () => {
    expect(parseUiFontSize("12")).toBe(12);
    expect(parseUiFontSize("14")).toBe(14);
    expect(parseUiFontSize("20")).toBe(20);
  });

  it("存量值无效时回退默认值而不阻断启动", () => {
    expect(loadUiFontSize(memoryStorage({ [UI_FONT_SIZE_STORAGE_KEY]: "99" }))).toBe(14);
    expect(loadUiFontSize(memoryStorage({ [UI_FONT_SIZE_STORAGE_KEY]: "big" }))).toBe(14);
    expect(loadUiFontSize(memoryStorage({ [UI_FONT_SIZE_STORAGE_KEY]: "13.5" }))).toBe(14);
    expect(loadUiFontSize(memoryStorage({ [UI_FONT_SIZE_STORAGE_KEY]: "18" }))).toBe(18);
  });

  it("拒绝越界、非整数与非法存储值", () => {
    expect(isUiFontSize(11)).toBe(false);
    expect(isUiFontSize(21)).toBe(false);
    expect(isUiFontSize(13.5)).toBe(false);
    expect(isUiFontSize("14")).toBe(false);
    expect(() => parseUiFontSize("11")).toThrow("界面字号格式无效");
    expect(() => parseUiFontSize("21")).toThrow("界面字号格式无效");
    expect(() => parseUiFontSize("14.5")).toThrow("界面字号格式无效");
    expect(() => parseUiFontSize("big")).toThrow("界面字号格式无效");
    expect(() => parseUiFontSize("")).toThrow("界面字号格式无效");
    expect(() => parseUiFontSize(undefined)).toThrow("界面字号格式无效");
  });

  it("写入与读取保持同一取值", () => {
    const storage = memoryStorage();
    saveUiFontSize(storage, 18);
    expect(storage.data[UI_FONT_SIZE_STORAGE_KEY]).toBe("18");
    expect(loadUiFontSize(storage)).toBe(18);
    expect(() => saveUiFontSize(storage, 21)).toThrow("界面字号格式无效");
    expect(() => saveUiFontSize(storage, 13.5)).toThrow("界面字号格式无效");
  });

  it("把取值收敛到允许范围并取整", () => {
    expect(normalizeUiFontSize(9)).toBe(12);
    expect(normalizeUiFontSize(30)).toBe(20);
    expect(normalizeUiFontSize(16.4)).toBe(16);
    expect(normalizeUiFontSize(Number.NaN)).toBe(14);
  });

  it("把字号写入根元素的 --ui-font-size", () => {
    const applied: Record<string, string> = {};
    const root = {
      style: {
        setProperty(name: string, value: string) {
          applied[name] = value;
        },
      },
    } as unknown as HTMLElement;
    applyUiFontSizeToDocument(17, root);
    expect(applied["--ui-font-size"]).toBe("17px");
    applyUiFontSizeToDocument(99, root);
    expect(applied["--ui-font-size"]).toBe("20px");
  });
});

describe("界面字号令牌接线", () => {
  const tokens = readSource(new URL("../styles/tokens.css", import.meta.url));

  it("文字令牌全部由 --ui-font-size 派生", () => {
    expect(tokens).toMatch(/--ui-font-size:\s*14px;/);
    expect(tokens).toMatch(
      /--ui-font-delta:\s*calc\(var\(--ui-font-size\)\s*-\s*14px\);/,
    );
    expect(tokens).toMatch(/--text-md:\s*var\(--ui-font-size\);/);
    for (const base of ["12", "13", "16", "28"]) {
      expect(tokens).toContain(`calc(${base}px + var(--ui-font-delta))`);
    }
  });

  it("对话正文字号接到界面字号而不是固定值", () => {
    const chatCss = readSource(
      new URL("../components/lobe-chat/lobe-chat.css", import.meta.url),
    );
    expect(chatCss).toMatch(/--chat-fs:\s*var\(--text-md\);/);
    expect(chatCss).toMatch(/--chat-prose-fs:\s*var\(--text-md\);/);
    expect(chatCss).toMatch(/--chat-fs-sm:\s*var\(--text-sm\);/);
    expect(chatCss).toMatch(/--chat-fs-xs:\s*var\(--text-xs\);/);
    // Harness 的 Markdown / 回合统计排版按该变量派生。
    expect(tokens).toMatch(/--dsh-content-font-size:\s*var\(--ui-font-size\);/);
  });

  it("侧栏项目名与对话名跟随界面字号", () => {
    const appCss = readSource(
      new URL("../styles/app-foundation.css", import.meta.url),
    );
    // 行首锚定，避免命中 `.sidebar__scroll .tree-l3` 等组合选择器。
    expect(appCss).toMatch(/^\.tree-l2 \{[^}]*font-size: var\(--text-md\);/m);
    expect(appCss).toMatch(/^\.tree-l3 \{[^}]*font-size: var\(--text-md\);/m);
  });

  it("图标字形尺寸保持固定，不随界面字号放大", () => {
    const appCss = readSource(
      new URL("../styles/app-foundation.css", import.meta.url),
    );
    // 图标规则块内不得出现随字号缩放的 calc。
    const block = appCss.match(/^\.nav-item__icon \{([^}]*)\}/m)?.[1] ?? "";
    expect(block).toContain("width: 16px;");
    expect(block).toContain("font-size: 16px;");
    expect(block).not.toContain("--ui-font-delta");
  });
});
