import { expect, it } from "vitest";
import { LocalGitError } from "./gitError";
import { localizeUiError } from "./session";

it("本地 Git 错误保持可操作诊断，普通未知错误仍使用通用提示", () => {
  expect(localizeUiError(new LocalGitError("nothing to commit, working tree clean"), "zh")).toContain("nothing to commit");
  expect(localizeUiError(new LocalGitError("fatal: branch exists\n检测到同名分支 feature，已保留"), "zh")).toContain("已保留");
  expect(new LocalGitError("x".repeat(6000)).message.length).toBe(4096);
  expect(localizeUiError(new Error("ordinary unknown failure"), "zh")).not.toContain("ordinary unknown failure");
});
