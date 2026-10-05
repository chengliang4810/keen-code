/** Native 验收只读取当前 Rust SessionEventRecord：事件被 serde flatten 到顶层。 */
export function journalEvents(records) {
  return records.flatMap((record) => {
    if (record.schema !== 'keencode/session-event' || record.version !== 8
        || typeof record.session !== 'string' || !Number.isSafeInteger(record.sequence)) {
      throw new Error('原生 Journal 记录的 schema/version/身份无效');
    }
    const events = record.type === 'atomic_batch' ? record.payload?.events : [record];
    if (!Array.isArray(events) || !events.length
        || events.some((event) => typeof event?.type !== 'string' || !event.payload)) {
      throw new Error('原生 Journal 事件 envelope 无效');
    }
    return events.map((event) => ({ type: event.type, payload: event.payload,
      session: record.session, sequence: record.sequence, timeUnixMs: record.timeUnixMs }));
  });
}

/** sequence 属于单个 Session；不能用另一个会话的高水位排除新会话的真实事件。 */
export function journalSequenceBaseline(events) {
  const baseline = new Map();
  for (const event of events) baseline.set(event.session, Math.max(baseline.get(event.session) ?? 0, event.sequence));
  return baseline;
}

export function eventsAfterJournalBaseline(events, baseline) {
  if (!(baseline instanceof Map)) throw new Error('缺少按会话记录的 Journal 高水位');
  return events.filter((event) => event.sequence > (baseline.get(event.session) ?? 0));
}

/**
 * 工具请求与结果必须绑定同一 child、turn 和 requestId；结果的错误位于
 * outcome.result.isError，不能读取未发布的 outcome.isError。
 */
export function workflowToolIdentityMatches(event, requested, {
  requestToolName,
  requestEffect,
  requestAgentId,
  requestTurnId,
  requestId,
  outcomeStatus,
  outcomeIsError,
} = {}) {
  const request = requested?.payload?.request;
  if (requestToolName !== undefined && request?.toolName !== requestToolName) return false;
  if (requestEffect !== undefined && request?.effect !== requestEffect) return false;
  if (requestAgentId !== undefined && request?.agentId !== requestAgentId) return false;
  if (requestTurnId !== undefined && request?.turnId !== requestTurnId) return false;
  if (requestId !== undefined) {
    const actualRequestId = event?.type === 'tool_requested'
      ? request?.requestId
      : event?.payload?.request_id;
    if (actualRequestId !== requestId) return false;
  }
  if (outcomeStatus !== undefined && event?.payload?.outcome?.status !== outcomeStatus) return false;
  if (outcomeIsError !== undefined
      && !Object.is(event?.payload?.outcome?.result?.isError, outcomeIsError)) return false;
  return true;
}

const TURN_TERMINAL_EVENTS = new Set([
  'turn_completed',
  'turn_stopped',
  'turn_cancelled',
  'turn_failed',
]);

/**
 * 只用同一 Session 的 Journal 序列判断 Turn 是否仍在运行；未知身份、重复开始
 * 或任何终态都失败关闭，避免把旧 Turn/其他 Session 当成 renderer reload 前置条件。
 */
export function isTurnInFlight(records, sessionId, turnId) {
  if (typeof sessionId !== 'string' || !sessionId.trim()
      || typeof turnId !== 'string' || !turnId.trim()) return false;
  const events = journalEvents(records)
    .filter((event) => event.session === sessionId
      && event.payload?.turn_id === turnId)
    .sort((left, right) => left.sequence - right.sequence);
  let started = false;
  for (const event of events) {
    if (event.type === 'turn_started') {
      if (event.payload?.turn_id !== turnId || started) return false;
      started = true;
      continue;
    }
    if (event.payload?.turn_id === turnId && TURN_TERMINAL_EVENTS.has(event.type)) return false;
  }
  return started;
}

/** 归档验收必须同时满足 receipt、物理目录、Git 注册和删除负向事实，缺一即失败关闭。 */
export function archiveEvidenceComplete({
  receipt,
  projectIdentity,
  targetIdentity,
  targetPresent,
  worktreePaths,
  tombstone,
  ownerSessionId,
} = {}) {
  if (!receipt || receipt.phase !== 'removed' || typeof receipt.sessionId !== 'string'
      || !receipt.sessionId.trim() || typeof receipt.operationId !== 'string'
      || !receipt.operationId.trim() || !Number.isSafeInteger(receipt.archiveSequence)
      || receipt.archiveSequence < 0 || typeof receipt.branch !== 'string'
      || !receipt.branch.trim() || receipt.sessionId !== ownerSessionId
      || !targetIdentity || targetPresent || !(worktreePaths instanceof Set)
      || worktreePaths.has(targetIdentity) || !worktreePaths.has(projectIdentity)
      || !worktreePaths.has(receiptRootIdentity(receipt))) return false;
  return Boolean(tombstone && archivePathIdentity(tombstone.projectRoot) === targetIdentity);
}

function archivePathIdentity(value) {
  if (typeof value !== 'string' || !value.trim()) return null;
  return value.replaceAll('/', '\\').replace(/^\\\\\?\\/, '')
    .replace(/[\\]+$/, '').toLowerCase();
}

function receiptRootIdentity(receipt) {
  if (typeof receipt.checkout?.root !== 'string' || !receipt.checkout.root.trim()) return null;
  return receipt.checkout.root.replaceAll('/', '\\').replace(/^\\\\\?\\/, '')
    .replace(/[\\]+$/, '').toLowerCase();
}

/** 资源测量只采信权威 Journal 中尚未出现终态的 Turn，不以界面按钮代替运行事实。 */
export function activeSessionTurns(records) {
  const events = journalEvents(records);
  const turns = new Map();
  for (const event of events.sort((a, b) => a.session.localeCompare(b.session) || a.sequence - b.sequence)) {
    if (event.type === 'turn_started') {
      let active = turns.get(event.session);
      if (!active) turns.set(event.session, active = new Set());
      active.add(event.payload.turn_id);
    } else if (event.type === 'turn_completed' || event.type === 'turn_stopped') {
      turns.get(event.session)?.delete(event.payload.turn_id);
    }
  }
  return [...turns].filter(([, active]) => active.size > 0)
    .map(([sessionId, active]) => ({ sessionId, turnCount: active.size }));
}

/** actor-started 只代表创建；active 必须由该 actor 自身 Journal 的未终态 Turn 证明。 */
export function activeWorkflowActors(records, runId) {
  const actors = new Map();
  for (const event of journalEvents(records)) {
    if (event.type === 'workflow_event_committed' && event.payload.record?.eventType === 'actor-started') {
      const record = event.payload.record;
      if (typeof record.actorSessionId === 'string' && (!runId || record.runId === runId)) {
        actors.set(record.actorSessionId, record.runId);
      }
    }
  }
  return activeSessionTurns(records).filter(({ sessionId }) => actors.has(sessionId))
    .map((session) => ({ ...session, runId: actors.get(session.sessionId) }));
}

/** 当前 run 的父事件和 actor 由 Journal 绑定，旧运行不能满足本轮取消或恢复断言。 */
export function workflowRunEvents(events, runId, actorsOnly = false) {
  if (typeof runId !== 'string' || !runId.trim()) throw new Error('工作流验收需要已捕获的 runId');
  const runEvents = events.filter((event) => event.type === 'workflow_event_committed'
    && event.payload.record?.runId === runId);
  // actor-bound 写在 actor 自己的 Journal；父归属应由启动事实确定，不能把 actor 当成第二个父。
  const parents = new Set(runEvents.filter((event) => ['run-started', 'actor-started'].includes(event.payload.record.eventType))
    .map((event) => event.session));
  if (parents.size > 1) throw new Error('同一工作流 runId 绑定了多个父会话');
  const actors = new Set(runEvents.filter((event) => ['actor-bound', 'actor-started'].includes(event.payload.record.eventType))
    .map((event) => event.payload.record.actorSessionId).filter((id) => typeof id === 'string'));
  if (runEvents.some((event) => !parents.has(event.session) && !actors.has(event.session))) {
    throw new Error('工作流事件来自未绑定的会话');
  }
  if (!actorsOnly) return runEvents;
  return events.filter((event) => actors.has(event.session));
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

function startedPayload(events, runId) {
  const started = workflowRunEvents(events, runId)
    .filter((event) => event.payload.record?.eventType === 'run-started');
  if (started.length !== 1) throw new Error(`工作流 ${runId} 缺少唯一 run-started 冻结事实`);
  return started[0].payload.record.payload;
}

function modelSemanticSnapshot(payload) {
  const provider = payload?.models?.provider;
  if (!provider || typeof provider !== 'object') throw new Error('工作流 run-started 缺少 provider 冻结事实');
  if (typeof payload.models.planEnabled !== 'boolean') throw new Error('工作流 run-started 缺少 boolean Plan 冻结事实');
  if (typeof provider.providerId !== 'string' || typeof provider.model !== 'string'
      || typeof provider.protocol !== 'string' || typeof provider.configFingerprint !== 'string') {
    throw new Error('工作流 run-started 的 provider 冻结字段不完整');
  }
  if (!/^sha256:[a-f0-9]{64}$/i.test(provider.configFingerprint)) {
    throw new Error('工作流 run-started 的 provider configFingerprint 无效');
  }
  // 只比较模型选择的语义字段；UI 可为同一选择携带不同的展示/子代理键形状。
  return {
    providerId: provider.providerId,
    model: provider.model,
    protocol: provider.protocol,
    configFingerprint: provider.configFingerprint,
    planEnabled: payload.models.planEnabled,
  };
}

/**
 * 校验 amend 产生的新 run 只替换执行身份，不能改变旧 run 的定义、输入、预算和父工作区。
 * 这是一项只读验收断言，事实全部来自 Journal，不向应用发送命令。
 */
export function assertWorkflowSuccessorFrozenFacts(events, {
  predecessorRunId,
  successorRunId,
  expectedProviderId,
  expectedModel,
  expectedProtocol = 'open_ai_chat_completions',
  expectedPlanEnabled,
} = {}) {
  if (typeof predecessorRunId !== 'string' || typeof successorRunId !== 'string'
      || predecessorRunId === successorRunId) throw new Error('工作流 successor 需要两个不同的 runId');
  const predecessor = startedPayload(events, predecessorRunId);
  const successor = startedPayload(events, successorRunId);
  if (successor.predecessorRunId !== predecessorRunId) {
    throw new Error('successor 的 predecessorRunId 未绑定被替换的旧 run');
  }
  for (const field of ['canonicalHash', 'inputHash', 'definition', 'inputs', 'budgets', 'budgetsHash', 'scope', 'parentSessionId', 'cwd']) {
    if (canonicalJson(predecessor[field]) !== canonicalJson(successor[field])) {
      throw new Error(`successor 改变了冻结字段: ${field}`);
    }
  }
  const predecessorModel = modelSemanticSnapshot(predecessor);
  const successorModel = modelSemanticSnapshot(successor);
  if (canonicalJson(predecessorModel) !== canonicalJson(successorModel)) {
    throw new Error('successor 改变了 provider/model/protocol/Plan 语义');
  }
  if (expectedProviderId !== undefined && successorModel.providerId !== expectedProviderId) {
    throw new Error('successor 未使用本次授权的 provider');
  }
  if (expectedModel !== undefined && successorModel.model !== expectedModel) {
    throw new Error('successor 未使用本次授权的模型');
  }
  if (expectedProtocol !== undefined && successorModel.protocol !== expectedProtocol) {
    throw new Error('successor 未使用预期的 Chat Completions 协议');
  }
  if (expectedPlanEnabled !== undefined && successorModel.planEnabled !== expectedPlanEnabled) {
    throw new Error('successor 改变了父运行的 Plan 状态');
  }
  return {
    predecessorRunId,
    successorRunId,
    predecessorRunIdInSuccessor: successor.predecessorRunId,
    canonicalHash: successor.canonicalHash,
    inputHash: successor.inputHash,
    provider: successorModel,
  };
}
