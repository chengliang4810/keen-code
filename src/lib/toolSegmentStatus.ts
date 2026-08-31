import type { MessageToolSegment } from "./session";

/** 判断工具片段是否仍处于运行态；流式标记优先于协议状态。 */
export function isToolSegmentRunning(segment: MessageToolSegment): boolean {
  if (segment.streaming) return true;
  const status = (segment.status || "").toLowerCase();
  return (
    status === "in_progress" ||
    status === "pending" ||
    status === "running" ||
    status === ""
  );
}

/** 判断工具片段是否处于失败态；显式错误标记优先于协议状态。 */
export function isToolSegmentFailed(segment: MessageToolSegment): boolean {
  if (segment.isError) return true;
  const status = (segment.status || "").toLowerCase();
  return (
    status === "failed" ||
    status === "error" ||
    status === "rejected" ||
    status === "denied"
  );
}
