import assert from "node:assert/strict";
import { test } from "vitest";
import type { ZCodeProvider } from "@zcode/shared";
import { filterSkillsForProvider } from "../../../packages/ui/src/lib/skillSourceFilter.js";

test("本地 user/workspace Skill 不因物理路径脱离 zcode 目录而被过滤", () => {
  const skills = [
    {
      id: "native-user-fixture",
      path: "C:/isolated/data/skills/native-user-fixture/SKILL.md",
      scope: "user",
    },
    {
      id: "native-workspace-fixture",
      path: "C:/isolated/project/.agents/skills/native-workspace-fixture/SKILL.md",
      scope: "workspace",
    },
    {
      id: "glm:legacy",
      path: "C:/isolated/project/.zcode/skills/legacy/SKILL.md",
      scope: "unknown",
    },
    {
      id: "untrusted",
      path: "C:/isolated/other/SKILL.md",
      scope: "unknown",
    },
  ];

  assert.deepEqual(
    filterSkillsForProvider(skills, "zcode" as ZCodeProvider).map((skill) => skill.id),
    ["native-user-fixture", "native-workspace-fixture", "glm:legacy"],
  );
});
