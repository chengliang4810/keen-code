import { describe, expect, it } from "vitest";
import {
  AGENT_NICKNAMES,
  agentNicknameLabel,
  agentNicknameSeed,
} from "./agentNicknames";

describe("Agent 昵称", () => {
  it("三种语言均有 128 个不重复名称", () => {
    expect(AGENT_NICKNAMES).toHaveLength(128);
    for (const locale of ["zh", "zh-TW", "en"] as const) {
      expect(new Set(AGENT_NICKNAMES.map((entry) => entry[locale])).size).toBe(
        128,
      );
    }
  });

  it("语言切换不改变头像种子，名字池耗尽后添加代数", () => {
    const nickname = { index: 0, generation: 2 };
    expect(agentNicknameSeed(nickname)).toBe("keencode-agent-nickname:0:2");
    expect(agentNicknameLabel(nickname, "zh")).toBe("孔子 #2");
    expect(agentNicknameLabel(nickname, "en")).toBe("Confucius #2");
  });
});
