import { describe, expect, it } from "vitest";
import { isAbsoluteFsPath, pathBasename } from "./filePath";

describe("isAbsoluteFsPath", () => {
  it.each([
    ["/usr/local/bin", true],
    ["C:\\Users\\dev\\main.ts", true],
    ["d:/projects/app", true],
    ["\\\\server\\share\\file.txt", true],
    ["//server/share/file.txt", true],
    ["C:relative\\file.txt", false],
    ["src/main.ts", false],
    ["./src/main.ts", false],
    ["../src/main.ts", false],
    ["https://example.com/file.ts", false],
    ["file:///C:/Users/dev/main.ts", false],
    ["", false],
  ])("判断路径 %s 的绝对路径结果为 %s", (path, expected) => {
    expect(isAbsoluteFsPath(path)).toBe(expected);
  });
});

describe("pathBasename", () => {
  it.each([
    ["/usr/local/bin/tool", "tool"],
    ["/usr/local/bin/tool/", "tool"],
    ["C:\\Users\\dev\\main.ts", "main.ts"],
    ["C:\\Users\\dev\\folder\\", "folder"],
    ["\\\\server\\share\\folder\\file.txt", "file.txt"],
    ["\\\\server\\share\\folder\\", "folder"],
    ["main.ts", "main.ts"],
    ["", ""],
  ])("提取路径 %s 的文件名为 %s", (path, expected) => {
    expect(pathBasename(path)).toBe(expected);
  });
});
