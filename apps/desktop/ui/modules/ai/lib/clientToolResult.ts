export const CLIENT_TOOL_RESULT_MAX_BYTES = 1024 * 1024;

const encoder = new TextEncoder();

export function serializedResultBytes(value: unknown): number {
  return encoder.encode(JSON.stringify(value) ?? "null").byteLength;
}

export function boundClientToolResult(value: unknown): unknown {
  const serialized = JSON.stringify(value ?? null) ?? "null";
  const originalBytes = encoder.encode(serialized).byteLength;
  if (originalBytes <= CLIENT_TOOL_RESULT_MAX_BYTES) return JSON.parse(serialized);

  const preview = (length: number) => ({
    truncated: true,
    original_bytes: originalBytes,
    result_preview: serialized.slice(0, length),
    warning:
      "The serialized tool result exceeded 1 MiB. This preview is incomplete. Request fewer entries or lines; for bash_logs, continue with next_offset and has_more.",
  });
  let lower = 0;
  let upper = Math.min(serialized.length, CLIENT_TOOL_RESULT_MAX_BYTES);
  while (lower < upper) {
    const length = Math.ceil((lower + upper) / 2);
    if (serializedResultBytes(preview(length)) <= CLIENT_TOOL_RESULT_MAX_BYTES)
      lower = length;
    else upper = length - 1;
  }
  const last = serialized.charCodeAt(lower - 1);
  if (last >= 0xd800 && last <= 0xdbff) lower--;
  return preview(lower);
}
