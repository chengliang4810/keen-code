import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PersonalizationSettingsPanel } from "./PersonalizationSettingsPanel";

describe("PersonalizationSettingsPanel", () => {
  it("展示全局自定义指令编辑区，不显示保存按钮", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value="使用中文回答"
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        memoryFile="# 长期记忆"
        onMemoryFileSave={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("自定义指令");
    expect(html).toContain("使用中文回答");
    expect(html).toContain("了解更多");
    expect(html).not.toContain("~/.keencode/AGENTS.md");
    expect(html).toContain("保存后从下一轮对话生效，无需重启");
    expect(html).toContain("<textarea");
    expect(html).not.toContain(">保存</button>");
    expect(html).toContain("启用本地记忆");
    expect(html).toContain("# 长期记忆");
    expect(html).toContain('aria-label="长期记忆"');
    expect(html).not.toContain("编辑 MEMORY.md 中的已整合长期记忆。");
  });

  it("未启用本地记忆时隐藏长期记忆文本域", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value=""
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories={false}
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        memoryFile="# 长期记忆"
        onMemoryFileSave={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("启用本地记忆");
    expect(html).not.toContain("长期记忆");
    expect(html).not.toContain("# 长期记忆");
  });
});
