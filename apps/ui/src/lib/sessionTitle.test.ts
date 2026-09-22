import { describe, expect, it } from "vitest";
import {
  buildSessionTitleFromFirstMessage,
  canGenerateAutomaticSessionTitle,
  extractDisplayTextFromUserMessage,
  extractFirstUserMessageText,
  isPlaceholderSessionTitle,
  markdownToReadableTitleText,
  sanitizeGeneratedSessionTitle,
} from "./sessionTitle";

describe("extractDisplayTextFromUserMessage", () => {
  it("移除草稿态 Skill 控制标记", () => {
    expect(
      extractDisplayTextFromUserMessage(
        "[[skill:frontend]] [[skill:test]]\n修复登录页",
      ),
    ).toBe("修复登录页");
  });

  it("移除发送态 Skill 首行但保留用户正文", () => {
    expect(
      extractDisplayTextFromUserMessage(
        "/frontend /test\n**修复** [登录页](https://example.com/login)",
      ),
    ).toBe("修复 登录页");
  });

  it("只包含控制标记时返回空文本", () => {
    expect(extractDisplayTextFromUserMessage("[[skill:test]]")).toBe("");
    expect(extractDisplayTextFromUserMessage("/frontend /test")).toBe("");
  });

  it("保留正文中的普通斜杠内容", () => {
    expect(
      extractDisplayTextFromUserMessage("请检查 /src/app.ts 和 https://a.test"),
    ).toBe("请检查 /src/app.ts 和 https://a.test");
  });

  it("折叠多行和多余空白", () => {
    expect(extractDisplayTextFromUserMessage("  第一行\n\n  第二行\t内容  ")).toBe(
      "第一行 第二行 内容",
    );
  });
});

describe("markdownToReadableTitleText", () => {
  it("将常用 Markdown 转为可读纯文本", () => {
    expect(
      markdownToReadableTitleText(
        "# 修复 **登录**\n- [x] 检查 `auth.ts`\n- 查看 ![截图](shot.png)",
      ),
    ).toBe("修复 登录 检查 auth.ts 查看 截图");
  });

  it("保留代码块内容并移除围栏", () => {
    expect(
      markdownToReadableTitleText("```ts\nconst ok = true;\n```"),
    ).toBe("const ok = true;");
  });

  it("解码常见实体并移除 HTML 标签", () => {
    expect(markdownToReadableTitleText("<b>A&amp;B</b>&nbsp;测试")).toBe(
      "A&B 测试",
    );
    expect(markdownToReadableTitleText("保留 &#99999999;")).toBe(
      "保留 &#99999999;",
    );
  });
});

describe("extractFirstUserMessageText", () => {
  it("跳过非用户消息和空控制消息", () => {
    expect(
      extractFirstUserMessageText([
        { role: "assistant", content: "不能作为标题" },
        { role: "user", content: "[[skill:test]]" },
        { role: "USER", content: "真正的 **首条内容**" },
        { role: "user", content: "后续内容" },
      ]),
    ).toBe("真正的 首条内容");
  });

  it("没有可用用户正文时返回空字符串", () => {
    expect(
      extractFirstUserMessageText([{ role: "assistant", content: "hello" }]),
    ).toBe("");
  });
});

describe("buildSessionTitleFromFirstMessage", () => {
  it("取首条用户消息的可读文本", () => {
    expect(
      buildSessionTitleFromFirstMessage([
        { role: "assistant", content: "不能作为标题" },
        { role: "user", content: "**帮我** [修复登录页](https://example.com)" },
      ]),
    ).toBe("帮我 修复登录页");
  });

  it("超长消息截断到 36 个字符", () => {
    expect(
      buildSessionTitleFromFirstMessage([
        { role: "user", content: "你好".repeat(30) },
      ]),
    ).toHaveLength(36);
  });

  it("没有可用用户消息时返回空字符串", () => {
    expect(buildSessionTitleFromFirstMessage([])).toBe("");
    expect(
      buildSessionTitleFromFirstMessage([
        { role: "user", content: "[[skill:test]]" },
      ]),
    ).toBe("");
  });
});

describe("sanitizeGeneratedSessionTitle", () => {
  it("移除 title 前缀、Markdown 和包围引号", () => {
    expect(
      sanitizeGeneratedSessionTitle('**Title:** "`修复登录重定向。`"'),
    ).toBe("修复登录重定向");
    expect(sanitizeGeneratedSessionTitle('"Title: Fix auth."')).toBe(
      "Fix auth",
    );
    expect(sanitizeGeneratedSessionTitle("标题：《修复登录重定向！》")).toBe(
      "修复登录重定向",
    );
  });

  it("移除尾部标点", () => {
    expect(sanitizeGeneratedSessionTitle("修复会话恢复……")).toBe(
      "修复会话恢复",
    );
    expect(sanitizeGeneratedSessionTitle("Fix session restore!!!")).toBe(
      "Fix session restore",
    );
  });

  it("最多保留 36 个 Unicode 字符且不切断 emoji", () => {
    const title = sanitizeGeneratedSessionTitle(`${"修".repeat(35)}😀继续`);
    expect(Array.from(title)).toHaveLength(36);
    expect(title.endsWith("😀")).toBe(true);
  });

  it("空候选返回空字符串", () => {
    expect(sanitizeGeneratedSessionTitle("``")).toBe("");
    expect(sanitizeGeneratedSessionTitle("标题：。。。")).toBe("");
  });
});

describe("isPlaceholderSessionTitle", () => {
  it("识别空值和默认中英文占位标题", () => {
    expect(isPlaceholderSessionTitle(null)).toBe(true);
    expect(isPlaceholderSessionTitle("  NEW   CHAT ")).toBe(true);
    expect(isPlaceholderSessionTitle("新任务")).toBe(true);
    expect(isPlaceholderSessionTitle("未命名")).toBe(true);
  });

  it("支持语言包额外占位值且不误判真实标题", () => {
    expect(isPlaceholderSessionTitle("Nouvelle tâche", ["Nouvelle tâche"])).toBe(
      true,
    );
    expect(isPlaceholderSessionTitle("新任务修复登录")).toBe(false);
    expect(isPlaceholderSessionTitle("Fix auth")).toBe(false);
  });
});

describe("canGenerateAutomaticSessionTitle", () => {
  it("只允许替换占位标题", () => {
    expect(
      canGenerateAutomaticSessionTitle({
        currentTitle: "新任务",
      }),
    ).toBe(true);
    expect(
      canGenerateAutomaticSessionTitle({
        currentTitle: "你好啊，你是谁。",
        titleSource: "automatic",
      }),
    ).toBe(false);
  });

  it("不覆盖手动标题或已经生成的语义化标题", () => {
    expect(
      canGenerateAutomaticSessionTitle({
        currentTitle: "我的自定义标题",
        titleSource: "manual",
      }),
    ).toBe(false);
    expect(
      canGenerateAutomaticSessionTitle({
        currentTitle: "询问助手身份",
        titleSource: "automatic",
      }),
    ).toBe(false);
  });

  it("消息前缀标题允许被自动短标题替换", () => {
    expect(
      canGenerateAutomaticSessionTitle({
        currentTitle: "帮我修复登录页的 bug",
        titleSource: "message-prefix",
      }),
    ).toBe(true);
  });
});
