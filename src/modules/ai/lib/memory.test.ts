import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { memoryPrompt, memoryWorkspaceLabel } from "@/modules/ai/lib/memory";
import { buildMemoryTools } from "@/modules/ai/tools/memory";
import { applyToolPermissions } from "@/modules/ai/lib/permissions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("workspace memory execution boundaries", () => {
  it("uses registered project names without mixing Windows and Unix identities", () => {
    const projects = [
      { root: "D:/Projects/Repo/", name: "Renamed", env: { kind: "local" } },
    ];
    expect(
      memoryWorkspaceLabel(
        { root: "\\\\?\\d:\\projects\\repo", label: "repo" },
        projects,
      ),
    ).toBe("Renamed");
    expect(
      memoryWorkspaceLabel({ root: "/Projects/Repo", label: "Repo" }, [
        { root: "/projects/repo", name: "wrong", env: { kind: "local" } },
      ]),
    ).toBe("Repo");
    expect(
      memoryWorkspaceLabel({ root: "D:/Projects/Repo", label: "repo" }, [
        { ...projects[0], removed: true },
      ]),
    ).toBe("repo");
  });
  it("disabled memory contributes neither tools nor prompt", () => {
    expect(buildMemoryTools()).toEqual({});
    expect(memoryPrompt()).toBe("");
  });
  it("plan mode exposes only recall and normal mutations retain approval", () => {
    const tools = buildMemoryTools({ id: "project-a", index: "" });
    expect(
      Object.keys(applyToolPermissions(tools, "full-access", true)),
    ).toEqual(["memory_read"]);
    expect(applyToolPermissions(tools, "edit").memory_write.needsApproval).toBe(
      true,
    );
    expect(
      applyToolPermissions(tools, "edit").memory_delete.needsApproval,
    ).toBe(true);
  });
  it("uses the frozen project identity and preserves null versus empty snapshots", async () => {
    const tools = buildMemoryTools({ id: "project-a", index: "" });
    const options = { toolCallId: "call", messages: [] };
    await tools.memory_read.execute?.({}, options);
    expect(invoke).toHaveBeenLastCalledWith("agent_memory_read", {
      id: "project-a",
      name: null,
    });
    await tools.memory_write.execute?.(
      { name: "feedback.md", content: "fact", expected: null },
      options,
    );
    expect(invoke).toHaveBeenLastCalledWith("agent_memory_change", {
      id: "project-a",
      name: "feedback.md",
      content: "fact",
      expected: null,
    });
    await tools.memory_delete.execute?.(
      { name: "feedback.md", expected: "" },
      options,
    );
    expect(invoke).toHaveBeenLastCalledWith("agent_memory_change", {
      id: "project-a",
      name: "feedback.md",
      content: null,
      expected: "",
    });
    const abort = new AbortController();
    abort.abort();
    vi.mocked(invoke).mockClear();
    await expect(
      tools.memory_write.execute?.(
        { name: "feedback.md", content: "fact", expected: null },
        { ...options, abortSignal: abort.signal },
      ),
    ).rejects.toThrow();
    expect(invoke).not.toHaveBeenCalled();
  });
  it("bounds the auto-loaded index while keeping topic recall available", () => {
    const prompt = memoryPrompt({
      id: "project",
      index: Array.from({ length: 250 }, (_, i) => `entry-${i}`).join("\n"),
    });
    expect(prompt).toContain("entry-199");
    expect(prompt).not.toContain("entry-200");
    expect(
      memoryPrompt({ id: "project", index: "x".repeat(30000) }),
    ).not.toContain("x".repeat(24001));
  });
});
