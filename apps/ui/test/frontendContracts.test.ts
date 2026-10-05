import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  conversationRowSchema,
  conversationDeltaSchema,
  conversationSnapshotSchema,
  apiRetryStateSchema,
  commandsQueryResultSchema,
  modelSelectionSchema,
  sessionConfigStateSchema,
  toolCallCreateWorkflowDisplaySchema,
  sessionsIndexSnapshotSchema,
  v4ConversationWorkflowRunArtifactDataResultSchema,
  v4ConversationWorkflowRunArtifactReadResultSchema,
  v4ConversationWorkflowRunArtifactsResultSchema,
  v4ConversationWorkflowRunEventsResultSchema,
  v4ConversationWorkflowRunNodeResultResultSchema,
  v4ConversationWorkflowRunWorkspaceResultSchema,
  v4ConversationWorkflowRunsResultSchema,
  workspaceConfigSnapshotSchema,
} from "../../../packages/shared/src/zcode-protocol-v4/index.ts";
import { zcodeTaskMetaSchema } from "../../../packages/shared/src/validation.ts";
import { expect, test } from "vitest";

const fixtureRoot = resolve(import.meta.dirname, "../../../tooling/native-live/workflow-contract-fixtures");

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(resolve(fixtureRoot, name), "utf8"));
}

// conversationWorkflowRunGraph 是 Host 的查询投影；source shared 当前没有为该方法导出
// schema（causalityGraph 只属于 create_workflow display，字段集不同），所以这里用与
// Rust `project_graph` 同构的 strict guard 覆盖实际查询合同，避免把两个图模型混用。
function assertWorkflowRunGraph(value: unknown): asserts value is {
  runId: string;
  nodes: Array<{ id: string; kind: string; label: string; lane: string; depth: number }>;
  edges: Array<{ from: string; to: string; back?: true }>;
} {
  expect(value).toMatchObject({
    runId: expect.any(String),
    nodes: expect.any(Array),
    edges: expect.any(Array),
  });
  const graph = value as Record<string, unknown>;
  expect(Object.keys(graph).sort()).toEqual(["edges", "nodes", "runId"]);
  expect((graph.nodes as unknown[]).length).toBeLessThanOrEqual(2000);
  expect((graph.edges as unknown[]).length).toBeLessThanOrEqual(4000);
  for (const node of graph.nodes as Record<string, unknown>[]) {
    expect(Object.keys(node).sort()).toEqual(["depth", "id", "kind", "label", "lane"]);
    expect(typeof node.id).toBe("string");
    expect(typeof node.kind).toBe("string");
    expect(typeof node.label).toBe("string");
    expect(typeof node.lane).toBe("string");
    expect(Number.isInteger(node.depth)).toBe(true);
    expect((node.depth as number) >= 0).toBe(true);
  }
  for (const edge of graph.edges as Record<string, unknown>[]) {
    expect(Object.keys(edge).every((key) => key === "from" || key === "to" || key === "back")).toBe(
      true,
    );
    expect(typeof edge.from).toBe("string");
    expect(typeof edge.to).toBe("string");
    if (edge.back !== undefined) expect(edge.back).toBe(true);
  }
}

test("Rust conversation snapshot, delta, sessions index, and workspace config satisfy source schemas", () => {
  const snapshot = conversationSnapshotSchema.parse(fixture("conversation_snapshot.json"));
  const childSnapshot = conversationSnapshotSchema.parse(
    fixture("conversation_child_snapshot.json"),
  );
  const config = snapshot.config;
  sessionConfigStateSchema.parse(config);
  if (config.modelSelection !== undefined) {
    modelSelectionSchema.parse(config.modelSelection);
  }
  expect(config.provider).toBe("provider-contract");
  expect(config.model).toBe("model-contract");
  expect(snapshot.goal).toEqual({
    targetId: "contract-goal",
    objective: "验证 Goal projection contract",
    summaryTitle: "Contract goal",
    timeUsedSeconds: 12,
    activeRunStartedAtMs: null,
    status: "active",
    iteration: 1,
    verifications: [],
    iterations: [],
  });
  expect(snapshot.availability.pauseGoal).toEqual({ allowed: true });
  expect(snapshot.availability.resumeGoal).toEqual({
    allowed: false,
    reasonCode: "goal.notPaused",
  });
  expect(snapshot.subagents).toEqual({
    revision: 8,
    childSessionIds: [
      "agent:contract-session:contract-child-ended",
      "agent:contract-session:contract-child-waiting",
    ],
    running: [
      {
        childSessionId: "agent:contract-session:contract-child-waiting",
        agentId: "contract-child-waiting",
        subagentType: "agent",
        title: "/root/contract-waiting",
        status: "waiting",
      },
    ],
    endedTotal: 1,
  });
  expect(snapshot.backgroundWorks).toEqual([
    {
      workId: "contract-workflow-live",
      kind: "workflow",
      title: "契约活动工作流",
      status: "running",
      startedAt: 124,
      cancellable: true,
      anchorRowId: null,
    },
  ]);
  expect(snapshot.workflowRuns?.runs).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        runId: "contract-workflow-live",
        status: "running",
        toolCallId: "tool-workflow-live",
      }),
      expect.objectContaining({
        runId: "contract-workflow-completed",
        status: "completed",
        toolCallId: "tool-workflow-completed",
      }),
    ]),
  );
  expect(childSnapshot.sessionId).toBe("agent:contract-session:contract-child-waiting");
  expect(childSnapshot.inputRouting.mode).toBe("reject");
  expect(childSnapshot.inputRouting.reasonCode).toBe("agent.readOnly");
  expect(childSnapshot.usage).toEqual({
    contextWindow: null,
    cumulative: {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    },
  });
  const workflowToolRow = snapshot.rows.window.find(
    (row) => row.kind === "toolCall" && row.toolCallId === "contract-workflow-call",
  );
  expect(workflowToolRow).toBeDefined();
  const workflowDisplay = toolCallCreateWorkflowDisplaySchema.parse(workflowToolRow?.display);
  expect(workflowDisplay).toMatchObject({
    kind: "create_workflow",
    ok: true,
    errorCount: 0,
    diagnostics: [],
  });
  expect(workflowDisplay.causalityGraph?.steps.map((step) => step.kind)).toEqual([
    "ask",
    "world-read",
  ]);
  expect(workflowDisplay.causalityGraph?.participants).toHaveLength(2);
  expect(apiRetryStateSchema.parse(snapshot.control.apiRetry)).toEqual({
    attempt: 2,
    maxAttempts: 4,
    nextRetryAt: 10750,
    reasonCode: "provider_retry",
  });
  expect(apiRetryStateSchema.parse(fixture("api_retry.json"))).toEqual(snapshot.control.apiRetry);

  conversationDeltaSchema.parse(fixture("conversation_delta.json"));
  sessionsIndexSnapshotSchema.parse(fixture("sessions_index.json"));
  workspaceConfigSnapshotSchema.parse(fixture("workspace_config.json"));
});

test("assistant feedback rows keep Source enum values and omit cancelled feedback", () => {
  const rows = fixture("assistant_feedback_rows.json") as Record<string, unknown>;
  const like = conversationRowSchema.parse(rows.like);
  const dislike = conversationRowSchema.parse(rows.dislike);
  const cancelled = conversationRowSchema.parse(rows.cancelled);

  expect(like).toMatchObject({ kind: "assistantText", feedback: "like" });
  expect(dislike).toMatchObject({ kind: "assistantText", feedback: "dislike" });
  expect(cancelled).not.toHaveProperty("feedback");
  expect(
    conversationRowSchema.safeParse({ ...(rows.cancelled as object), feedback: null }).success,
  ).toBe(false);
});

test("legacy renameTask returns a complete Source task metadata DTO", () => {
  const raw = fixture("rename_task_meta.json") as Record<string, unknown>;
  const meta = zcodeTaskMetaSchema.parse(raw);

  expect(meta).toMatchObject({
    taskId: "session-rename-contract",
    title: "冷恢复手动标题",
    titleOverridden: true,
    workspacePath: "C:/isolated/project",
    unreadAt: 123,
    status: "completed",
  });
  // pinned/archive 是 legacy task service 的投影附加字段，必须随 ACK 保留，
  // 但不扩展 Source 的 ZCodeTaskMeta schema。
  expect(raw).toMatchObject({ pinned: true, archived: false });
  expect(raw).not.toHaveProperty("response", null);
});

test("Rust command receipt query states satisfy the Source strict schema", () => {
  const query = commandsQueryResultSchema.parse(fixture("command_query.json"));
  expect(query.results[0]?.result).toMatchObject({
    commandId: "command-rejected",
    status: "rejected",
    reasonCode: "rpc.invalidParams",
  });
  expect(query.results[1]?.result).toBe("unknown");
  expect(query.results[2]?.result).toBe("unknown");
});

test("Rust workflow detail, graph, timeline, workspace, node result, and artifacts satisfy contracts", () => {
  v4ConversationWorkflowRunArtifactsResultSchema.parse(fixture("workflow_artifacts.json"));
  v4ConversationWorkflowRunArtifactDataResultSchema.parse(
    fixture("workflow_artifact_data.json"),
  );
  v4ConversationWorkflowRunArtifactReadResultSchema.parse(fixture("workflow_artifact_read.json"));
  v4ConversationWorkflowRunWorkspaceResultSchema.parse(fixture("workflow_workspace.json"));
  v4ConversationWorkflowRunNodeResultResultSchema.parse(fixture("workflow_node_result.json"));
  v4ConversationWorkflowRunEventsResultSchema.parse(fixture("workflow_run_timeline.json"));
  v4ConversationWorkflowRunsResultSchema.parse(fixture("workflow_runs.json"));

  const graph = fixture("workflow_graph.json");
  assertWorkflowRunGraph(graph);
  expect(graph.runId).toBe("run-contract");
  expect(graph.nodes.length).toBeGreaterThan(0);
});
