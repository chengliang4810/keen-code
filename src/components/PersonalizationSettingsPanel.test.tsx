import { readFileSync } from "node:fs";
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
        memoryStatus={null}
        memoryStatusLoading={false}
        memoryStatusError={false}
        onRefreshMemoryStatus={vi.fn().mockResolvedValue(undefined)}
        onRevealMemoryRoot={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("自定义指令");
    expect(html).toContain("使用中文回答");
    expect(html).toContain("了解更多");
    expect(html).not.toContain("~/.keencode/AGENTS.md");
    expect(html).toContain("新建或重新加载的对话生效");
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
        memoryStatus={null}
        memoryStatusLoading={false}
        memoryStatusError={false}
        onRefreshMemoryStatus={vi.fn().mockResolvedValue(undefined)}
        onRevealMemoryRoot={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("启用本地记忆");
    expect(html).not.toContain("长期记忆");
    expect(html).not.toContain("# 长期记忆");
  });

  it("展示本机记忆状态、条目数、抽取状态和根目录操作", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value=""
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        memoryFile=""
        onMemoryFileSave={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
        memoryStatus={{
          enabled: true,
          root: "C:\\Users\\test\\.keencode\\memories",
          memoryCount: 3,
          running: true,
        }}
        memoryStatusLoading={false}
        memoryStatusError={false}
        onRefreshMemoryStatus={vi.fn().mockResolvedValue(undefined)}
        onRevealMemoryRoot={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("记忆状态");
    expect(html).toContain("已启用");
    expect(html).toContain("记忆条目数");
    expect(html).toContain(">3</div>");
    expect(html).toContain("抽取状态");
    expect(html).toContain("运行中");
    expect(html).toContain("记忆根目录");
    expect(html).toContain("C:\\Users\\test\\.keencode\\memories");
    expect(html).toContain("查看目录");
    expect(html).toContain("复制路径");
    expect(html).toContain(
      "settings-card settings-personalization__memory-status",
    );
    expect(html).toContain("settings-row settings-row--stack");
    expect(html).toContain('aria-busy="false"');
  });

  it("每次进入个性化面板时按需刷新状态且不建立轮询", () => {
    const source = readFileSync(
      new URL("./PersonalizationSettingsPanel.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("void onRefreshMemoryStatus().catch(() => {});");
    expect(source).toContain("[onRefreshMemoryStatus]");
    expect(source).toContain("aria-busy={memoryStatusAction !== null}");
    expect(source).toContain("memoryStatusActionError");
    expect(source).toContain("memoryStatusRootRevealed");
    expect(source).not.toContain("setInterval");
  });

  it("刷新期间保留上一次可用状态，避免状态卡片短暂空白", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value=""
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        memoryFile=""
        onMemoryFileSave={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
        memoryStatus={{
          enabled: true,
          root: "C:\\Users\\test\\.keencode\\memories",
          memoryCount: 4,
          running: false,
        }}
        memoryStatusLoading
        memoryStatusError={false}
        onRefreshMemoryStatus={vi.fn().mockResolvedValue(undefined)}
        onRevealMemoryRoot={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("正在加载记忆状态");
    expect(html).toContain("记忆条目数");
    expect(html).toContain(">4</div>");
    expect(html).toContain("空闲");
    expect(html).toContain('aria-busy="true"');
  });

  it("刷新失败时保留状态快照并显示明确的降级提示", () => {
    const html = renderToString(
      <PersonalizationSettingsPanel
        value=""
        locale="zh"
        onSave={vi.fn().mockResolvedValue(undefined)}
        localMemories
        onLocalMemoriesChange={vi.fn().mockResolvedValue(undefined)}
        memoryFile=""
        onMemoryFileSave={vi.fn().mockResolvedValue(undefined)}
        onMemoriesReset={vi.fn().mockResolvedValue(undefined)}
        memoryStatus={{
          enabled: true,
          root: "C:\\Users\\test\\.keencode\\memories",
          memoryCount: 5,
          running: true,
        }}
        memoryStatusLoading={false}
        memoryStatusError
        onRefreshMemoryStatus={vi.fn().mockResolvedValue(undefined)}
        onRevealMemoryRoot={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(html).toContain("暂时无法刷新记忆状态");
    expect(html).toContain("记忆条目数");
    expect(html).toContain(">5</div>");
    expect(html).toContain("运行中");
  });
});
