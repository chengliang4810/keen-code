import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "vitest";
import { sessionDebugSnapshotSchema } from "../../../packages/shared/src/session-debug.ts";
import { zcodeSessionSubagentsResultSchema } from "../../../packages/shared/src/zcode-protocol/index.ts";

// Rust 实际序列化的 Journal 投影夹具；原始严格 schema 持续保护两端字段和状态语义。
const fixture = JSON.parse(readFileSync(resolve(import.meta.dirname,
  "../../../tooling/native-live/workflow-contract-fixtures/agent_views_contract.json"), "utf8"));

test("真实 Rust 调试投影遵守 source schema，TPS 使用 Adapter 的实测生成耗时", () => {
  const debug = sessionDebugSnapshotSchema.parse(fixture.debug);
  expect(debug.rounds[0]?.tokensPerSecond).toBe(20);
  expect(debug.rounds[0]?.generationDurationMs).toBe(2000);
  expect(debug.cache?.hitRate).toBe(0.25);
});

test("普通子 Agent 目录使用父 Journal 的稳定只读身份", () => {
  const directory = zcodeSessionSubagentsResultSchema.parse(fixture.subagents);
  expect(directory.ended.items[0]?.childSessionId).toBe("agent:view-session:child-01");
  expect(directory.ended.items[0]?.status).toBe("success");
  expect(directory.childSessionIds).toEqual(["agent:view-session:child-01"]);
});
