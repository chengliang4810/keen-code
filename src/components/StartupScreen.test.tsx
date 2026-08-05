import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { StartupScreen } from "./StartupScreen";

describe("StartupScreen", () => {
  it("只展示 KeenCode 产品信息，不暴露内部实现", () => {
    const html = renderToString(
      <StartupScreen useCustomWindowChrome={false} />,
    );

    expect(html).toContain("KeenCode");
    expect(html).toContain("一款轻量、本地优先的桌面 AI 编码工具。");
    expect(html).not.toMatch(/peri|ACP|Backend|Session|日志|状态/i);
  });

  it("为自绘窗口标题栏保留拖拽区域", () => {
    const html = renderToString(
      <StartupScreen useCustomWindowChrome />,
    );

    expect(html).toContain("setup-gate--custom-chrome");
    expect(html).toContain("data-tauri-drag-region");
  });
});
