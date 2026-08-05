/** ACP 结构化工具结果类型。

这些类型只描述数据形状，无运行时逻辑；peri tool_call_update 的
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

/** 把工具输出解析为结构化结果，字符串按 JSON 或纯文本处理。 */
export function parseToolResult(output: unknown): AcpStructuredToolResult {
  if (typeof output === "string") {
    const trimmed = output.trim();
    if (trimmed.startsWith("{")) {
      try {
        const parsed = JSON.parse(trimmed) as AcpStructuredToolResult;
        if (typeof parsed.output === "string") return parsed;
        return { output: trimmed };
      } catch {
        return { output: trimmed };
      }
    }
    return { output: trimmed };
  }
  if (output && typeof output === "object") {
    return output as AcpStructuredToolResult;
  }
  return { output: String(output ?? "") };
}
