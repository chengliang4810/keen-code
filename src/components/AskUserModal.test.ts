import { describe, expect, it } from "vitest";
import type { AskUserQuestionItem } from "@/lib/session";
import { buildAskUserAnswers } from "./AskUserModal";

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
    ];

    expect(
      buildAskUserAnswers(
        questions,
        { deployment_target: ["server"] },
        { release_note: "今晚发布" },
      ),
    ).toEqual({
      deployment_target: "服务器",
      release_note: "今晚发布",
    });
  });
});
