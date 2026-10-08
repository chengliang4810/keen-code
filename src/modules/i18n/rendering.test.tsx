import { AiToolApproval } from "@/modules/ai/components/AiToolApproval";
import { agentStepLabel } from "@/modules/ai/lib/agentStepLabel";
import type { ComponentProps } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import { applyLanguagePreference } from "./state";

afterEach(async () => {
  await applyLanguagePreference("en-US");
});

describe("translated AI display boundaries", () => {
  it("renders approval controls in both languages while preserving commands and paths", async () => {
    const part: ComponentProps<typeof AiToolApproval>["part"] = {
      type: "tool-bash_run",
      state: "approval-requested",
      toolCallId: "call-1",
      approval: { id: "approval-1" },
      input: { command: "echo Reading README.md", cwd: "D:/projects/Reading" },
    };
    const render = () =>
      renderToStaticMarkup(
        <AiToolApproval part={part} toolName="bash_run" onRespond={() => {}} />,
      );
    await applyLanguagePreference("zh-CN");
    const chinese = render();
    expect(chinese).toContain("批准");
    expect(chinese).toContain("拒绝");
    expect(chinese).toContain("echo Reading README.md");
    expect(chinese).toContain("D:/projects/Reading");
    await applyLanguagePreference("en-US");
    expect(render()).toContain("Approve");
    expect(part.input).toEqual({
      command: "echo Reading README.md",
      cwd: "D:/projects/Reading",
    });
  });

  it("localizes execution prefixes without rewriting their arguments", async () => {
    await applyLanguagePreference("zh-CN");
    expect(agentStepLabel("Running echo Reading README.md")).toBe(
      "正在运行 echo Reading README.md",
    );
    expect(agentStepLabel("Reading D:/projects/Running/file.ts")).toBe(
      "正在读取 D:/projects/Running/file.ts",
    );
    expect(agentStepLabel("Spawning Architect subagent")).toBe(
      "正在启动 Architect 子智能体",
    );
    expect(agentStepLabel("Updating plan (2 items)")).toBe(
      "正在更新计划（2 项）",
    );
    expect(agentStepLabel("Raw error: ENOENT")).toBe("Raw error: ENOENT");
  });
});
