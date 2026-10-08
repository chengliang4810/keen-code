import { describe, expect, it } from "vitest";
import {
  boundClientToolResult,
  CLIENT_TOOL_RESULT_MAX_BYTES,
  serializedResultBytes,
} from "@/modules/ai/lib/clientToolResult";

describe("client tool serialized UTF-8 budget", () => {
  it("preserves serializable small results and normalizes undefined to null", () => {
    const result = { bytes: "日志\n", next_offset: 7, has_more: false };
    expect(boundClientToolResult(result)).toEqual(result);
    expect(boundClientToolResult(undefined)).toBeNull();
  });

  it("accepts exactly the IPC budget", () => {
    const value = "a".repeat(CLIENT_TOOL_RESULT_MAX_BYTES - 2);
    expect(serializedResultBytes(value)).toBe(CLIENT_TOOL_RESULT_MAX_BYTES);
    expect(boundClientToolResult(value)).toBe(value);
  });

  it.each([
    { name: "ASCII", character: "a" },
    { name: "multibyte text", character: "界" },
    { name: "surrogate pairs", character: "\ud83d\ude00" },
    { name: "control characters", character: "\u0000" },
    { name: "escaped quotes and newlines", character: '"\\\n' },
  ])(
    "returns an explicitly incomplete preview within budget for $name",
    ({ character }) => {
      const value = { output: character.repeat(CLIENT_TOOL_RESULT_MAX_BYTES) };
      const result = boundClientToolResult(value) as {
        truncated: boolean;
        original_bytes: number;
        result_preview: string;
        warning: string;
      };
      expect(result.truncated).toBe(true);
      expect(result.original_bytes).toBe(serializedResultBytes(value));
      expect(result.result_preview.length).toBeGreaterThan(0);
      expect(result.warning).toContain("incomplete");
      expect(serializedResultBytes(result)).toBeLessThanOrEqual(
        CLIENT_TOOL_RESULT_MAX_BYTES,
      );
      expect(result.result_preview).toBe(
        JSON.stringify(value).slice(0, result.result_preview.length),
      );
      const last = result.result_preview.charCodeAt(
        result.result_preview.length - 1,
      );
      expect(last < 0xd800 || last > 0xdbff).toBe(true);
    },
  );
});
