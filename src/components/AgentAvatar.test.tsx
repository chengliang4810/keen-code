import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AgentAvatar } from "./AgentAvatar";

const identity = {
  nickname: { index: 0, generation: 1 },
  agentId: "agent-1",
  size: 24,
} as const;

describe("AgentAvatar", () => {
  it("只让运行状态使用内联 SVG 动画", () => {
    const running = renderToStaticMarkup(
      <AgentAvatar {...identity} status="running" />,
    );
    const done = renderToStaticMarkup(
      <AgentAvatar {...identity} status="done" />,
    );
    const failed = renderToStaticMarkup(
      <AgentAvatar {...identity} status="failed" />,
    );

    expect(running).toContain("<svg");
    expect(running).toContain("mo-always");
    expect(done).toContain("<img");
    expect(failed).toContain("<img");
    expect(done).not.toContain("mo-always");
    expect(failed).not.toContain("mo-always");
    expect(done).not.toBe(failed);
  });
});
