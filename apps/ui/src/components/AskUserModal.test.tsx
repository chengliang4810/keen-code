import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { AskUserPayload, AskUserQuestionItem } from "@/lib/session";
import { AskUserModal, buildAskUserAnswers } from "./AskUserModal";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const askUserLabels = {
  title: "需要确认",
  submit: "提交",
  next: "下一题",
  cancel: "取消",
  otherPlaceholder: "其他",
  freeTextHint: "自定义回答",
  multiHint: "可多选",
  close: "关闭",
};

describe("buildAskUserAnswers", () => {
  it("始终使用问题标识作为答案键", () => {
    const questions: AskUserQuestionItem[] = [
      {
        id: "deployment_target",
        question: "部署到哪里？",
        options: [
          { id: "local", label: "本机" },
          { id: "server", label: "服务器" },
        ],
      },
      {
        id: "release_note",
        question: "补充发布说明",
        options: [],
      },
      {
        id: "checks",
        question: "选择检查项",
        options: [
          { id: "lint,strict", label: "Lint, strict" },
          { id: "tests", label: "测试" },
        ],
        multiSelect: true,
      },
    ];

    expect(
      buildAskUserAnswers(
        questions,
        { deployment_target: ["server"], checks: ["lint,strict", "tests"] },
        { release_note: "今晚发布" },
      ),
    ).toEqual({
      deployment_target: "server",
      release_note: "今晚发布",
      checks: ["lint,strict", "tests"],
    });
  });

  it("标准问答始终保留导航、自由回答和提交操作", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./AskUserModal.tsx", import.meta.url)),
      "utf8",
    );
    expect(source).toContain('<div className="ask-user__nav">');
    expect(source).toContain("question.allowCustomAnswer !== false");
    expect(source).toContain('<footer className="ask-user__footer">');
  });
});

describe("AskUserModal 选项语义", () => {
  const payload = (multiSelect: boolean): AskUserPayload => ({
    rpcId: "rpc-1",
    sessionId: "session-1",
    questions: [
      {
        id: "target",
        question: "部署到哪里？",
        options: [
          { id: "local", label: "本机" },
          { id: "server", label: "服务器" },
        ],
        multiSelect,
      },
    ],
  });

  it("单选问题渲染 radiogroup 与 Appica radio 控件", () => {
    const html = renderToStaticMarkup(
      <AskUserModal
        payload={payload(false)}
        labels={askUserLabels}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(html).toContain('role="radiogroup"');
    expect(html).toContain('data-slot="radio"');
    expect(html).not.toContain("aria-pressed");
  });

  it("多选问题渲染 checkboxgroup 与 Appica checkbox 控件", () => {
    const html = renderToStaticMarkup(
      <AskUserModal
        payload={payload(true)}
        labels={askUserLabels}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(html).toContain('role="group"');
    expect(html).toContain('data-slot="checkbox"');
    expect(html).not.toContain("aria-pressed");
  });
});
