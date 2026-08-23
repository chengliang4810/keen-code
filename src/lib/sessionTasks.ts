/** Normalize ACP tool statuses for timeline rows. */
export type AgentTaskStatus =
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export function normalizeTaskStatus(
  status: string | null | undefined,
  streaming?: boolean,
): AgentTaskStatus {
  if (streaming) return "running";

  switch ((status || "").toLowerCase().trim()) {
    case "":
    case "in_progress":
    case "pending":
    case "running":
      return "running";
    case "failed":
    case "error":
    case "rejected":
      return "failed";
    case "cancelled":
    case "canceled":
      return "cancelled";
    case "completed":
    case "complete":
    case "done":
    case "success":
    default:
      return "completed";
  }
}
