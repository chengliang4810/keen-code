/** 前端归一化后的系统通知等级。 */
export type AcpSystemNotificationLevel = "info" | "warning" | "error";

/** ACP 标准 MCP Server 配置；SSE 当前由 KeenCode 明确拒绝。 */
export type AcpMcpServerConfig =
  | {
      /** Streamable HTTP 传输。 */
      type: "http";
      /** Session 内唯一 Server 名称。 */
      name: string;
      /** 完整 HTTP/HTTPS MCP 端点。 */
      url: string;
      /** 随请求发送的显式 Header。 */
      headers: Array<{ name: string; value: string; _meta?: Record<string, unknown> }>;
      /** ACP 保留元数据。 */
      _meta?: Record<string, unknown>;
    }
  | {
      /** Session 内唯一 Server 名称。 */
      name: string;
      /** stdio 可执行文件路径或命令名。 */
      command: string;
      /** 原样传给子进程的参数。 */
      args: string[];
      /** 显式覆盖的环境变量。 */
      env: Array<{ name: string; value: string; _meta?: Record<string, unknown> }>;
      /** ACP 保留元数据。 */
      _meta?: Record<string, unknown>;
    };

/** Session MCP Server 相对当前已发布延迟目录的状态。 */
export interface SessionMcpServerStatus {
  name: string;
  transport: "stdio" | "streamable_http";
  status: "pending" | "ready" | "pending_unload" | "failed";
  toolsCount: number;
  error?: string;
}

/** `keencode/session/mcp/status` 的完整 Session 独立目录快照。 */
export interface SessionMcpStatusResult {
  sessionId: string;
  catalogGeneration: number;
  servers: SessionMcpServerStatus[];
}

/** Session MCP load/unload 的幂等变更结果。 */
export interface SessionMcpMutationResult extends SessionMcpStatusResult {
  changed: boolean;
  deduplicated: boolean;
}

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
