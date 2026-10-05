import { describe, expect, it } from "vitest";
import { buildSkillReferenceCatalogParams } from "../src/hooks/useSkills.js";

describe("Skill 引用目录请求 authority", () => {
  it("草稿不把预热会话 ID 发送给冻结目录接口", () => {
    expect(
      buildSkillReferenceCatalogParams(
        {
          workspacePath: "C:/workspace/project",
          workspaceIdentity: "project",
          sessionId: null,
        },
        undefined,
      ),
    ).toEqual({
      workspacePath: "C:/workspace/project",
      workspaceIdentity: "project",
    });
  });

  it("已有会话仍携带 sessionId 并保留 remote attachment", () => {
    expect(
      buildSkillReferenceCatalogParams(
        {
          workspacePath: "C:/workspace/project",
          workspaceIdentity: "project",
          sessionId: "session-1",
        },
        "remote-1",
      ),
    ).toEqual({
      workspacePath: "C:/workspace/project",
      workspaceIdentity: "project",
      remoteSessionId: "remote-1",
      sessionId: "session-1",
    });
  });
});
