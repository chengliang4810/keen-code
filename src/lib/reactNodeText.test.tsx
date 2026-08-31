import { createElement } from "react";
import { describe, expect, it } from "vitest";
import { reactNodeText } from "./reactNodeText";

describe("reactNodeText", () => {
  it.each([
    [null, ""],
    [false, ""],
    [true, ""],
    ["文本", "文本"],
    [42, "42"],
    [12n, "12"],
  ])("提取基础节点 %#", (node, expected) => {
    expect(reactNodeText(node)).toBe(expected);
  });

  it("递归连接嵌套数组和 React 元素子节点", () => {
    const node = [
      "打开 ",
      createElement(
        "strong",
        null,
        "src/",
        createElement("code", null, "main.ts"),
      ),
      [" 第 ", 2, " 行"],
    ];

    expect(reactNodeText(node)).toBe("打开 src/main.ts 第 2 行");
  });

  it("忽略没有文本子节点的 React 元素", () => {
    expect(reactNodeText(createElement("img", { alt: "替代文本" }))).toBe("");
  });
});
