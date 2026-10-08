import { describe, expect, it, vi } from "vitest";
import {
  createProjectDirectoryDropTarget,
  projectNameForDirectory,
  resolveProjectDirectory,
} from "@/modules/ai/lib/projectDirectory";

describe("project directory form", () => {
  it("uses the selected folder name for an empty or whitespace name on each platform", () => {
    expect(
      projectNameForDirectory("", "D:\\projects\\中文项目\\", "Project"),
    ).toBe("中文项目");
    expect(
      projectNameForDirectory("  ", "/home/user/new-project/", "Project"),
    ).toBe("new-project");
    expect(
      projectNameForDirectory("", "//server/share/project", "Project"),
    ).toBe("project");
    expect(projectNameForDirectory("", "/", "Project")).toBe("Project");
  });
  it("retains the user's name when replacing a directory, but infers again when cleared", () => {
    expect(projectNameForDirectory("  自定义名称  ", "D:/old", "Project")).toBe(
      "自定义名称",
    );
    expect(projectNameForDirectory("  自定义名称  ", "D:/new", "Project")).toBe(
      "自定义名称",
    );
    expect(projectNameForDirectory("", "D:/new", "Project")).toBe("new");
  });
  it("resolves directory links before validating the target and accepts external project folders", async () => {
    const access = {
      canonicalize: vi.fn().mockResolvedValue("D:/external/project"),
      stat: vi.fn().mockResolvedValue({ kind: "dir" }),
    };
    expect(await resolveProjectDirectory(["D:/linked-project"], access)).toBe(
      "D:/external/project",
    );
    expect(access.stat).toHaveBeenCalledWith("D:/external/project");
  });
  it("refuses files, missing folders, relative paths and multiple drops", async () => {
    const access = {
      canonicalize: vi.fn().mockResolvedValue("D:/file.txt"),
      stat: vi.fn().mockResolvedValue({ kind: "file" }),
    };
    await expect(
      resolveProjectDirectory(["D:/file.txt"], access),
    ).rejects.toThrow("instead of a file");
    access.canonicalize.mockRejectedValue(new Error("not found"));
    await expect(
      resolveProjectDirectory(["D:/missing"], access),
    ).rejects.toThrow("not found");
    access.canonicalize.mockClear();
    await expect(
      resolveProjectDirectory(["relative/folder"], access),
    ).rejects.toThrow("absolute");
    await expect(
      resolveProjectDirectory(["D:/one", "D:/two"], access),
    ).rejects.toThrow("one folder");
    await expect(resolveProjectDirectory([], access)).rejects.toThrow(
      "one folder",
    );
    expect(access.canonicalize).not.toHaveBeenCalled();
  });
  it("only accepts OS drops inside the form, including scaled displays", () => {
    const onDrop = vi.fn();
    const onHover = vi.fn();
    const handle = createProjectDirectoryDropTarget({
      contains: (x, y) => x >= 100 && x < 200 && y >= 50 && y < 100,
      pixelRatio: () => 2,
      onDrop,
      onHover,
    });
    handle({ type: "enter", position: { x: 250, y: 150 } });
    expect(onHover).toHaveBeenLastCalledWith(true);
    handle({
      type: "drop",
      position: { x: 250, y: 150 },
      paths: ["D:/project"],
    });
    expect(onDrop).toHaveBeenCalledWith(["D:/project"]);
    expect(onHover).toHaveBeenLastCalledWith(false);
    handle({
      type: "drop",
      position: { x: 410, y: 150 },
      paths: ["D:/another"],
    });
    expect(onDrop).toHaveBeenCalledTimes(1);
    handle({ type: "leave" });
    expect(onHover).toHaveBeenLastCalledWith(false);
  });
});
