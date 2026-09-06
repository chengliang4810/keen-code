import type { MessageToolSegment } from "./session";

/** 工具状态各展示入口共享的最小数据。 */
type ToolStatusFields = Pick<MessageToolSegment, "status" | "streaming" | "isError" | "completionStatus">;

/** 模型错误标记不等同于执行失败；主动取消以权威结果为准。 */
export function isToolSegmentCancelled(segment: ToolStatusFields): boolean {
  return segment.completionStatus === "cancelled";
}

/** 判断工具片段是否仍处于运行态；权威终态优先，其次才采用流式标记与协议状态。 */
export function isToolSegmentRunning(segment: ToolStatusFields): boolean {
  if (segment.completionStatus) return false;
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
export function isToolSegmentFailed(segment: ToolStatusFields): boolean {
  if (isToolSegmentCancelled(segment)) return false;
  if (segment.isError) return true;
  const status = (segment.status || "").toLowerCase();
  return (
    status === "failed" ||
    status === "error" ||
    status === "rejected" ||
    status === "denied"
  );
}
