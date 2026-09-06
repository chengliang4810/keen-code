/** 前端归一化后的系统通知等级。 */
export type AcpSystemNotificationLevel = "info" | "warning" | "error";

/** 当前 Session 的模型重试投影。 */
export interface AcpRetryProjection {
  /** 当前重试序号。 */
  attempt: number;
  /** 最大重试次数。 */
  maxAttempts: number;
  /** 下次重试前等待的毫秒数。 */
  delayMs: number;
  /** 供应商返回的重试原因。 */
  reason: string;
}

/** ACP 结构化工具结果类型。

这些类型只描述数据形状，无运行时逻辑；ACP tool_call_update 的
raw_output 可能直接是字符串，也可能是 JSON 对象。
 */

export type AcpFileOperation =
  | "created"
  | "modified"
  | "deleted"
  | "renamed"
  | "read"
  | "unknown";

export interface AcpArtifactReference {
  id: string;
  path?: string | null;
  media_type: string;
  size_bytes: number;
  sha256?: string | null;
}

export type AcpToolResultItem =
  | { type: "text"; text: string }
  | {
      type: "diff";
      path: string;
      patch: string;
      old_path?: string | null;
    }
  | {
      type: "file";
      path: string;
      operation: AcpFileOperation;
      size_bytes?: number | null;
      sha256?: string | null;
    }
  | {
      type: "command";
      command: string;
      exit_code?: number | null;
      stdout?: string;
      stderr?: string;
      duration_ms?: number | null;
    }
  | { type: "image"; media_type: string; data: string; label?: string | null }
  | { type: "artifact"; artifact: AcpArtifactReference };

export interface AcpStructuredToolResult {
  output: string;
  is_error?: boolean;
  truncated?: boolean;
  original_bytes?: number | null;
  items?: AcpToolResultItem[];
  artifact?: AcpArtifactReference | null;
  extensions?: Array<{ [key: string]: unknown }>;
}
