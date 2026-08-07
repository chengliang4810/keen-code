import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { PersonalizationSettingsPanel } from "./PersonalizationSettingsPanel";

describe("PersonalizationSettingsPanel", () => {
  it("展示全局自定义指令编辑区，并在内容未变时禁用保存", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value="使用中文回答"
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("自定义指令");
    expect(html).toContain("使用中文回答");
    expect(html).toContain("了解更多");
    expect(html).toContain("<textarea");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>保存<\/button>/);
    expect(html).toContain("启用本地记忆");
  });
});
