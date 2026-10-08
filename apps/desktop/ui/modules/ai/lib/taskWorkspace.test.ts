import { describe, expect, it } from "vitest";
import { isTaskRunning, resolveTaskWorkspace } from "./taskWorkspace";

describe("task workspace", () => {
  it("uses the prepared default directory for independent conversations, ignoring stale bindings", () => {
    expect(
      resolveTaskWorkspace(
        {
          projectless: true,
          workspaceRoot: "D:/stale-project",
          workspaceScope: "local",
        },
        "local",
        "D:/previous-project",
        "C:/Users/test/.rcode/chat/default",
      ),
    ).toBe("C:/Users/test/.rcode/chat/default");
  });
  it("does not inherit a project directory in an independent conversation", () => {
    expect(
      resolveTaskWorkspace(
        { projectless: true, workspaceScope: "local" },
        "local",
        "D:/previous-project",
      ),
    ).toBeNull();
    expect(() =>
      resolveTaskWorkspace(
        { projectless: true, workspaceScope: "local" },
        "wsl:Ubuntu",
        "/previous-project",
      ),
    ).toThrow("workspace environment");
  });
  it("keeps a bound project when a terminal changes directory", () => {
    expect(
      resolveTaskWorkspace(
        { workspaceRoot: "D:/project-a", workspaceScope: "local" },
        "local",
        "D:/project-b",
      ),
    ).toBe("D:/project-a");
  });
  it("refuses to run a task through another environment", () => {
    expect(() =>
      resolveTaskWorkspace(
        { workspaceRoot: "/home/project", workspaceScope: "wsl:Ubuntu" },
        "local",
        "D:/project",
      ),
    ).toThrow("workspace environment");
  });
  it("lets an old unbound conversation acquire the current project", () => {
    expect(resolveTaskWorkspace(undefined, "local", "D:/project")).toBe(
      "D:/project",
    );
  });
  it("treats approvals as an active run, but permits idle and failed navigation", () => {
    expect(isTaskRunning("awaiting-approval")).toBe(true);
    expect(isTaskRunning("thinking")).toBe(true);
    expect(isTaskRunning("idle")).toBe(false);
    expect(isTaskRunning("error")).toBe(false);
  });
});
