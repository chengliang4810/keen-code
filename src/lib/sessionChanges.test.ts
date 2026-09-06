import { describe, expect, it } from "vitest";
import {
  buildUnifiedDiff,
  fileChangeForPath,
  filePathsMatch,
  isEditToolKind,
  normalizePath,
} from "./sessionChanges";

/** 构造带稳定原始行号的测试文本。 */
function numberedLines(count: number): string[] {
  return Array.from({ length: count }, (_, index) => `line-${index}\n`);
}

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

  it("preserves UNC and extended Windows prefixes", () => {
    expect(normalizePath("\\\\server\\share\\dir\\")).toBe(
      "//server/share/dir",
    );
    expect(normalizePath("\\\\?\\C:\\dir\\")).toBe("//?/C:/dir");
    expect(normalizePath("\\\\?\\UNC\\server\\share\\dir\\")).toBe(
      "//?/UNC/server/share/dir",
    );
  });

  it("does not rewrite a drive-relative path as an absolute path", () => {
    expect(normalizePath("C:dir\\file.ts")).toBe("C:dir/file.ts");
  });
});

describe("filePathsMatch", () => {
  it("compares Windows drive paths case-insensitively", () => {
    expect(
      filePathsMatch(
        "C:\\Repo\\src\\App.ts",
        "c:/repo/SRC/app.TS",
        "C:\\repo",
      ),
    ).toBe(true);
  });

  it("keeps POSIX path comparison case-sensitive", () => {
    expect(
      filePathsMatch(
        "/repo/src/App.ts",
        "/repo/src/app.ts",
        "/repo",
      ),
    ).toBe(false);
  });

  it("matches regular and extended UNC paths", () => {
    expect(
      filePathsMatch(
        "\\\\SERVER\\SHARE\\repo\\src\\App.ts",
        "//server/share/repo/src/app.ts",
        "\\\\server\\share\\repo",
      ),
    ).toBe(true);
    expect(
      filePathsMatch(
        "\\\\?\\UNC\\server\\share\\repo\\src\\App.ts",
        "\\\\server\\share\\repo\\src\\app.ts",
        "\\\\server\\share\\repo",
      ),
    ).toBe(true);
  });

  it("matches regular and extended drive paths", () => {
    expect(
      filePathsMatch(
        "\\\\?\\C:\\repo\\src\\App.ts",
        "c:/REPO/src/app.ts",
        "C:\\repo",
      ),
    ).toBe(true);
  });

  it("matches a project-relative path to its absolute path", () => {
    expect(
      filePathsMatch(
        "src/app.ts",
        "/workspace/repo/src/app.ts",
        "/workspace/repo",
      ),
    ).toBe(true);
  });

  it("rejects different files, roots, and similar prefixes", () => {
    expect(filePathsMatch("/repo/src/app.ts", "/repo/src/app.test.ts", "/repo")).toBe(
      false,
    );
    expect(filePathsMatch("/repo/src/app.ts", "/other/src/app.ts", "/repo")).toBe(
      false,
    );
    expect(filePathsMatch("/repo2/src/app.ts", "/repo/src/app.ts", "/repo")).toBe(
      false,
    );
    expect(filePathsMatch("C:/repo/src/app.ts", "D:/repo/src/app.ts", "C:/repo")).toBe(
      false,
    );
  });

  it("preserves POSIX spaces, backslashes, and unsupported drive-relative paths", () => {
    expect(filePathsMatch("/repo/file ", "/repo/file ", "/repo")).toBe(true);
    expect(filePathsMatch("/repo/file ", "/repo/file", "/repo")).toBe(false);
    expect(filePathsMatch("/repo/name\\part", "/repo/name/part", "/repo")).toBe(
      false,
    );
    expect(filePathsMatch("C:repo\\file.ts", "C:/repo/file.ts", "C:/repo")).toBe(
      false,
    );
  });

  it("returns no snapshot for null, empty, or an empty change list", () => {
    expect(filePathsMatch(null, "/repo/file.ts", "/repo")).toBe(false);
    expect(filePathsMatch("", "", "/repo")).toBe(false);
    expect(
      fileChangeForPath([], "/repo/file.ts", "/repo"),
    ).toBeUndefined();
    expect(
      fileChangeForPath(undefined, "/repo/file.ts", "/repo"),
    ).toBeUndefined();
  });

  it("does not alter null, empty, BOM, or CRLF snapshot content", () => {
    const created = {
      path: "/repo/new.txt",
      oldText: null,
      newText: "\uFEFFfirst\r\nsecond\r\n",
    };
    const empty = { path: "/repo/empty.txt", oldText: "", newText: "" };
    expect(fileChangeForPath([created], "/repo/new.txt", "/repo")).toBe(
      created,
    );
    expect(fileChangeForPath([empty], "/repo/empty.txt", "/repo")).toBe(
      empty,
    );
    expect(
      buildUnifiedDiff(
        created.path,
        created.oldText === null ? "" : created.oldText,
        created.newText,
      ),
    ).toContain("+\uFEFFfirst");
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

  it("large files keep a localized replacement visible within a context-sized diff", () => {
    const context = 3;
    const lineWidth = 4096;
    const fillerLine = `${"x".repeat(lineWidth - 1)}\n`;
    const before =
      "KC_MAX_SNAPSHOT_BEFORE\n" + fillerLine.repeat(16_384);
    const after = "KC_MAX_SNAPSHOT_AFTER_\n" + fillerLine.repeat(16_384);
    const diff = buildUnifiedDiff("large.txt", before, after, context);

    expect(diff).toContain("-KC_MAX_SNAPSHOT_BEFORE");
    expect(diff).toContain("+KC_MAX_SNAPSHOT_AFTER_");
    expect(diff.length).toBeLessThan((context * 2 + 8) * lineWidth);
  });

  it("large diffs retain an empty leading context line and its original hunk span", () => {
    const before = "\nold\n" + "x\n".repeat(1_500);
    const after = "\nnew\n" + "x\n".repeat(1_500);
    const diff = buildUnifiedDiff("blank.txt", before, after);

    expect(diff).toContain("@@ -1,5 +1,5 @@");
    expect(diff).toContain(" \n-old\n+new\n");
  });

  it("large-window diffs retain original hunk offsets in the middle and at EOF", () => {
    const beforeLines = numberedLines(1_500);
    const middleLines = [...beforeLines];
    middleLines[700] = "middle-replaced\n";
    const middleDiff = buildUnifiedDiff(
      "offsets.txt",
      beforeLines.join(""),
      middleLines.join(""),
    );
    expect(middleDiff).toContain("@@ -698,7 +698,7 @@");
    expect(middleDiff).toContain("-line-700\n+middle-replaced\n");

    const tailLines = [...beforeLines];
    tailLines[1_499] = "tail-replaced\n";
    const tailDiff = buildUnifiedDiff(
      "offsets.txt",
      beforeLines.join(""),
      tailLines.join(""),
    );
    expect(tailDiff).toContain("@@ -1497,4 +1497,4 @@");
    expect(tailDiff).toContain("-line-1499\n+tail-replaced\n");
  });

  it("large-window pure insertions and deletions keep zero-context coordinates", () => {
    const beforeLines = numberedLines(1_500);
    const insertedLines = [
      ...beforeLines.slice(0, 700),
      "inserted\n",
      ...beforeLines.slice(700),
    ];
    const insertionDiff = buildUnifiedDiff(
      "edit.txt",
      beforeLines.join(""),
      insertedLines.join(""),
      0,
    );
    expect(insertionDiff).toContain("@@ -700,0 +701,1 @@");
    expect(insertionDiff).toContain("+inserted\n");

    const deletedLines = beforeLines.filter((_, index) => index !== 700);
    const deletionDiff = buildUnifiedDiff(
      "edit.txt",
      beforeLines.join(""),
      deletedLines.join(""),
      0,
    );
    expect(deletionDiff).toContain("@@ -701,1 +700,0 @@");
    expect(deletionDiff).toContain("-line-700\n");
  });

  it("large-window diffs preserve BOM, CRLF, and a missing final LF", () => {
    const commonCrlf = "same\r\n".repeat(1_500);
    const crlfDiff = buildUnifiedDiff(
      "boundaries.txt",
      "\uFEFFbefore\r\n" + commonCrlf,
      "\uFEFFafter\r\n" + commonCrlf,
    );
    expect(crlfDiff).toContain("@@ -1,4 +1,4 @@");
    expect(crlfDiff).toContain("-\uFEFFbefore\r\n+\uFEFFafter\r\n");

    const commonLf = "same\n".repeat(1_499);
    const noFinalLfDiff = buildUnifiedDiff(
      "boundaries.txt",
      "head\n" + commonLf + "tail",
      "head\n" + commonLf + "tail-replaced",
    );
    expect(noFinalLfDiff).toContain("@@ -1498,4 +1498,4 @@");
    expect(noFinalLfDiff).toContain(
      "-tail\n\\ No newline at end of file\n+tail-replaced\n\\ No newline at end of file\n",
    );
  });

  it("large-window diffs keep separate hunk offsets for distant changes", () => {
    const beforeLines = numberedLines(1_500);
    const afterLines = [...beforeLines];
    afterLines[100] = "first-replaced\n";
    afterLines[1_200] = "second-replaced\n";
    const diff = buildUnifiedDiff(
      "separated.txt",
      beforeLines.join(""),
      afterLines.join(""),
    );

    expect(diff).toContain("@@ -98,7 +98,7 @@");
    expect(diff).toContain("@@ -1198,7 +1198,7 @@");
    expect(diff).toContain("-line-100\n+first-replaced\n");
    expect(diff).toContain("-line-1200\n+second-replaced\n");
  });
});
