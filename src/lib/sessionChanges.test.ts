import { describe, expect, it } from "vitest";
import {
  buildUnifiedDiff,
  isEditToolKind,
  normalizePath,
} from "./sessionChanges";

describe("normalizePath", () => {
  it("unifies separators and strips trailing slash", () => {
    expect(normalizePath("a\\b\\c\\")).toBe("a/b/c");
    expect(normalizePath("/tmp/foo/")).toBe("/tmp/foo");
    expect(normalizePath("/")).toBe("/");
  });

  it("collapses duplicate slashes", () => {
    expect(normalizePath("/tmp//foo///bar")).toBe("/tmp/foo/bar");
  });

  it("trims whitespace", () => {
    expect(normalizePath("  /x/y  ")).toBe("/x/y");
  });
});

describe("isEditToolKind", () => {
  it("recognizes current file mutation tools", () => {
    expect(isEditToolKind("write")).toBe(true);
    expect(isEditToolKind("edit")).toBe(true);
    expect(isEditToolKind("folder_operations")).toBe(true);
    expect(isEditToolKind("Write")).toBe(true);
  });

  it("rejects read / search / shell", () => {
    expect(isEditToolKind("read")).toBe(false);
    expect(isEditToolKind("bash")).toBe(false);
    expect(isEditToolKind("grep")).toBe(false);
    expect(isEditToolKind("")).toBe(false);
  });
});

describe("buildUnifiedDiff", () => {
  it("produces unified headers and +/- lines", () => {
    const d = buildUnifiedDiff(
      "a.ts",
      "line1\nline2\nline3\n",
      "line1\nline2-changed\nline3\n",
    );
    expect(d).toContain("--- a/a.ts");
    expect(d).toContain("+++ b/a.ts");
    expect(d).toContain("-line2");
    expect(d).toContain("+line2-changed");
  });
});
