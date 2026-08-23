import { describe, expect, it } from "vitest";
import { normalizeTaskStatus } from "./sessionTasks";

describe("normalizeTaskStatus", () => {
  it("maps in-flight statuses and streaming rows to running", () => {
    expect(normalizeTaskStatus("in_progress")).toBe("running");
    expect(normalizeTaskStatus("pending")).toBe("running");
    expect(normalizeTaskStatus("running")).toBe("running");
    expect(normalizeTaskStatus("")).toBe("running");
    expect(normalizeTaskStatus(undefined, true)).toBe("running");
  });

  it("maps failed and cancelled statuses", () => {
    expect(normalizeTaskStatus("failed")).toBe("failed");
    expect(normalizeTaskStatus("error")).toBe("failed");
    expect(normalizeTaskStatus("rejected")).toBe("failed");
    expect(normalizeTaskStatus("cancelled")).toBe("cancelled");
    expect(normalizeTaskStatus("canceled")).toBe("cancelled");
  });

  it("maps successful and unknown terminal statuses to completed", () => {
    expect(normalizeTaskStatus("completed")).toBe("completed");
    expect(normalizeTaskStatus("complete")).toBe("completed");
    expect(normalizeTaskStatus("done")).toBe("completed");
    expect(normalizeTaskStatus("success")).toBe("completed");
    expect(normalizeTaskStatus("unexpected")).toBe("completed");
  });
});
