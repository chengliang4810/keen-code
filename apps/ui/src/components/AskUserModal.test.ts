import { describe, expect, it } from "vitest";
import type { AskUserQuestionItem } from "@/lib/session";
import { buildAskUserAnswers } from "./AskUserModal";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

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
