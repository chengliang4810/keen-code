import { describe, expect, it } from "vitest";
import type { MessageToolSegment } from "./session";
import {
  isToolSegmentFailed,
  isToolSegmentRunning,
} from "./toolSegmentStatus";

/** 创建仅覆盖状态判定所需字段的工具片段。 */
function toolSegment(
  status: string,
  overrides: Partial<MessageToolSegment> = {},
): MessageToolSegment {
  return {
    kind: "tool",
    toolCallId: "tool-1",
    title: "Read",
    status,
    ...overrides,
  };
}

describe("isToolSegmentRunning", () => {
  it.each(["", "in_progress", "pending", "running", "RUNNING"])(
    "将状态 %s 判定为运行中",
    (status) => {
      expect(isToolSegmentRunning(toolSegment(status))).toBe(true);
    },
  );

  it.each(["completed", "success", "failed", "error", "denied"])(
    "将状态 %s 判定为非运行中",
    (status) => {
      expect(isToolSegmentRunning(toolSegment(status))).toBe(false);
    },
  );

  it("流式标记会覆盖终态状态", () => {
    expect(
      isToolSegmentRunning(toolSegment("completed", { streaming: true })),
    ).toBe(true);
  });
});

describe("isToolSegmentFailed", () => {
  it.each(["failed", "error", "rejected", "denied", "FAILED"])(
    "将状态 %s 判定为失败",
    (status) => {
      expect(isToolSegmentFailed(toolSegment(status))).toBe(true);
    },
  );

  it.each(["", "pending", "running", "completed", "success"])(
    "将状态 %s 判定为非失败",
    (status) => {
      expect(isToolSegmentFailed(toolSegment(status))).toBe(false);
    },
  );

  it("显式错误标记会覆盖成功状态", () => {
    expect(
      isToolSegmentFailed(toolSegment("completed", { isError: true })),
    ).toBe(true);
  });
});
