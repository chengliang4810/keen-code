import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { readSource } from "../test-utils/readCssSource";
import {
  buildComposerMentionRows,
  ComposerMentionPanel,
} from "./ComposerMentionPanel";
import type { ComposerMention } from "@/lib/composerMentions";

const groupLabels = {
  file: "文件",
  directory: "目录",
  session: "会话",
  plugin: "插件",
  skill: "技能",
} satisfies Record<ComposerMention["kind"], string>;

function mention(kind: ComposerMention["kind"], label: string): ComposerMention {
  return {
    id: `${kind}:${label}`,
    kind,
    label,
    value: label,
    markdown: label,
  };
}

const panelProps = {
  activeIndex: 0,
  onActiveIndexChange: () => {},
  onSelect: () => {},
  groupLabels,
  emptyLabel: "没有相符的引用",
  loadingLabel: "正在加载引用",
};

describe("ComposerMentionPanel", () => {
  it("按现有 ComposerMentionKind 分组且导航索引不包含 section header", () => {
    const rows = buildComposerMentionRows(
      [mention("file", "README.md"), mention("directory", "src")],
      groupLabels,
    );

    expect(rows).toEqual([
      { type: "section", id: "mention-section-file", label: "@ 文件" },
      { type: "entry", entry: expect.objectContaining({ kind: "file" }), navIndex: 0 },
      { type: "section", id: "mention-section-directory", label: "@ 目录" },
      { type: "entry", entry: expect.objectContaining({ kind: "directory" }), navIndex: 1 },
    ]);
  });

  it("表达 loading、empty 和 ready 状态，同时保留选中索引定位契约", () => {
    const loading = renderToString(
      <ComposerMentionPanel {...panelProps} entries={[]} loading />,
    );
    const empty = renderToString(
      <ComposerMentionPanel {...panelProps} entries={[]} loading={false} />,
    );
    const ready = renderToString(
      <ComposerMentionPanel
        {...panelProps}
        entries={[mention("file", "README.md")]}
      />,
    );

    expect(loading).toContain('data-state="loading"');
    expect(loading).toContain("正在加载引用");
    expect(empty).toContain('data-state="empty"');
    expect(empty).toContain("没有相符的引用");
    expect(ready).toContain('data-state="ready"');
    expect(ready).toContain('data-mention-index="0"');
  });

  it("选中项使用 nearest scrollIntoView，避免候选列表撑出视口", () => {
    const source = readSource(new URL("./ComposerMentionPanel.tsx", import.meta.url));
    expect(source).toContain("scrollIntoView");
    expect(source).toContain('block: "nearest"');
  });
});
