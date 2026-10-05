import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { activeSessionTurns, activeWorkflowActors, archiveEvidenceComplete, assertWorkflowSuccessorFrozenFacts, isTurnInFlight, journalEvents, workflowRunEvents, journalSequenceBaseline, eventsAfterJournalBaseline, workflowToolIdentityMatches } from './native-live-journal.mjs';

const records = JSON.parse(await readFile(new URL('../native-live/workflow-contract-fixtures/native_journal_contract.json', import.meta.url), 'utf8'));

test('发送基线按各 Session 序号隔离，旧会话高序号不能排除新会话真实子 Turn', () => {
  const before = [{ session: 'old', sequence: 100 }, { session: 'parent', sequence: 3 }];
  const baseline = journalSequenceBaseline(before);
  const events = [...before, { session: 'old', sequence: 99 },
    { session: 'parent', sequence: 4 }, { session: 'new-parent', sequence: 1 }];
  assert.deepEqual(eventsAfterJournalBaseline(events, baseline), events.slice(-2));
  assert.deepEqual(eventsAfterJournalBaseline(before.reverse(), baseline), []);
  assert.deepEqual(eventsAfterJournalBaseline(events.slice(-1), new Map()), events.slice(-1));
  assert.throws(() => eventsAfterJournalBaseline(events, undefined), /高水位/);
});

test('实际 Rust Journal flatten 与 atomic batch 保留会话归属', () => {
  assert.equal(records[0].event, undefined);
  const events = journalEvents(records);
  assert.equal(events.length, 5);
  assert.equal(events[0].type, 'workflow_event_committed');
  assert.equal(events[0].session, 'parent');
  assert.equal(events[2].payload.turn_id, 'left-turn');
  assert.throws(() => journalEvents([{ ...records[0], version: 7 }]), /schema\/version/);
});

test('创建 actor 不能冒充 active，终态按自身 Journal 扣除', () => {
  assert.deepEqual(activeWorkflowActors(records.slice(0, 1)), []);
  assert.equal(activeWorkflowActors(records.slice(0, 3)).length, 2);
  assert.deepEqual(activeWorkflowActors(records), [{ sessionId: 'actor-right', runId: 'contract-run', turnCount: 1 }]);
  assert.equal(activeWorkflowActors([...records].reverse()).length, 1);
});

test('资源采样的活跃会话由真实 Turn 终态决定，空会话不计入', () => {
  assert.deepEqual(activeSessionTurns(records.slice(0, 1)), []);
  assert.equal(activeSessionTurns(records.slice(0, 3)).length, 2);
  assert.deepEqual(activeSessionTurns(records), [{ sessionId: 'actor-right', turnCount: 1 }]);
});

function turnRecord(session, sequence, type, turnId) {
  return {
    schema: 'keencode/session-event',
    version: 8,
    session,
    sequence,
    timeUnixMs: sequence,
    type,
    payload: { turn_id: turnId },
  };
}

test('in-flight 断言只接受当前 Session 的唯一未终态 Turn', () => {
  const active = [
    turnRecord('session-a', 1, 'turn_started', 'turn-a'),
    turnRecord('session-a', 2, 'model_round_completed', 'turn-a'),
  ];
  assert.equal(isTurnInFlight(active, 'session-a', 'turn-a'), true);
  assert.equal(isTurnInFlight(active, 'session-unknown', 'turn-a'), false);
  assert.equal(isTurnInFlight(active, 'session-a', 'turn-unknown'), false);
  assert.equal(isTurnInFlight(active, 'session-a', ''), false);
});

test('in-flight 断言拒绝同一 Turn 的完成、停止和重复开始', () => {
  for (const terminal of ['turn_completed', 'turn_stopped', 'turn_cancelled', 'turn_failed']) {
    const events = [
      turnRecord('session-a', 1, 'turn_started', 'turn-a'),
      turnRecord('session-a', 2, terminal, 'turn-a'),
    ];
    assert.equal(isTurnInFlight(events, 'session-a', 'turn-a'), false, terminal);
  }
  assert.equal(isTurnInFlight([
    turnRecord('session-a', 1, 'turn_started', 'turn-a'),
    turnRecord('session-a', 2, 'turn_started', 'turn-a'),
  ], 'session-a', 'turn-a'), false);
  assert.equal(isTurnInFlight([
    turnRecord('session-a', 1, 'turn_started', 'turn-a'),
    turnRecord('session-b', 99, 'turn_completed', 'turn-a'),
  ], 'session-a', 'turn-a'), true);
});

function toolEvent(type, request, outcome) {
  return {
    type,
    payload: type === 'tool_requested'
      ? { request }
      : { request_id: request.requestId, outcome },
  };
}

test('工具请求与完成只接受同一 child、turn、requestId 和真实 result 错误位', () => {
  const request = {
    requestId: 'request-child-1',
    agentId: 'child-1',
    turnId: 'child-turn-1',
    toolName: 'Bash',
    effect: 'write',
  };
  const requested = toolEvent('tool_requested', request);
  const completed = toolEvent('tool_completed', request, {
    status: 'succeeded',
    result: { isError: false, content: 'real output' },
  });
  const expected = {
    requestToolName: 'Bash',
    requestEffect: 'write',
    requestAgentId: 'child-1',
    requestTurnId: 'child-turn-1',
    requestId: 'request-child-1',
  };
  const expectedCompleted = {
    ...expected,
    outcomeStatus: 'succeeded',
    outcomeIsError: false,
  };
  assert.equal(workflowToolIdentityMatches(requested, requested, expected), true);
  assert.equal(workflowToolIdentityMatches(completed, requested, expectedCompleted), true);
  assert.equal(workflowToolIdentityMatches(
    completed,
    { payload: { request: { ...request, agentId: 'root-agent' } } },
    expectedCompleted,
  ), false);
  assert.equal(workflowToolIdentityMatches(
    completed,
    { payload: { request: { ...request, turnId: 'old-turn' } } },
    expectedCompleted,
  ), false);
  assert.equal(workflowToolIdentityMatches(
    { ...completed, payload: { ...completed.payload, request_id: 'request-other' } },
    requested,
    expectedCompleted,
  ), false);
  assert.equal(workflowToolIdentityMatches(
    { ...completed, payload: { ...completed.payload, outcome: { status: 'succeeded', result: { isError: true } } } },
    requested,
    expectedCompleted,
  ), false);
  assert.equal(workflowToolIdentityMatches(
    { ...completed, payload: { ...completed.payload, outcome: { status: 'succeeded', isError: false } } },
    requested,
    expectedCompleted,
  ), false);
});

function archiveReceipt(phase = 'removed') {
  return {
    phase,
    sessionId: 'session-archive',
    operationId: 'archive-operation',
    archiveSequence: 7,
    checkout: { root: 'C:\\isolated\\source-worktree', path: 'C:\\isolated\\target' },
    branch: 'feat/handoff-target',
  };
}

test('归档证据缺少任一只读事实时不能假通过', () => {
  const base = {
    receipt: archiveReceipt(),
    projectIdentity: 'c:\\isolated\\project',
    targetIdentity: 'c:\\isolated\\target',
    targetPresent: false,
    worktreePaths: new Set([
      'c:\\isolated\\project',
      'c:\\isolated\\source-worktree',
    ]),
    tombstone: {
      sessionId: 'session-recreated',
      projectRoot: 'C:\\isolated\\target',
    },
    ownerSessionId: 'session-archive',
  };
  assert.equal(archiveEvidenceComplete(base), true);
  for (const change of [
    { receipt: archiveReceipt('prepared') },
    { targetPresent: true },
    { worktreePaths: new Set(['c:\\isolated\\project', 'c:\\isolated\\target']) },
    { tombstone: undefined },
    { tombstone: { sessionId: 'other-session', projectRoot: 'C:\\isolated\\other' } },
    { ownerSessionId: 'other-session' },
    { worktreePaths: new Set(['c:\\isolated\\project']) },
  ]) {
    assert.equal(archiveEvidenceComplete({ ...base, ...change }), false);
  }
});

test('工作流验收按真实 run 和 actor 绑定隔离，不累计另一轮的活动或终态', () => {
  const events = journalEvents(records);
  const other = structuredClone(records);
  for (const record of other) {
    record.session = `other-${record.session}`;
    for (const event of record.type === 'atomic_batch' ? record.payload.events : [record]) {
      if (event.type === 'workflow_event_committed') {
        event.payload.record.runId = 'other-run';
        event.payload.record.actorSessionId = `other-${event.payload.record.actorSessionId}`;
      }
    }
  }
  const combined = [...events, ...journalEvents(other)];
  assert.equal(workflowRunEvents(combined, 'contract-run').length, 2);
  assert.equal(workflowRunEvents(combined, 'contract-run', true).length, 3);
  assert.ok(workflowRunEvents(combined, 'contract-run', true).every((event) => !event.session.startsWith('other-')));
  assert.equal(activeWorkflowActors([...records, ...other], 'contract-run').length, 1);
  assert.deepEqual(workflowRunEvents(combined, 'missing-run', true), []);
  assert.throws(() => workflowRunEvents([...events, { ...events[0], session: 'wrong-parent' }], 'contract-run'), /多个父会话/);
  const actorBound = { ...events[0], session: 'actor-left', payload: { record: {
    ...events[0].payload.record, eventType: 'actor-bound', actorSessionId: 'actor-left',
    payload: { parentSessionId: 'parent' },
  } } };
  assert.equal(workflowRunEvents([...combined, actorBound], 'contract-run').length, 3);
});

function startedEvent(session, runId, payload, sequence) {
  return {
    type: 'workflow_event_committed',
    session,
    sequence,
    payload: { record: {
      runId,
      toolCallId: `${runId}-tool`,
      sequence,
      eventType: 'run-started',
      payload,
      artifacts: [],
      actorSessionId: null,
      launchInputId: `${runId}-launch`,
    } },
  };
}

function frozenRun(runId, predecessorRunId = null) {
  return {
    status: 'running',
    name: 'native_recoverable_read',
    scope: 'project',
    canonicalHash: 'sha256:definition',
    inputHash: 'sha256:inputs',
    definition: { body: [{ node_id: 'read', type: 'tool' }] },
    inputs: { label: 'NATIVE_RECOVERABLE_READ_41' },
    budgets: { maxNodes: 1024 },
    budgetsHash: 'sha256:budgets',
    parentSessionId: 'parent',
    cwd: 'C:\\isolated\\project',
    models: {
      provider: {
        providerId: 'native-live-deepseek', model: 'deepseek-v4.1-flash',
        protocol: 'open_ai_chat_completions', configFingerprint: 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
      },
      planEnabled: false,
    },
    predecessorRunId,
    runId,
  };
}

test('amend successor 只验证冻结语义，允许同模型展示结构变化', () => {
  const predecessor = frozenRun('old-run');
  const successor = structuredClone(predecessor);
  successor.runId = 'new-run';
  successor.predecessorRunId = 'old-run';
  successor.models.provider.displayName = 'DeepSeek';
  const events = [startedEvent('parent', 'old-run', predecessor, 1), startedEvent('parent', 'new-run', successor, 2)];
  const result = assertWorkflowSuccessorFrozenFacts(events, {
    predecessorRunId: 'old-run', successorRunId: 'new-run',
    expectedProviderId: 'native-live-deepseek', expectedModel: 'deepseek-v4.1-flash',
  });
  assert.equal(result.successorRunId, 'new-run');
  assert.equal(result.provider.protocol, 'open_ai_chat_completions');
});

test('amend successor 拒绝输入、模型或 predecessor 身份漂移', () => {
  const predecessor = frozenRun('old-run');
  const successor = frozenRun('new-run', 'old-run');
  successor.inputs.label = 'changed';
  const events = [startedEvent('parent', 'old-run', predecessor, 1), startedEvent('parent', 'new-run', successor, 2)];
  assert.throws(() => assertWorkflowSuccessorFrozenFacts(events, {
    predecessorRunId: 'old-run', successorRunId: 'new-run',
  }), /冻结字段: inputs/);
  successor.inputs.label = predecessor.inputs.label;
  successor.models.provider.model = 'other-model';
  assert.throws(() => assertWorkflowSuccessorFrozenFacts(events, {
    predecessorRunId: 'old-run', successorRunId: 'new-run',
  }), /provider\/model/);
  successor.models.provider.model = predecessor.models.provider.model;
  successor.predecessorRunId = 'other-run';
  assert.throws(() => assertWorkflowSuccessorFrozenFacts(events, {
    predecessorRunId: 'old-run', successorRunId: 'new-run',
  }), /predecessorRunId/);
});

test('amend successor 拒绝多个 run-started 事实造成的身份歧义', () => {
  const predecessor = frozenRun('old-run');
  const successor = frozenRun('new-run', 'old-run');
  const events = [startedEvent('parent', 'old-run', predecessor, 1),
    startedEvent('parent', 'new-run', successor, 2), startedEvent('parent', 'new-run', successor, 3)];
  assert.throws(() => assertWorkflowSuccessorFrozenFacts(events, {
    predecessorRunId: 'old-run', successorRunId: 'new-run',
  }), /缺少唯一 run-started/);
});
