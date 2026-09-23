import { describe, expect, it } from "vitest";
import { normalizeSingleDollarMath } from "./dollarMathGuard";

describe("normalizeSingleDollarMath", () => {
  it("保留像公式的单美元内容", () => {
    expect(normalizeSingleDollarMath("能量 $E=mc^2$ 守恒")).toBe(
      "能量 $E=mc^2$ 守恒",
    );
    expect(normalizeSingleDollarMath("积分 $\\frac{1}{2}$")).toBe(
      "积分 $\\frac{1}{2}$",
    );
  });

  it("转义价格区间与环境变量文本", () => {
    // 两个 `$` 都会被转义:行内没有闭合符的 `$` 同样必须处理,否则会与
    // 下一行的 `$` 跨行误配成公式(remark-math 行内公式允许跨一个换行)。
    expect(normalizeSingleDollarMath("价格在 $5-$10 之间")).toBe(
      "价格在 \\$5-\\$10 之间",
    );
    expect(normalizeSingleDollarMath("$HOME 与 $PATH 不同")).toBe(
      "\\$HOME 与 \\$PATH 不同",
    );
  });

  it("转义 Windows 盘符与共享路径中的美元定界符", () => {
    expect(normalizeSingleDollarMath("复制到 C$\\out 目录")).toBe(
      "复制到 C\\$\\out 目录",
    );
    expect(normalizeSingleDollarMath("查看 \\\\filesrv\\share$\\dir 下的文件")).toBe(
      "查看 \\\\filesrv\\share\\$\\dir 下的文件",
    );
  });

  it("代码围栏与行内代码保持原文", () => {
    const fenced = "```\n$5-$10 与 $\\alpha$\n```";
    expect(normalizeSingleDollarMath(fenced)).toBe(fenced);
    const inline = "运行 `$HOME 与 $PATH` 对比";
    expect(normalizeSingleDollarMath(inline)).toBe(inline);
  });

  it("无美元符号时原样返回", () => {
    expect(normalizeSingleDollarMath("普通中文 with English")).toBe(
      "普通中文 with English",
    );
  });
});
