import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("应用退出保护", () => {
  it("拦截窗口关闭并明确说明任务和终端进程会中断", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain("onCloseRequested");
    expect(source).toContain("event.preventDefault()");
    expect(source).toContain("仍有 ${activeCount} 个任务正在运行");
    expect(source).toContain("退出会中断这些任务及其启动的终端进程");
    expect(source).toContain("手动输入“继续”");
    expect(source).toContain("停止任务并退出");
  });
});
