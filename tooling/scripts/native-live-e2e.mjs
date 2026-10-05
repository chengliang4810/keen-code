#!/usr/bin/env node
/**
 * Windows 原生 WebView2 真实验收入口。执行计划只包含合成项目、界面动作和断言。
 * 凭据来自显式 provider 配置，复制到一次性隔离数据根；不进入参数、截图和报告。
 * 用法：node tooling/scripts/native-live-e2e.mjs --plan PATH --provider-config PATH
 * 需先 cargo build -p keencode-desktop --features native-desktop-tests。
 */
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, mkdtemp, open, readFile, readdir, realpath, rename, unlink, writeFile } from 'node:fs/promises';
import { cpus, tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { createServer } from 'node:net';
import { fileURLToPath } from 'node:url';
import { activeSessionTurns, activeWorkflowActors, archiveEvidenceComplete, assertWorkflowSuccessorFrozenFacts, isTurnInFlight, journalEvents, workflowRunEvents, journalSequenceBaseline, eventsAfterJournalBaseline, workflowToolIdentityMatches } from './native-live-journal.mjs';
import { nativeFrontendFaults, nativeProtocolFaults } from './native-live-faults.mjs';
import { cdpTargetIdentity, exactCdpTarget, sameCdpTarget, waitForStableCdpTarget } from './native-live-browser-surface.mjs';
import { validatePdfBytes } from './native-live-pdf.mjs';
import { validateJsonEvidence } from './native-live-json.mjs';
import { serializePageProbe } from './native-live-page-probe.mjs';
import { applyRuntimeArtifactLimit, parseRuntimeArtifactLimit } from './native-live-runtime-options.mjs';
import { redactHeapSnapshot, summarizeHeapSnapshot } from './native-live-heap.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const flags = new Map();
for (let i = 2; i < process.argv.length; i += 2) {
  if (!['--plan', '--provider-config', '--binary', '--output', '--port', '--request-timeout-ms'].includes(process.argv[i]) || !process.argv[i + 1]) {
    throw new Error('需要 --plan 和 --provider-config，可选 --binary、--output、--port、--request-timeout-ms');
  }
  flags.set(process.argv[i], process.argv[i + 1]);
}
/** 仅控制测试构建中真实 HTTP 请求的总超时，不更换接口、模型或返回内容。 */
function requestTimeoutMs(value) {
  if (value === null || value === undefined) return null;
  const timeout = Number(value);
  if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 300_000) throw new Error('真实请求超时必须为 1..300000 毫秒整数');
  return timeout;
}
const initialRequestTimeoutMs = requestTimeoutMs(flags.get('--request-timeout-ms'));
const required = (name) => {
  const value = flags.get(name);
  if (!value) throw new Error(`缺少 ${name}`);
  return resolve(value);
};
const plan = JSON.parse(await readFile(required('--plan'), 'utf8'));
// 先于 provider、binary 和原生进程准备严格校验，非法计划必须 fail-closed。
const runtimeArtifactLimit = parseRuntimeArtifactLimit(plan.runtimeArtifactLimit);
const workflowDefinition = plan.workflowFile
  ? JSON.parse(await readFile(resolve(dirname(required('--plan')), plan.workflowFile), 'utf8'))
  : undefined;
const config = JSON.parse(await readFile(required('--provider-config'), 'utf8'));
const provider = config.providers?.find((entry) => entry.id === (plan.providerId ?? config.activeProviderId));
const model = plan.model ?? 'deepseek-v4.1-flash';
if (!provider || provider.apiBackend !== 'chat_completions' || !provider.models.includes(model) || !provider.apiKey) {
  throw new Error('验收需要显式配置的 Chat Completions 供应商、模型和凭据');
}
const secrets = config.providers.flatMap((entry) => [entry.apiKey, entry.baseUrl]).filter(Boolean);
const selectedProviderId = provider.id;
const redact = (text) => secrets.reduce((result, secret) => result.split(secret).join('[redacted]'), String(text));
const output = resolve(flags.get('--output') ?? join(root, 'out/native-live', randomUUID()));
await mkdir(output, { recursive: true });
// 验收期间计划文件可能继续完善；保存本次实际载入的非敏感夹具，报告可绑定同一执行快照。
const planSnapshot = JSON.stringify(plan, null, 2);
const planSha256 = createHash('sha256').update(planSnapshot).digest('hex');
await writeFile(join(output, 'plan-snapshot.json'), redact(planSnapshot));
if (workflowDefinition) await writeFile(join(output, 'workflow-snapshot.json'), redact(JSON.stringify(workflowDefinition, null, 2)));
const isolation = await mkdtemp(join(tmpdir(), 'keencode-native-live-'));
const data = join(isolation, 'data');
const project = join(isolation, 'project');
await mkdir(data); await mkdir(project);
// 私有配置只保留本次选择，禁止请求误用现有主配置的默认模型。
await writeFile(join(data, 'providers.json'), JSON.stringify({
  schema: 'keencode/providers', version: 1,
  activeProviderId: provider.id, activeModelId: model, providers: [{ ...provider, models: [model] }],
}), { mode: 0o600 });
for (const [path, content] of Object.entries(plan.files ?? { 'evidence.txt': '原生验收初始内容' })) {
  const target = resolve(project, path);
  if (!target.startsWith(project + '\\') && !target.startsWith(project + '/')) throw new Error('项目夹具路径越界');
  await mkdir(dirname(target), { recursive: true }); await writeFile(target, content);
}
const binaryFixtureObservations = [];
const binaryFixtureTargets = new Set();
if (plan.binaryFixtures !== undefined && !Array.isArray(plan.binaryFixtures)) {
  throw new Error('binaryFixtures 必须为数组');
}
// 二进制夹具只能从固定仓库来源复制到本次项目，并在写入前后各校验一次摘要。
for (const fixture of plan.binaryFixtures ?? []) {
  const source = String(fixture?.source ?? '');
  const targetName = String(fixture?.target ?? '');
  const expectedSha256 = String(fixture?.sha256 ?? '').toLowerCase();
  if ((fixture?.sha256Algorithm !== undefined && fixture.sha256Algorithm !== 'sha256')
      || (fixture?.copy !== undefined && fixture.copy !== 'binary')) {
    throw new Error('binaryFixtures 只支持 sha256 二进制复制');
  }
  const sourcePath = resolve(root, source);
  const target = resolve(project, targetName);
  const sourceIsRelative = source && !isAbsolute(source) && !/^[a-zA-Z]:[\\/]/u.test(source)
    && !source.startsWith('\\\\') && !source.split(/[\\/]+/u).includes('..');
  const targetIsRelative = targetName && !isAbsolute(targetName) && !/^[a-zA-Z]:[\\/]/u.test(targetName)
    && !targetName.startsWith('\\\\') && !targetName.split(/[\\/]+/u).includes('..');
  if (!sourceIsRelative || !targetIsRelative || !/^[0-9a-f]{64}$/u.test(expectedSha256)) {
    throw new Error('binaryFiles 必须使用仓库内来源、项目内目标和 SHA-256');
  }
  if (!sourcePath.startsWith(root + '\\') && !sourcePath.startsWith(root + '/')) throw new Error('二进制夹具来源越界');
  if (!target.startsWith(project + '\\') && !target.startsWith(project + '/')) throw new Error('二进制夹具目标越界');
  if (binaryFixtureTargets.has(target.toLowerCase())) throw new Error('二进制夹具目标重复');
  const bytes = await readFile(sourcePath);
  if (bytes.byteLength === 0 || bytes.byteLength > 64 * 1024 * 1024) throw new Error('二进制夹具大小无效');
  const actualSha256 = createHash('sha256').update(bytes).digest('hex');
  if (actualSha256 !== expectedSha256) throw new Error('二进制夹具 SHA-256 不匹配');
  await mkdir(dirname(target), { recursive: true });
  await writeFile(target, bytes, { mode: 0o600 });
  const copied = await readFile(target);
  const copiedSha256 = createHash('sha256').update(copied).digest('hex');
  if (copiedSha256 !== actualSha256) throw new Error('二进制夹具复制后摘要不匹配');
  binaryFixtureTargets.add(target.toLowerCase());
  binaryFixtureObservations.push({ target: targetName, sha256: copiedSha256, sizeBytes: copied.byteLength });
}
const port = Number(flags.get('--port') ?? 9236);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('CDP 端口无效');
// 不能连接碰巧使用相同端口的其他 WebView2；本次程序启动前必须确认端口空闲。
await new Promise((resolvePort, rejectPort) => {
  const server = createServer();
  server.once('error', () => rejectPort(new Error('CDP 端口已占用，请使用 --port 指定空闲端口')));
  server.listen(port, '127.0.0.1', () => server.close(resolvePort));
});
const binary = resolve(flags.get('--binary') ?? join(root, 'target/debug/keencode-desktop.exe'));
// 报告绑定实际执行的文件，避免重建前后的相同路径被误认为同一个验收版本。
const binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
let environment = { ...process.env, KEENCODE_BENCHMARK: '1', KEENCODE_BENCHMARK_DATA_DIR: data,
  KEENCODE_NATIVE_TEST_DIRECTORY: project,
  WEBVIEW2_USER_DATA_FOLDER: join(isolation, 'webview2'),
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` };
for (const name of Object.keys(environment)) {
  if (/^(OPENAI_|KEENCODE_PROVIDER_)/.test(name)) delete environment[name];
}
// 清除宿主可能遗留的测试覆盖；每次验收只采用本次显式配置。
delete environment.KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS;
environment = applyRuntimeArtifactLimit(environment, runtimeArtifactLimit);
function configureRequestTimeout(value) {
  const timeout = requestTimeoutMs(value);
  if (timeout === null) delete environment.KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS;
  else environment.KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS = String(timeout);
}
configureRequestTimeout(initialRequestTimeoutMs);
const requestTimeoutHistory = [];
let app;
let cdp;
let applicationOutput = `${JSON.stringify({ event: 'native-live.config', runtimeArtifactLimit: runtimeArtifactLimit ?? null })}\n`;
let nativeConnected = false;
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
const expand = (text) => String(text)
  .replaceAll('${WORKFLOW_JSON}', JSON.stringify(workflowDefinition ?? null))
  .replaceAll('${PROJECT_JSON}', JSON.stringify(project))
  .replaceAll('${DATA_JSON}', JSON.stringify(data))
  .replaceAll('${MODEL_JSON}', JSON.stringify(model))
  .replaceAll('${PROJECT}', project).replaceAll('${DATA}', data).replaceAll('${MODEL}', model);
const results = [];
const measureStart = Date.now();
const measurements = [];
const nativeHostProbes = [];
const heapObservations = [];
const runtimeObservations = [];
const cpuProfiles = [];
let activeCpuProfile = false;

/** 分开读取各 Worker 的 V8 堆；主页面的 getHeapUsage 不包含子 isolate。 */
async function runtimeEvidence(step) {
  await nativeHostResponsive(`${step.name}:runtime`);
  const { targetInfos } = await cdp.send('Target.getTargets');
  const heaps = [{ type: 'page', ...(await cdp.send('Runtime.getHeapUsage')) }];
  for (const target of targetInfos.filter((target) => target.type === 'worker')) {
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId: target.targetId, flatten: true });
    try {
      heaps.push({ type: 'worker', url: redact(target.url), ...(await cdp.send('Runtime.getHeapUsage', {}, sessionId)) });
    } finally { await cdp.send('Target.detachFromTarget', { sessionId }); }
  }
  runtimeObservations.push({ name: step.name, at: Date.now(), heaps, dom: await cdp.send('Memory.getDOMCounters') });
}

async function cpuProfile(step) {
  if (step.phase === 'start') {
    await cdp.send('Profiler.enable');
    await cdp.send('Profiler.start');
    activeCpuProfile = true;
  } else if (step.phase === 'stop' && /^[a-z0-9-]{1,64}$/.test(step.name ?? '')) {
    const { profile } = await cdp.send('Profiler.stop');
    activeCpuProfile = false;
    await cdp.send('Profiler.disable');
    const filename = `${step.name}.cpuprofile`;
    await writeFile(join(output, filename), redact(JSON.stringify(profile)));
    cpuProfiles.push({ name: step.name, filename, samples: profile.samples?.length ?? 0, elapsedUs: profile.endTime - profile.startTime });
  } else throw new Error('CPU profile 动作无效');
}

async function heapEvidence(step) {
  if (!/^[a-z0-9-]{1,64}$/.test(step.name ?? '')) throw new Error('堆证据名称无效');
  await cdp.send('HeapProfiler.enable');
  const beforeGc = await cdp.send('Runtime.getHeapUsage');
  await cdp.send('HeapProfiler.collectGarbage');
  const afterGc = await cdp.send('Runtime.getHeapUsage');
  const dom = await cdp.send('Memory.getDOMCounters');
  const chunks = [];
  let bytes = 0;
  let oversized = false;
  const onChunk = ({ data: message }) => {
    const event = JSON.parse(message);
    if (event.method !== 'HeapProfiler.addHeapSnapshotChunk') return;
    bytes += Buffer.byteLength(event.params.chunk);
    if (bytes > 128 * 1024 * 1024) { oversized = true; return; }
    chunks.push(event.params.chunk);
  };
  cdp.ws.addEventListener('message', onChunk);
  try {
    await cdp.send('HeapProfiler.takeHeapSnapshot', { reportProgress: false });
  } finally {
    cdp.ws.removeEventListener('message', onChunk);
    await cdp.send('HeapProfiler.disable');
  }
  if (oversized) throw new Error('堆证据超过 128MiB 上限');
  // 先解码再脱敏，避免 JSON 转义导致原始字符串凭据漏出；磁盘不保存原始快照。
  const snapshot = redactHeapSnapshot(JSON.parse(chunks.join('')), secrets);
  const summary = summarizeHeapSnapshot(snapshot);
  const name = `heap-${step.name}.heapsnapshot`;
  await writeFile(join(output, name), JSON.stringify(snapshot));
  const observation = { name: step.name, beforeGc, afterGc, dom, summary, snapshotFile: name };
  heapObservations.push(observation);
  await writeFile(join(output, `heap-${step.name}-summary.json`), JSON.stringify(observation, null, 2));
}
const frontendErrors = [];
const permissionApprovals = [];
const journalSnapshots = new Map();
const journalBaselines = new Map();
const journalValues = new Map();
const diagnosticBaselines = new Map();
const diagnosticObservations = new Map();
const archiveObservations = new Map();
const pdfObservations = new Map();
const jsonObservations = new Map();
const pageProbeObservations = new Map();
const nativeExitObservations = [];
const workflowRuns = new Map();
const workflowBindings = new Map();
const workflowFrozenRuns = new Map();
const browserSurfaceIdentities = new Map();
const selectionSideIdentities = new Map();
const capturedTasks = new Map();
let launchStarted;
let firstInteractiveMs;
let uiEnvironment;

async function processMetrics() {
  if (!Number.isSafeInteger(app?.pid)) throw new Error('没有本次启动的原生进程');
  // 进程树归属、PID/starttime 守卫、EX2/EX 回退和 GPU WMI 可用性统一由只读探针负责。
  const probe = join(root, 'tooling/native-live/windows-memory-probe.ps1');
  return new Promise((resolveMetrics, rejectMetrics) => {
    const child = spawn('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', probe,
      '-RootProcessId', String(app.pid), '-DataDirectory', data, '-DurationSeconds', '0',
    ], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let text = ''; let errors = '';
    child.stdout.on('data', (chunk) => { text += chunk; });
    child.stderr.on('data', (chunk) => { errors += chunk; });
    child.on('error', rejectMetrics);
    child.on('exit', (code) => {
      if (code) return rejectMetrics(new Error(redact(errors.slice(0, 500))));
      try {
        const lines = text.trim().split(/\r?\n/u).filter(Boolean);
        if (!lines.length) throw new Error('内存探针未返回快照');
        const snapshot = JSON.parse(lines.at(-1));
        if (snapshot.rootPid !== app.pid || !Array.isArray(snapshot.processes)) throw new Error('内存探针快照归属无效');
        const finiteMetric = (value) => {
          if (value === null || value === undefined || value === '') return null;
          const number = Number(value);
          return Number.isFinite(number) && number >= 0 ? number : null;
        };
        const byteMetric = (value) => {
          const number = finiteMetric(value);
          return number !== null && Number.isSafeInteger(number) ? number : null;
        };
        const rawCompleteness = snapshot.completeness && typeof snapshot.completeness === 'object'
          ? snapshot.completeness : {};
        let processRowsComplete = true;
        const processes = snapshot.processes.map((entry) => {
          const name = typeof entry?.name === 'string' ? entry.name : null;
          const cpuMs = finiteMetric(entry?.cpuMs);
          const workingSetBytes = byteMetric(entry?.workingSetBytes ?? entry?.rssBytes);
          const privateBytes = byteMetric(entry?.privateBytes ?? entry?.privateCommitBytes);
          const privateWorkingSetBytes = byteMetric(entry?.privateWorkingSetBytes);
          const privateCommitBytes = byteMetric(entry?.privateCommitBytes);
          const sharedCommitBytes = byteMetric(entry?.sharedCommitBytes);
          const success = entry?.success === true;
          if (!success || name === null || cpuMs === null || workingSetBytes === null || privateBytes === null) processRowsComplete = false;
          return {
            pid: byteMetric(entry?.pid), name, role: entry?.role ?? null, workingSetBytes, privateBytes, cpuMs,
            privateWorkingSetBytes, privateCommitBytes, sharedCommitBytes,
            handles: byteMetric(entry?.handles), threads: byteMetric(entry?.threads), success, error: entry?.error ?? null,
          };
        });
        const completeness = {
          processes: rawCompleteness.processes === true && processRowsComplete,
          workingSetBytes: rawCompleteness.workingSetBytes === true && processRowsComplete,
          privateBytes: rawCompleteness.privateBytes === true && processRowsComplete,
          privateWorkingSetBytes: rawCompleteness.privateWorkingSetBytes === true
            && processRowsComplete && processes.every((entry) => entry.privateWorkingSetBytes !== null),
          privateCommitBytes: rawCompleteness.privateCommitBytes === true
            && processRowsComplete && processes.every((entry) => entry.privateCommitBytes !== null),
          sharedCommitBytes: rawCompleteness.sharedCommitBytes === true
            && processRowsComplete && processes.every((entry) => entry.sharedCommitBytes !== null),
          gpu: rawCompleteness.gpu === true,
        };
        const gpuAvailable = snapshot.gpuAvailable === true;
        let gpu = null;
        if (gpuAvailable) {
          if (!Array.isArray(snapshot.gpu)) throw new Error('GPU 探针快照结构无效');
          gpu = snapshot.gpu.map((entry) => ({
            pid: byteMetric(entry?.pid),
            dedicatedBytes: byteMetric(entry?.dedicatedBytes),
            sharedBytes: byteMetric(entry?.sharedBytes),
            commitBytes: byteMetric(entry?.commitBytes ?? entry?.totalCommittedBytes),
          }));
          if (gpu.some((entry) => entry.pid === null || entry.dedicatedBytes === null || entry.sharedBytes === null || entry.commitBytes === null)) {
            completeness.gpu = false;
          }
        } else {
          completeness.gpu = false;
        }
        const completeAggregate = (value, key) => completeness[key] ? byteMetric(value) : null;
        resolveMetrics({
          at: Date.now(), rootPid: snapshot.rootPid, rootStartedUtc: snapshot.rootStartedUtc,
          logicalCores: byteMetric(snapshot.logicalCores), responding: snapshot.responding === true,
          activeSessions: snapshot.activeSessions ?? null, processes,
          workingSetBytes: completeAggregate(snapshot.rssBytes, 'workingSetBytes'),
          privateBytes: completeAggregate(snapshot.privateCommitBytes, 'privateBytes'),
          privateWorkingSetBytes: completeAggregate(snapshot.privateWorkingSetBytes, 'privateWorkingSetBytes'),
          privateCommitBytes: completeAggregate(snapshot.privateCommitBytes, 'privateCommitBytes'),
          sharedCommitBytes: completeAggregate(snapshot.sharedCommitBytes, 'sharedCommitBytes'),
          gpuAvailable, gpu,
          gpuDedicatedBytes: completeAggregate(snapshot.gpuDedicatedBytes, 'gpu'),
          gpuSharedBytes: completeAggregate(snapshot.gpuSharedBytes, 'gpu'),
          gpuCommitBytes: completeAggregate(snapshot.gpuCommitBytes, 'gpu'),
          completeness,
        });
      } catch (error) { rejectMetrics(error); }
    });
  });
}

async function nativeHostResponsive(phase) {
  const startedAt = Date.now();
  const replied = await cdp.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_zoom_level').then(() => true)");
  if (replied !== true) throw new Error('Rust 宿主未确认只读 IPC');
  nativeHostProbes.push({phase, startedAt, elapsedMs:Date.now() - startedAt, replied:true});
}

async function measure(label, durationMs = 3000, settleMs = 0, minimumActiveActors = 0, activeSessionCount, runId) {
  if (!Number.isInteger(durationMs) || durationMs < 1000 || durationMs > 30000) throw new Error('资源采样时长应为 1 到 30 秒');
  if (!Number.isInteger(settleMs) || settleMs < 0 || settleMs > 30000) throw new Error('资源稳定等待应为 0 到 30 秒');
  await pause(settleMs);
  // WebView 可在宿主锁死时继续渲染；采样前后要求真实只读 IPC 回复，避免把死锁当成空闲优化。
  await nativeHostResponsive(`${label}:before`);
  if (!Number.isInteger(minimumActiveActors) || minimumActiveActors < 0 || minimumActiveActors > 8) throw new Error('并行 actor 采样预算无效');
  if (activeSessionCount !== undefined && (!Number.isInteger(activeSessionCount) || activeSessionCount < 0 || activeSessionCount > 8)) throw new Error('活跃会话采样数量无效');
  const actorSamples = [];
  const sessionSamples = [];
  if (minimumActiveActors) {
    const deadline = Date.now() + 60000;
    while (activeWorkflowActors(await journalRecords(), runId).length < minimumActiveActors) {
      if (Date.now() >= deadline) throw new Error('未同时观察到要求数量的真实 active actor');
      await pause(100);
    }
  }
  const before = await processMetrics();
  const deadline = Date.now() + durationMs;
  do {
    if (minimumActiveActors) actorSamples.push({ at: Date.now(), actors: activeWorkflowActors(await journalRecords(), runId) });
    if (activeSessionCount !== undefined) sessionSamples.push({ at: Date.now(), sessions: activeSessionTurns(await journalRecords()) });
    await pause(Math.min(200, Math.max(0, deadline - Date.now())));
  } while (Date.now() < deadline);
  const after = await processMetrics();
  await nativeHostResponsive(`${label}:after`);
  const prior = new Map(before.processes.map((entry) => [entry.pid, entry.cpuMs]));
  const processMetricsComplete = before.completeness?.processes === true && after.completeness?.processes === true;
  const cpuMs = processMetricsComplete
    ? after.processes.reduce((sum, entry) => sum + Math.max(0, entry.cpuMs - (prior.get(entry.pid) ?? entry.cpuMs)), 0)
    : null;
  const elapsedMs = after.at - before.at;
  measurements.push({ label, settleMs, startedAt: before.at, endedAt: after.at, cpuMs, elapsedMs,
    cpuPercentOfMachine: cpuMs !== null && elapsedMs > 0 ? cpuMs / elapsedMs / cpus().length * 100 : null,
    workingSetBytes: after.completeness?.workingSetBytes === true ? after.workingSetBytes : null,
    privateBytes: after.completeness?.privateBytes === true ? after.privateBytes : null,
    privateWorkingSetBytes: after.completeness?.privateWorkingSetBytes === true ? after.privateWorkingSetBytes : null,
    privateCommitBytes: after.completeness?.privateCommitBytes === true ? after.privateCommitBytes : null,
    sharedCommitBytes: after.completeness?.sharedCommitBytes === true ? after.sharedCommitBytes : null,
    gpuAvailable: after.gpuAvailable, gpu: after.gpuAvailable ? after.gpu : null,
    gpuDedicatedBytes: after.completeness?.gpu === true ? after.gpuDedicatedBytes : null,
    gpuSharedBytes: after.completeness?.gpu === true ? after.gpuSharedBytes : null,
    gpuCommitBytes: after.completeness?.gpu === true ? after.gpuCommitBytes : null,
    completeness: after.completeness, processes: after.processes,
    ...(activeSessionCount !== undefined ? { sessionSamples, activeSessionCountObserved: sessionSamples.some((sample) => sample.sessions.length === activeSessionCount) } : {}),
    ...(minimumActiveActors ? { actorSamples, concurrentActorsObserved: actorSamples.some((sample) => sample.actors.length >= minimumActiveActors) } : {}) });
  if (minimumActiveActors && !actorSamples.some((sample) => sample.actors.length >= minimumActiveActors)) throw new Error('资源采样期间未确认真实并行 actor');
  if (activeSessionCount !== undefined && !sessionSamples.some((sample) => sample.sessions.length === activeSessionCount)) throw new Error('资源采样期间未确认要求数量的活跃会话');
}

/** 外部文件变化只写本次合成项目，不能通过路径或符号链接修改真实工作区。 */
async function projectFileChange(step) {
  const ownedPath = async (name) => {
    if (typeof name !== 'string' || !name || isAbsolute(name) || /^[a-zA-Z]:/.test(name)
        || name.split(/[\\/]+/).includes('..')) throw new Error('合成项目路径无效');
    const target = resolve(project, name);
    if (!target.startsWith(project + '\\') && !target.startsWith(project + '/')) throw new Error('合成项目路径越界');
    for (let path = target; path !== project; path = dirname(path)) {
      const info = await lstat(path).catch((error) => { if (error.code === 'ENOENT') return null; throw error; });
      if (info?.isSymbolicLink()) throw new Error('合成项目路径不能经过符号链接');
    }
    return target;
  };
  const target = await ownedPath(step.path);
  if (step.operation === 'write' && typeof step.text === 'string' && Buffer.byteLength(step.text) <= 64 * 1024) {
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, step.text);
  } else if (step.operation === 'rename') {
    await rename(target, await ownedPath(step.to));
  } else if (step.operation === 'remove') {
    await unlink(target);
  } else throw new Error('合成项目文件变化操作无效');
}

class Cdp {
  constructor(ws) {
    this.ws = ws; this.sequence = 0; this.pending = new Map();
    ws.addEventListener('message', ({ data: message }) => {
      const result = JSON.parse(message); const pending = this.pending.get(result.id);
      if (result.method === 'Runtime.exceptionThrown') {
        frontendErrors.push(redact(result.params.exceptionDetails.exception?.description ?? result.params.exceptionDetails.text));
      } else if (result.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(result.params.type)) {
        // 只保留浏览器已经报告的错误文本，不展开可能包含凭据的对象属性。
        frontendErrors.push(redact(result.params.args.map((item) => item.value ?? item.description ?? '').join(' ')));
      }
      if (frontendErrors.length > 100) frontendErrors.splice(0, frontendErrors.length - 100);
      if (!pending) return;
      this.pending.delete(result.id); clearTimeout(pending.timer);
      if (result.error) pending.reject(new Error(redact(result.error.message))); else pending.resolve(result.result);
    });
  }
  send(method, params = {}, sessionId) {
    const id = ++this.sequence;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`CDP 超时: ${method}`)); }, 20_000);
      this.pending.set(id, { resolve, reject, timer }); this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }
  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', { expression: expand(expression), returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(redact(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text));
    return result.result?.value;
  }
  close() {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer); pending.reject(new Error('CDP 已关闭'));
    }
    this.pending.clear(); this.ws.close();
  }
}

async function connect() {
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    if (app.exitCode !== null) throw new Error('桌面程序在建立 WebView2 连接前退出');
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`);
      const targets = await response.json();
      const page = targets.find((target) => target.type === 'page' && /tauri\.localhost|127\.0\.0\.1:1421/.test(target.url));
      if (page) {
        const socket = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
        const client = new Cdp(socket); await client.send('Page.enable'); await client.send('Runtime.enable');
        nativeConnected = true;
        return client;
      }
    } catch { /* 启动期 CDP 目标还未注册，不把暂时的拒绝连接当成验收成功。 */ }
    await pause(200);
  }
  throw new Error('原生 WebView2 目标启动超时');
}

async function launch() {
  launchStarted = Date.now();
  requestTimeoutHistory.push({
    launch: requestTimeoutHistory.length + 1,
    requestTimeoutMs: requestTimeoutMs(environment.KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS),
  });
  app = spawn(binary, [], { cwd: root, env: environment, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  app.on('error', (error) => { applicationOutput += redact(error.message); });
  for (const stream of [app.stdout, app.stderr]) stream.on('data', (chunk) => {
    // 只保存脱敏、有界输出，真实凭据绝不落入验收报告。
    applicationOutput = (applicationOutput + redact(chunk.toString())).slice(-128 * 1024);
  });
  cdp = await connect();
  if (plan.viewport) {
    const {width, height, deviceScaleFactor} = plan.viewport;
    if (!Number.isInteger(width) || width < 640 || width > 2560 || !Number.isInteger(height) || height < 480 || height > 1800 || ![1, 2].includes(deviceScaleFactor)) {
      throw new Error('截图对照视口参数无效');
    }
    // 仅控制 renderer 的验证视口，不改产品配置或假造运行事件；原生默认验收不使用此项。
    await cdp.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor, mobile:false});
  }
  uiEnvironment = await cdp.evaluate("({viewport:{width:innerWidth,height:innerHeight},deviceScaleFactor:devicePixelRatio,userAgent:navigator.userAgent})");
}
async function stop() {
  if (!app || app.exitCode !== null) { cdp?.close(); return; }
  // 先正常请求应用退出以检验 Journal 屏障；超时后仅清理本测试创建的进程树。
  try {
    await cdp?.send('Runtime.evaluate', { expression: "void window.__TAURI_INTERNALS__.invoke('app_confirm_exit').catch(() => {})" });
  } catch { /* 主窗口也可能已经关闭。 */ }
  cdp?.close();
  const deadline = Date.now() + 5000;
  while (app.exitCode === null && Date.now() < deadline) await pause(100);
  if (app.exitCode === null) {
    await new Promise((done) => {
      const child = spawn('taskkill.exe', ['/PID', String(app.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
      child.on('exit', done); child.on('error', done);
    });
  }
}

async function waitForNativeExit(step) {
  const timeoutMs = Number(step.timeoutMs ?? 30_000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 180_000) {
    throw new Error('原生进程退出等待时间无效');
  }
  if (!app) throw new Error('没有本次启动的原生进程');
  const outcome = await new Promise((resolveExit, rejectExit) => {
    let settled = false;
    let timer;
    const onExit = (code, signal) => finish({ exitCode: code, signal: signal ?? null });
    const onError = (error) => finish(undefined, error);
    const finish = (value, error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      app.removeListener('exit', onExit);
      app.removeListener('error', onError);
      if (error) rejectExit(error); else resolveExit(value);
    };
    timer = setTimeout(() => finish(undefined, new Error('原生进程退出等待超时')), timeoutMs);
    app.once('exit', onExit);
    app.once('error', onError);
    if (app.exitCode !== null) finish({ exitCode: app.exitCode, signal: app.signalCode ?? null });
  });
  if (step.expectedExitCode !== undefined
      && (!Number.isSafeInteger(step.expectedExitCode) || outcome.exitCode !== step.expectedExitCode)) {
    throw new Error(`原生进程退出码不匹配: expected=${step.expectedExitCode}, actual=${outcome.exitCode}`);
  }
  nativeExitObservations.push({ exitCode: outcome.exitCode, signal: outcome.signal });
  cdp?.close();
  cdp = undefined;
}

// PDF 只从隔离 data/exports 读取原生保存结果；DOM、toast 和文件扩展名不能替代字节证据。
async function pdfEvidence(step) {
  const expectedPageCount = Number(step.expectedPageCount);
  if (!Number.isSafeInteger(expectedPageCount) || expectedPageCount < 1 || expectedPageCount > 1000) {
    throw new Error('PDF 只读验收缺少有效页数');
  }
  if (typeof step.path !== 'string' || !step.path.trim()) throw new Error('PDF 只读验收缺少目标路径');
  const target = resolve(expand(step.path));
  const dataRoot = resolve(data);
  if (target !== dataRoot && !target.startsWith(dataRoot + '\\') && !target.startsWith(dataRoot + '/')) {
    throw new Error('PDF 只读验收路径必须位于隔离 data 根');
  }
  const bytes = await readFile(target).catch((error) => {
    if (error.code === 'ENOENT') throw new Error('PDF 保存字节尚未出现在隔离导出目录');
    throw new Error('PDF 保存字节读取失败');
  });
  if (step.minBytes !== undefined
      && (!Number.isSafeInteger(step.minBytes) || step.minBytes < 1 || bytes.byteLength < step.minBytes)) {
    throw new Error('PDF 保存字节小于计划下限');
  }
  const result = validatePdfBytes(bytes, {
    expectedPageCount,
    expectedPageWidthPoints: step.expectedPageWidthPoints,
    expectedPageHeightPoints: step.expectedPageHeightPoints,
    pageSizeTolerancePoints: step.pageSizeTolerancePoints,
  });
  if (step.requireHeader !== undefined && step.requireHeader !== '%PDF-') {
    throw new Error('PDF 只读验收只支持标准 PDF header');
  }
  if (step.requireEof !== undefined && step.requireEof !== '%%EOF') {
    throw new Error('PDF 只读验收只支持标准 PDF EOF');
  }
  if (step.requireCrossReference !== undefined && step.requireCrossReference !== true) {
    throw new Error('PDF 只读验收必须要求交叉引用结构');
  }
  pdfObservations.set(step.name ?? `pdf-${Date.now()}`, { path: step.path, ...result });
  if (step.captureSha256As) journalValues.set(step.captureSha256As, result.sha256);
}

async function jsonEvidence(step) {
  if (typeof step.path !== 'string' || !/^\$\{DATA\}\/exports\//u.test(step.path)) {
    throw new Error('JSON 只读验收路径必须为 ${DATA}/exports 下的文件');
  }
  const timeoutMs = step.timeoutMs ?? 30000;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 180000) {
    throw new Error('JSON 导出等待超时必须为 100..180000 毫秒整数');
  }
  const target = resolve(expand(step.path));
  const exportRoot = resolve(data, 'exports');
  if (!target.startsWith(exportRoot + '\\') && !target.startsWith(exportRoot + '/')) {
    throw new Error('JSON 只读验收路径越界');
  }
  const validationOptions = {
    schema: step.jsonSchema ?? (step.schema && typeof step.schema === 'object' ? step.schema : undefined),
    expectedSchema: step.expectedSchema ?? (Number.isSafeInteger(step.schema) ? step.schema : undefined),
    requiredTopLevelKeys: step.requiredTopLevelKeys ?? [],
    requiredNonEmpty: step.requiredNonEmpty ?? [],
    secrets,
    forbiddenKeyNames: step.forbiddenKeyNames ?? [],
    forbiddenValuePatterns: step.forbiddenValuePatterns ?? [],
    minBytes: step.minBytes ?? 1,
    maxBytes: step.maxBytes ?? 256 * 1024,
  };
  const deadline = Date.now() + timeoutMs;
  let lastTransientError;
  let result;
  for (;;) {
    try {
      const bytes = await readFile(target);
      result = validateJsonEvidence(bytes, validationOptions);
      break;
    } catch (error) {
      if (error?.code && error.code !== 'ENOENT') throw new Error('JSON 导出读取失败');
      const message = String(error?.message ?? '');
      // Rust 保存文件可能经历短暂的不存在或半写入状态；schema、敏感值和
      // 目录越界错误不会重试，防止把真实契约失败拖到超时后才报告。
      if (error?.code !== 'ENOENT' && !/JSON 导出格式无效|JSON 导出为空/u.test(message)) throw error;
      lastTransientError = error?.code === 'ENOENT'
        ? new Error('JSON 导出尚未出现在隔离 exports 目录') : error;
      if (Date.now() >= deadline) throw lastTransientError;
      await pause(Math.min(100, Math.max(1, deadline - Date.now())));
    }
  }
  jsonObservations.set(step.name ?? `json-${Date.now()}`, {
    exists: true,
    validJson: result.jsonValid,
    secretFree: result.configuredSecretsAbsent && result.forbiddenKeysAbsent
      && result.forbiddenValuePatternsAbsent,
    bytes: result.sizeBytes,
    sha256: result.sha256,
    jsonValid: result.jsonValid,
    schemaValid: result.schemaValid,
    requiredNonEmpty: result.requiredNonEmpty,
    configuredSecretsAbsent: result.configuredSecretsAbsent,
    forbiddenKeysAbsent: result.forbiddenKeysAbsent,
    forbiddenValuePatternsAbsent: result.forbiddenValuePatternsAbsent,
    sizeBytes: result.sizeBytes,
  });
  if (step.captureSha256As) journalValues.set(step.captureSha256As, result.sha256);
}

async function approveVisiblePermission(stepIndex) {
  const selector = '[data-permission-option-kind="allowOnce"]';
  const requestId = await cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.dataset.permissionRequestId`);
  if (!requestId || permissionApprovals.some((entry) => entry.requestId === requestId)) return;
  if (permissionApprovals.length >= 32) throw new Error('真实权限审批超过验收预算');
  await screenshot(`permission-${permissionApprovals.length + 1}`);
  await click(selector);
  // 来源控件第一次点击可能只选择条目；Enter 通过原键盘语义提交当前 allowOnce。
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter' });
  permissionApprovals.push({ stepIndex, requestId, option: 'allowOnce', at: new Date().toISOString() });
}

async function readDiagnosticLines() {
  try {
    const lines = (await readFile(join(data, 'logs', 'keencode-desktop.log'), 'utf8')).split(/\r?\n/);
    // 文件尾换行不代表日志记录；否则下一条 spawn 会占据旧空行并被基线跳过。
    if (lines.at(-1) === '') lines.pop();
    return lines;
  } catch (error) {
    if (['ENOENT', 'EBUSY', 'EACCES', 'EPERM'].includes(error.code)) return [];
    throw error;
  }
}

function escapedRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function diagnosticTimestamp(line) {
  const match = String(line).match(/^\s*(\d{13})\s/);
  return match ? Number(match[1]) : null;
}

async function captureDiagnosticBaseline(step) {
  const name = String(step.name ?? '');
  if (!name || diagnosticBaselines.has(name)) throw new Error('诊断日志基线名称缺失或重复');
  const lines = await readDiagnosticLines();
  diagnosticBaselines.set(name, { lineCount: lines.length, capturedAt: Date.now() });
}

async function waitForEditorLaunchDiagnostic(step) {
  const name = String(step.name ?? '');
  const editorId = String(step.editorId ?? '');
  if (!name || !editorId) throw new Error('编辑器启动诊断需要名称和 editorId');
  const baseline = diagnosticBaselines.get(name);
  if (!baseline) throw new Error(`缺少编辑器启动诊断基线: ${name}`);
  const timeoutMs = Number(step.timeoutMs ?? 10000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 120000) throw new Error('编辑器启动诊断等待时间无效');
  const editorPattern = new RegExp(`(?:editor_id|editorId)(?:=|:)\\s*"?${escapedRegExp(editorId)}"?(?=\\s|$|[,}])`);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const lines = await readDiagnosticLines();
    if (lines.length < baseline.lineCount) throw new Error('编辑器诊断日志在点击后发生轮转，无法建立新增事件边界');
    const appended = lines.slice(baseline.lineCount);
    const line = appended.find((candidate) => {
      const timestamp = diagnosticTimestamp(candidate);
      return candidate.includes('ui_editors_open spawn succeeded')
        && editorPattern.test(candidate)
        && /(?:success=|success:)\s*"?true"?(?=\s|$|[,}])/.test(candidate)
        && (timestamp === null || timestamp >= baseline.capturedAt);
    });
    if (line) {
      diagnosticObservations.set(name, { status: 'fulfilled', success: true, editorId, appendedLineCount: appended.length });
      return;
    }
    await pause(100);
  }
  diagnosticObservations.set(name, { status: 'timeout', success: false, editorId });
  throw new Error(`未观察到 ${editorId} 的 ui_editors_open spawn 成功诊断`);
}

async function waitFor(expression, timeout = 120_000, approvePermissions = false, stepIndex) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await cdp.evaluate(expression)) return;
    if (approvePermissions) await approveVisiblePermission(stepIndex);
    await pause(150);
  }
  throw new Error('界面断言等待超时');
}

async function waitForTurnInFlightAndStop(step, stepIndex) {
  const sessionId = journalValues.get(step.sessionEqualsCaptured);
  const turnId = journalValues.get(step.turnEqualsCaptured);
  if (!sessionId || !turnId) throw new Error('reload 前置条件缺少已捕获的 Session/Turn 身份');
  const selector = step.selector ?? '[data-testid="v4-stop"]';
  const timeoutMs = Number(step.timeoutMs ?? 120_000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 300_000) {
    throw new Error('in-flight 前置条件等待时间无效');
  }
  const visibleExpression = `(() => { const e=document.querySelector(${JSON.stringify(selector)}); if(!e)return false; const r=e.getBoundingClientRect(); const s=getComputedStyle(e); return r.width>0 && r.height>0 && s.display!=='none' && s.visibility!=='hidden' && Number(s.opacity)!==0 && !e.hidden && e.getAttribute('aria-hidden')!=='true' && !e.matches(':disabled,[aria-disabled="true"],[data-disabled]:not([data-disabled="false"])'); })()`;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const inFlight = isTurnInFlight(await journalRecords(), sessionId, turnId);
    if (inFlight && await cdp.evaluate(visibleExpression)) return;
    if (step.approvePermissions) await approveVisiblePermission(stepIndex);
    await pause(150);
  }
  throw new Error('目标 Turn 未同时满足 Journal in-flight 与 Source stop 可见');
}

async function elementPosition(selector, client = cdp) {
  // forceMount 弹层可能仍有尺寸；必须排除退出态和透明祖先，避免命中已关闭的副本。
  return client.evaluate(`(() => { const e=[...document.querySelectorAll(${JSON.stringify(selector)})].find(e=>{const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return false;for(let p=e;p;p=p.parentElement){const s=getComputedStyle(p);if(p.hidden||p.getAttribute('aria-hidden')==='true'||s.display==='none'||s.visibility==='hidden'||Number(s.opacity)===0)return false;if(['menu','dialog','listbox'].includes(p.getAttribute('role'))&&p.getAttribute('data-state')==='closed')return false;}return true;}); if(!e)return null;
    e.scrollIntoView({block:'center'}); const r=e.getBoundingClientRect(); const x=r.x+r.width/2; const y=r.y+r.height/2; const hit=document.elementFromPoint(x,y); return {x,y,w:r.width,h:r.height,disabled:e.disabled,testId:e.getAttribute('data-testid'),hitTarget:hit===e||e.contains(hit)}; })()`);
}
function stableLayout(a, b) {
  return Boolean(a && b)
    && Math.abs(a.x - b.x) <= 0.5
    && Math.abs(a.y - b.y) <= 0.5
    && Math.abs(a.w - b.w) <= 0.5
    && Math.abs(a.h - b.h) <= 0.5;
}
async function stableElementPosition(selector, client = cdp) {
  let previous;
  // 先移动真实鼠标触发 hover，再重新取布局；连续两帧坐标稳定且命中目标才允许点击。
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const beforeHover = await elementPosition(selector, client);
    if (!beforeHover?.w || !beforeHover?.h || beforeHover.disabled) return beforeHover;
    await client.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved', x: beforeHover.x, y: beforeHover.y,
    });
    await pause(32);
    const afterHover = await elementPosition(selector, client);
    if (afterHover?.hitTarget && stableLayout(beforeHover, afterHover) && stableLayout(previous, afterHover)) return afterHover;
    previous = afterHover;
  }
  throw new Error('目标控件布局未稳定或鼠标命中目标失败');
}
async function hover(selector, client = cdp) {
  // 先把动作栏真实移动到视口，再由后续 click 使用同一 DOM 定位；不能用 dispatchEvent 冒充 hover。
  const position = await stableElementPosition(selector, client);
  if (!position || !position.w || !position.h || position.disabled) throw new Error('目标控件不存在、隐藏或禁用');
  await client.send('Input.dispatchMouseEvent', {
    type: 'mouseMoved', x: position.x, y: position.y,
  });
}
async function click(selector, client = cdp, button = 'left') {
  // Portal 和设置页由 React 异步挂载；等待真实控件可操作，避免把瞬时缺失当成产品失败。
  // 等待不改变 DOM 或 Store，也不会把隐藏的 forceMount 菜单当成可点击目标。
  const deadline = Date.now() + 5000;
  let position;
  do {
    try {
      position = await stableElementPosition(selector, client);
    } catch {
      position = null;
    }
    if (position?.w && position?.h && !position.disabled) break;
    await pause(50);
  } while (Date.now() < deadline);
  if (!position || !position.w || !position.h || position.disabled) throw new Error('目标控件不存在、隐藏或禁用');
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await client.send('Input.dispatchMouseEvent', {
    type, x: position.x, y: position.y, button: button === 'right' && type === 'mouseMoved' ? 'none' : button, clickCount: 1,
    ...(button === 'right' ? { buttons: type === 'mousePressed' ? 2 : 0 } : {}),
  });
}
async function typeTargetState(selector) {
  // 只读确认当前真实编辑宿主；不调用 focus、修改 DOM 或触发业务事件。
  return cdp.evaluate(`(() => {
    const elements = [...document.querySelectorAll(${JSON.stringify(selector)})];
    const element = elements.find((candidate) => {
      const rect = candidate.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return false;
      for (let parent = candidate; parent; parent = parent.parentElement) {
        const style = getComputedStyle(parent);
        if (parent.hidden || parent.getAttribute('aria-hidden') === 'true'
          || style.display === 'none' || style.visibility === 'hidden'
          || Number(style.opacity) === 0) return false;
        if (['menu', 'dialog', 'listbox'].includes(parent.getAttribute('role'))
          && parent.getAttribute('data-state') === 'closed') return false;
      }
      return true;
    });
    if (!element) return null;
    const contentEditable = element.getAttribute('contenteditable');
    const editingHost = contentEditable !== null && contentEditable !== 'false'
      && (contentEditable === '' || contentEditable === 'true' || element.isContentEditable);
    const formControl = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;
    // Source 候选项显式写 data-disabled="false"；Radix 的禁用项则使用空属性。
    const disabled = element.matches(':disabled,[aria-disabled="true"],[data-disabled]:not([data-disabled="false"])')
      || contentEditable === 'false';
    const active = document.activeElement === element || element.contains(document.activeElement);
    const bridge = element.getAttribute('data-e2e-lexical-bridge');
    return {
      kind: editingHost || contentEditable !== null ? 'contenteditable' : formControl ? 'form' : 'other',
      editingHost,
      disabled,
      active,
      bridgeReady: bridge === null || bridge === 'ready',
      text: editingHost ? (element.textContent ?? '') : null,
    };
  })()`);
}
async function prepareContentEditableType(selector) {
  const deadline = Date.now() + 5000;
  let retriedPhysicalClick = false;
  while (Date.now() < deadline) {
    const state = await typeTargetState(selector);
    if (state?.kind === 'form' || state?.kind === 'other') return false;
    if (state?.kind === 'contenteditable') {
      if (!state.editingHost || state.disabled) throw new Error('目标 ContentEditable 不可编辑');
      if (state.active && state.bridgeReady) return { initialText: state.text };
      // 冷恢复可能在首次点击后替换编辑宿主；只重做真实鼠标点击，不直接调用 DOM focus。
      if (!retriedPhysicalClick) {
        await click(selector);
        retriedPhysicalClick = true;
      }
    }
    await pause(50);
  }
  throw new Error('目标 ContentEditable 未获得焦点或编辑桥接未就绪');
}
async function waitForContentEditableText(selector, expectedText, initialText) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const state = await typeTargetState(selector);
    if (state?.kind === 'contenteditable' && state.editingHost && !state.disabled
      && state.active && state.bridgeReady && state.text !== initialText
      && state.text.includes(expectedText)) return;
    await pause(50);
  }
  throw new Error('ContentEditable 注入后未读回文本或已失去焦点');
}
async function textPosition(texts, selector = '[role=button],[role=option],button,[data-slot]', exact = true, client = cdp) {
  // 只从可见的原控件取坐标，再发送 CDP 鼠标事件，避免 DOM click 绕过 Radix/React 交互。
  const candidates = Array.isArray(texts) ? texts : [texts];
  return client.evaluate(`(() => {
    const wanted = ${JSON.stringify(candidates.map((text) => String(text)))};
    const exact = ${JSON.stringify(exact)};
    const elements = [...document.querySelectorAll(${JSON.stringify(selector)})];
    const visible = elements.filter((element) => {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      const label = (element.innerText ?? element.textContent ?? '').trim();
      for(let p=element;p;p=p.parentElement){const s=getComputedStyle(p);if(p.hidden || p.getAttribute('aria-hidden')==='true' || s.display==='none' || s.visibility==='hidden' || Number(s.opacity)===0 || (['menu','dialog','listbox'].includes(p.getAttribute('role')) && p.getAttribute('data-state')==='closed'))return false;}
      return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden'
        && !element.matches(':disabled,[aria-disabled="true"],[data-disabled]:not([data-disabled="false"])')
        && wanted.some((value) => exact ? label === value : label.includes(value));
    });
    const element = visible[0];
    if (!element) return null;
    element.scrollIntoView({ block: 'center' });
    const rect = element.getBoundingClientRect();
    const x = rect.x + rect.width / 2; const y = rect.y + rect.height / 2; const hit = document.elementFromPoint(x, y);
    return { x, y, w: rect.width, h: rect.height, disabled: false, hitTarget: hit === element || element.contains(hit) };
  })()`);
}
async function stableTextPosition(texts, selector, exact = true) {
  let previous;
  // 文本控件也必须先真实 hover；菜单/tooltip 可能改变滚动布局，不能沿用旧坐标。
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const beforeHover = await textPosition(texts, selector, exact);
    if (!beforeHover?.w || !beforeHover?.h || beforeHover.disabled) return beforeHover;
    await cdp.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved', x: beforeHover.x, y: beforeHover.y,
    });
    await pause(32);
    const afterHover = await textPosition(texts, selector, exact);
    if (afterHover?.hitTarget && stableLayout(beforeHover, afterHover) && stableLayout(previous, afterHover)) return afterHover;
    previous = afterHover;
  }
  throw new Error('文字控件布局未稳定或鼠标命中目标失败');
}
async function clickText(texts, selector, exact = true) {
  const deadline = Date.now() + 5000;
  let position;
  do {
    try {
      position = await stableTextPosition(texts, selector, exact);
    } catch {
      position = null;
    }
    if (position?.w && position?.h && !position.disabled) break;
    await pause(50);
  } while (Date.now() < deadline);
  if (!position || !position.w || !position.h || position.disabled) throw new Error('目标文字控件不存在、隐藏或禁用');
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await cdp.send('Input.dispatchMouseEvent', {
    type, x: position.x, y: position.y, button: 'left', clickCount: 1,
  });
}
async function clearField(selector) {
  // 通过真实键盘选择并删除受控输入值，使 onChange/onKeyDown 与用户操作保持一致。
  await click(selector);
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyDown', key: 'Control', code: 'ControlLeft', modifiers: 2,
  });
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2,
  });
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyUp', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, modifiers: 2,
  });
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyUp', key: 'Control', code: 'ControlLeft', modifiers: 0,
  });
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyDown', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8,
  });
  await cdp.send('Input.dispatchKeyEvent', {
    type: 'keyUp', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8,
  });
}
async function journalRecords() {
  const found = [];
  async function walk(path) {
    for (const entry of await readdir(path, { withFileTypes: true })) {
      const file = join(path, entry.name);
      if (entry.isDirectory() && !entry.isSymbolicLink()) await walk(file);
      else if (entry.name === 'events.jsonl') {
        // 并发 append 的未完成尾行尚未成为完整记录；不把它当成损坏或已确认事实。
        for (const line of (await readFile(file, 'utf8')).split('\n').slice(0, -1).filter(Boolean)) {
          const record = JSON.parse(line); found.push(record);
        }
      }
    }
  }
  await walk(data); return found;
}

function archivePathIdentity(value) {
  if (typeof value !== 'string' || !value.trim()) return null;
  let text = value.replaceAll('/', '\\');
  if (text.startsWith('\\\\?\\')) text = text.slice(4);
  return text.replace(/[\\]+$/, '').toLowerCase();
}

async function readJsonDirectory(directory, label) {
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error.code === 'ENOENT') return [];
    throw new Error(`${label}目录读取失败`);
  }
  const result = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
    const file = join(directory, entry.name);
    const text = await readFile(file, 'utf8');
    if (Buffer.byteLength(text, 'utf8') > 64 * 1024) throw new Error(`${label}记录超过大小限制`);
    let value;
    try {
      value = JSON.parse(text);
    } catch {
      throw new Error(`${label}记录格式无效`);
    }
    result.push(value);
  }
  return result;
}

function runReadOnlyProcess(command, args) {
  return new Promise((resolveProcess, rejectProcess) => {
    const child = spawn(command, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    const append = (target, chunk) => {
      const text = chunk.toString();
      if (Buffer.byteLength(target + text, 'utf8') > 256 * 1024) {
        child.kill();
        rejectProcess(new Error('只读命令输出超过大小限制'));
        return target;
      }
      return target + text;
    };
    child.stdout.on('data', (chunk) => { stdout = append(stdout, chunk); });
    child.stderr.on('data', (chunk) => { stderr = append(stderr, chunk); });
    child.once('error', rejectProcess);
    child.once('close', (code) => resolveProcess({ code, stdout, stderr }));
  });
}

async function archiveEvidence(step) {
  if (step.branch !== undefined) throw new Error('归档只读验收不得按分支选择回执');
  const ownerSessionId = journalValues.get(step.sessionEqualsCaptured);
  if (typeof ownerSessionId !== 'string' || !ownerSessionId.trim()) {
    throw new Error('归档只读验收缺少稳定会话身份');
  }
  const projectIdentity = archivePathIdentity(await realpath(project));
  const receiptRecords = await readJsonDirectory(join(data, 'ui-archive-worktrees'), '归档回执');
  const candidates = receiptRecords.filter((record) => record?.sessionId === ownerSessionId
    && record?.checkout?.root && record?.checkout?.path);
  if (candidates.length > 1) throw new Error('归档只读验收发现多个当前项目回执');
  if (candidates.length !== 1) return null;
  const receipt = candidates[0];
  const target = receipt.checkout?.path;
  const targetIdentity = archivePathIdentity(target);
  if (receipt.phase !== 'removed' || typeof receipt.sessionId !== 'string'
      || typeof target !== 'string' || !isAbsolute(target) || !targetIdentity) return null;
  let targetPresent = true;
  try {
    await lstat(target);
  } catch (error) {
    if (error.code === 'ENOENT') targetPresent = false;
    else throw new Error('归档目标目录状态读取失败');
  }
  if (targetPresent) return null;

  const gitResult = await runReadOnlyProcess('git', ['-C', project, 'worktree', 'list', '--porcelain', '-z']);
  if (gitResult.code !== 0) throw new Error('归档后 Git worktree 只读检查失败');
  const worktreePaths = gitResult.stdout.split('\0')
    .filter((field) => field.startsWith('worktree '))
    .map((field) => archivePathIdentity(field.slice('worktree '.length)))
    .filter(Boolean);
  if (!worktreePaths.includes(projectIdentity)) throw new Error('归档后主 checkout 未出现在 Git worktree 列表');
  if (worktreePaths.includes(targetIdentity)) return null;

  const deletedText = await readFile(join(data, 'deleted-sessions.json'), 'utf8').catch((error) => {
    if (error.code === 'ENOENT') return null;
    throw new Error('Session 删除事实读取失败');
  });
  if (!deletedText) return null;
  if (Buffer.byteLength(deletedText, 'utf8') > 64 * 1024) throw new Error('Session 删除事实超过大小限制');
  let deleted;
  try {
    deleted = JSON.parse(deletedText);
  } catch {
    throw new Error('Session 删除事实格式无效');
  }
  if (!Array.isArray(deleted)) throw new Error('Session 删除事实不是数组');
  // 归档后的 Session 可能在 handoff/重建中生成新的删除记录；用 receipt
  // 的稳定 owner 绑定回执，用目标 checkout 根绑定删除事实，不能猜测 sessionId 一定相同。
  const tombstone = deleted.find((record) => archivePathIdentity(record?.projectRoot) === targetIdentity);
  if (!archiveEvidenceComplete({
    receipt,
    projectIdentity,
    targetIdentity,
    targetPresent,
    worktreePaths: new Set(worktreePaths),
    tombstone,
    ownerSessionId,
  })) return null;
  return {
    receiptPhaseRemoved: true,
    targetAbsent: true,
    gitTargetAbsent: true,
    mainCheckoutListed: true,
    sessionTombstonePresent: true,
  };
}

async function waitForArchiveEvidence(step) {
  const timeoutMs = Number(step.timeoutMs ?? 30_000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 180_000) {
    throw new Error('归档只读验收等待时间无效');
  }
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const evidence = await archiveEvidence(step);
    if (evidence) {
      archiveObservations.set(step.name ?? `archive-${Date.now()}`, evidence);
      return;
    }
    await pause(200);
  }
  throw new Error('归档只读证据不完整');
}

function journalCaptureEvents(records, step) {
  const allEvents = scopedJournalEvents(records, step);
  const candidates = step.newSinceBaseline
    ? eventsAfterJournalBaseline(allEvents, journalBaselines.get(step.newSinceBaseline)?.sequenceBySession)
    : allEvents;
  const sessionId = step.sessionEqualsCaptured
    ? journalValues.get(step.sessionEqualsCaptured)
    : undefined;
  if (step.sessionEqualsCaptured && !sessionId) throw new Error('Journal 快照引用了尚未捕获的会话');
  return candidates.filter((event) => {
    if (event.type !== step.eventType || (sessionId && event.session !== sessionId)) return false;
    if (step.payloadEqualsCaptured && Object.entries(step.payloadEqualsCaptured).some(([key, name]) => {
      if (!journalValues.has(name)) throw new Error('Journal 快照引用了尚未捕获的身份');
      return !Object.is(event.payload?.[key], journalValues.get(name));
    })) return false;
    return true;
  });
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    const entries = Object.entries(value).sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
    return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

function interactionPayloadSha256(interactionId, optionId) {
  // 与 Rust command_receipts::payload_sha256 保持相同的递归对象排序；权限答复键均为 ASCII。
  return createHash('sha256').update(canonicalJson({
    interactionId,
    answer: { optionId },
  })).digest('hex');
}

function journalMatches(events, step) {
  const candidates = step.newSinceBaseline
    ? eventsAfterJournalBaseline(events, journalBaselines.get(step.newSinceBaseline)?.sequenceBySession)
    : events;
  const matches = candidates.filter((event) => {
    if (event?.type !== step.eventType || (step.reason && event.payload?.reason !== step.reason)) return false;
    if (step.sessionEqualsCaptured) {
      const session = journalValues.get(step.sessionEqualsCaptured);
      if (!session) throw new Error('Journal 断言引用了尚未捕获的会话');
      if (event.session !== session) return false;
    }
    if (step.payloadEqualsCaptured && Object.entries(step.payloadEqualsCaptured).some(([key, name]) => {
      if (!journalValues.has(name)) throw new Error('Journal 断言引用了尚未捕获的身份');
      return !Object.is(event.payload?.[key], journalValues.get(name));
    })) return false;
    if (step.workflowEventType && event.payload?.record?.eventType !== step.workflowEventType) return false;
    if (step.workflowStatus && event.payload?.record?.payload?.status !== step.workflowStatus) return false;
    if (step.workflowPredecessorRun) {
      const predecessorRunId = workflowRunId({ workflowRun: step.workflowPredecessorRun });
      if (event.payload?.record?.payload?.predecessorRunId !== predecessorRunId) return false;
    }
    // 只支持一层字段，足够断言本地 Journal 的反馈值且不会把验收器变成通用 JSON 查询器。
    if (step.payloadContains && typeof step.payloadContains === 'object'
        && Object.entries(step.payloadContains).some(([key, value]) => !Object.is(event.payload?.[key], value))) return false;
    const requested = event.type === 'tool_requested'
      ? event
      : candidates.find((candidate) => candidate?.type === 'tool_requested' && candidate.session === event.session
        && candidate.payload?.request?.requestId === event.payload?.request_id);
    if (step.requestAgentIdEqualsCaptured && !journalValues.has(step.requestAgentIdEqualsCaptured)) {
      throw new Error('Journal 断言引用了尚未捕获的 Agent 身份');
    }
    if (step.requestTurnIdEqualsCaptured && !journalValues.has(step.requestTurnIdEqualsCaptured)) {
      throw new Error('Journal 断言引用了尚未捕获的 Turn 身份');
    }
    if (step.requestIdEqualsCaptured && !journalValues.has(step.requestIdEqualsCaptured)) {
      throw new Error('Journal 断言引用了尚未捕获的工具请求身份');
    }
    if (!workflowToolIdentityMatches(event, requested, {
      requestToolName: step.requestToolName,
      requestEffect: step.requestEffect,
      requestAgentId: step.requestAgentIdEqualsCaptured
        ? journalValues.get(step.requestAgentIdEqualsCaptured)
        : undefined,
      requestTurnId: step.requestTurnIdEqualsCaptured
        ? journalValues.get(step.requestTurnIdEqualsCaptured)
        : undefined,
      requestId: step.requestIdEqualsCaptured
        ? journalValues.get(step.requestIdEqualsCaptured)
        : undefined,
      outcomeStatus: step.outcomeStatus,
      outcomeIsError: step.outcomeIsError,
    })) return false;
    if (step.requestInputContains && typeof step.requestInputContains === 'object'
        && !Array.isArray(step.requestInputContains)) {
      const argumentsValue = requested?.payload?.request?.arguments;
      if (!argumentsValue || typeof argumentsValue !== 'object'
          || Object.entries(step.requestInputContains).some(([key, value]) => {
            const expected = typeof value === 'string' ? expand(value) : value;
            return !Object.hasOwn(argumentsValue, key) || !Object.is(argumentsValue[key], expected);
          })) return false;
    }
    if (step.requestContains) {
      const requestText = event.type === 'tool_requested'
        ? JSON.stringify(event.payload?.request)
        : JSON.stringify(requested?.payload?.request?.arguments);
      if (!requestText?.includes(expand(step.requestContains))) return false;
    }
    // 权限拒绝在工具通过 gate 前不会生成 ToolRequested；此时以同一 Session
    // Journal 中的 resolveInteraction 收据作为决定事实，避免误选历史 Read。
    const receipt = event.payload?.receipt;
    if (step.receiptCommandType && receipt?.commandType !== step.receiptCommandType) return false;
    if (step.receiptStatus && receipt?.status?.status !== step.receiptStatus) return false;
    if (step.receiptOptionId && receipt?.status?.ack?.result?.resolvedBy?.optionId !== step.receiptOptionId) return false;
    // CommandReceipt 的 ACK 状态位于 ack.status；result 只承载业务回执。
    if (step.receiptAckStatus && receipt?.status?.ack?.status !== step.receiptAckStatus) return false;
    if (step.receiptNewSince) {
      const baseline = journalBaselines.get(step.receiptNewSince);
      if (!baseline) throw new Error(`Journal receipt 基线不存在: ${step.receiptNewSince}`);
      // CommandReceipt 的 commandId 是当前 resolveInteraction 的持久身份；只接受
      // 基线之后新出现的 id，避免同一计划的旧 deny 收据满足后续断言。
      if (!receipt?.commandId || baseline.commandIds.has(receipt.commandId)) return false;
    }
    if (step.receiptInteractionSince) {
      const baseline = journalBaselines.get(step.receiptInteractionSince);
      if (!baseline?.interactionId) throw new Error(`Journal interaction 基线不存在: ${step.receiptInteractionSince}`);
      // commandId 只能证明是新收据；payload 摘要再把收据绑定到当前 DOM 中的同一次审批。
      // payload 摘要绑定 Rust 规范化后的 optionId；UI 的 rejectOnce 只是展示 kind。
      const payloadOptionId = step.receiptPayloadOptionId ?? step.receiptOptionId;
      if (!receipt?.payloadSha256 || !payloadOptionId
          || receipt.payloadSha256 !== interactionPayloadSha256(baseline.interactionId, payloadOptionId)) return false;
    }
    return true;
  });
  const passed = matches.length >= (step.minCount ?? 1)
    && new Set(matches.map((event) => event.session)).size >= (step.minSessions ?? 1);
  if (passed && (step.captureAs || step.captureSessionAs || step.captureWorkflowRunAs)) {
    const save = (name, values) => {
      const unique = new Set(values);
      if (unique.size !== 1 || ![...unique].every((value) => typeof value === 'string' && value.trim())) {
        throw new Error('当前 Journal 事实未绑定唯一非空身份');
      }
      const value = [...unique][0];
      if (journalValues.has(name) && journalValues.get(name) !== value) throw new Error('已捕获的 Journal 身份发生变化');
      journalValues.set(name, value);
    };
    if (step.captureAs) {
      if (!['agent.agentId', 'turn_id', 'request.requestId'].includes(step.capturePayloadPath)) {
        throw new Error('只支持权威 child/turn/tool-request 身份字段');
      }
      save(step.captureAs, matches.map((event) => step.capturePayloadPath.split('.').reduce((value, key) => value?.[key], event.payload)));
    }
    if (step.captureSessionAs) save(step.captureSessionAs, matches.map((event) => event.session));
    if (step.captureWorkflowRunAs) {
      if (!matches.every((event) => event.type === 'workflow_event_committed'
          && typeof event.payload?.record?.runId === 'string' && event.payload.record.runId.trim())) {
        throw new Error('Journal 捕获工作流 runId 需要 workflow_event_committed 记录');
      }
      save(step.captureWorkflowRunAs, matches.map((event) => event.payload.record.runId));
    }
  }
  return passed;
}

function journalPathIdentity(value) {
  if (typeof value !== 'string' || !value.trim()) return null;
  const withoutVerbatimPrefix = value.startsWith('\\\\?\\') ? value.slice(4) : value;
  return withoutVerbatimPrefix.replaceAll('/', '\\').replace(/[\\]+$/, '').toLowerCase();
}

async function journalPathMatchesProject(value) {
  const candidate = journalPathIdentity(value);
  if (!candidate) return false;
  const projectIdentity = journalPathIdentity(await realpath(project));
  if (candidate === projectIdentity) return true;
  try {
    return journalPathIdentity(await realpath(value)) === projectIdentity;
  } catch {
    try {
      return journalPathIdentity(await realpath(value.startsWith('\\\\?\\') ? value.slice(4) : value)) === projectIdentity;
    } catch {
      return false;
    }
  }
}

async function readSelectionSideIdentity(step) {
  const parentSession = journalValues.get(step.parentSessionCaptured);
  const childSession = journalValues.get(step.childSessionCaptured);
  if (typeof parentSession !== 'string' || typeof childSession !== 'string') {
    throw new Error('selection side identity 需要已捕获的父/子 Session');
  }
  const records = await journalRecords();
  const events = journalEvents(records);
  const parentSelectionReceipts = events.filter((event) => {
    const receipt = event.type === 'command_receipt_committed' ? event.payload?.receipt : undefined;
    const ack = receipt?.status?.status === 'completed' ? receipt.status.ack : undefined;
    return event.session === parentSession
      && receipt?.commandType === 'createSelectionSideSession'
      && receipt?.scope === `session:${parentSession}`
      && ack?.status === 'accepted';
  });
  if (parentSelectionReceipts.length > 1) throw new Error('selection side identity 出现多个父会话完成收据');
  const receipts = parentSelectionReceipts.filter((event) => {
    const result = event.payload.receipt.status.ack?.result;
    return result?.type === 'createSelectionSideSession' && result?.sessionId === childSession;
  });
  if (receipts.length !== 1) return null;
  const receipt = receipts[0].payload.receipt;
  const parentCreated = events.filter((event) => event.type === 'session_created' && event.session === parentSession);
  if (parentCreated.length > 1) throw new Error('selection side 父会话出现多个 session_created 事实');
  if (parentCreated.length !== 1) return null;
  if (!(await journalPathMatchesProject(parentCreated[0].payload?.project_root))) {
    throw new Error('selection side 父会话的 project_root 与当前隔离项目不一致');
  }
  const childCreated = events.filter((event) => event.type === 'session_created' && event.session === childSession);
  if (childCreated.length > 1) throw new Error('selection side child 出现多个 session_created 事实');
  if (childCreated.length !== 1) return null;
  const childRoot = childCreated[0].payload?.project_root;
  if (!(await journalPathMatchesProject(childRoot))) {
    throw new Error('selection side child 的 project_root 与当前隔离项目不一致');
  }
  const unrelatedCompleted = events.filter((event) => {
    const candidate = event.type === 'command_receipt_committed' ? event.payload?.receipt : undefined;
    return candidate?.commandType === 'createSelectionSideSession'
      && candidate?.status?.status === 'completed'
      && candidate?.scope?.startsWith('session:')
      && candidate.status.ack?.status === 'accepted';
  });
  const unrelatedScopes = [];
  for (const event of unrelatedCompleted) {
    if (event.session === parentSession) continue;
    const otherParent = events.find((candidate) => candidate.type === 'session_created'
      && candidate.session === event.session);
    if (!otherParent || !(await journalPathMatchesProject(otherParent.payload?.project_root))) {
      unrelatedScopes.push(event);
    }
  }
  if (unrelatedScopes.length) throw new Error('selection side Journal 出现其他 workspace 父会话的完成收据');
  return {
    parentSessionId: parentSession,
    childSessionId: childSession,
    parentReceiptEventSequence: receipts[0].sequence,
    parentSessionCreatedEventSequence: parentCreated[0].sequence,
    childSessionCreatedEventSequence: childCreated[0].sequence,
    commandType: receipt.commandType,
    receiptScopeMatchesParentSession: true,
    parentProjectRootMatchesProject: true,
    childProjectRootMatchesProject: true,
    childSessionCreatedCount: childCreated.length,
    unrelatedCompletedParentScopeCount: 0,
  };
}

async function waitForSelectionSideIdentity(step) {
  const deadline = Date.now() + (step.timeoutMs ?? 30_000);
  while (Date.now() < deadline) {
    const identity = await readSelectionSideIdentity(step);
    if (identity) return identity;
    await pause(200);
  }
  throw new Error('等待 selection side 父子 Session identity Journal 事实超时');
}

function workflowRunId(step) {
  if (!step.workflowRun) return undefined;
  const runId = workflowRuns.get(step.workflowRun);
  if (!runId) throw new Error('工作流断言引用了尚未由真实 UI 捕获的 run');
  return runId;
}
function scopedJournalEvents(records, step) {
  const events = journalEvents(records);
  const runId = workflowRunId(step);
  return runId ? workflowRunEvents(events, runId, step.workflowActorsOnly === true) : events;
}
async function captureWorkflowRun(step, stepIndex) {
  if (!step.name || workflowRuns.has(step.name)) throw new Error('工作流验收身份名称缺失或重复');
  const expectedJournalRun = step.equalsCapturedAs ? journalValues.get(step.equalsCapturedAs) : undefined;
  if (step.equalsCapturedAs && !expectedJournalRun) throw new Error('UI 工作流 run 需要先捕获对应 Journal runId');
  const deadline = Date.now() + (step.timeoutMs ?? 60000);
  do {
    // 仅读取当前可见原 UI 的 runId，既不调用业务 RPC，也不修改产品状态。
    const ids = await cdp.evaluate(`(() => [...new Set([...document.querySelectorAll('[data-workflow-run-id]')].filter(e => {
      const r=e.getBoundingClientRect(); if (!r.width || !r.height) return false;
      for(let p=e;p;p=p.parentElement){const s=getComputedStyle(p);if(p.hidden || p.getAttribute('aria-hidden')==='true' || s.display==='none' || s.visibility==='hidden' || Number(s.opacity)===0 || (p.getAttribute('role')==='tabpanel' && p.getAttribute('data-state')==='inactive'))return false;}
      return true;
    }).map(e => e.getAttribute('data-workflow-run-id')).filter(Boolean))])()`);
    const unseen = ids.filter((id) => ![...workflowRuns.values()].includes(id));
    if (unseen.length > 1) throw new Error('当前 UI 出现多个未绑定的新工作流 run');
    if (unseen.length === 1) {
      // UI 只能提供可见身份；必须等同一 run-started 已落入 Journal，不能凭 DOM
      // 文本把一个旧/未持久化的 id 绑定到后续断言。
      const started = journalEvents(await journalRecords()).filter((event) => event.type === 'workflow_event_committed'
        && event.payload?.record?.eventType === 'run-started' && event.payload.record.runId === unseen[0]);
      if (started.length !== 1) {
        if (started.length > 1) throw new Error('Journal 为同一工作流 run 写入了多个 run-started 身份');
        if (step.approvePermissions) await approveVisiblePermission(stepIndex);
        await pause(150);
        continue;
      }
      if (expectedJournalRun && unseen[0] !== expectedJournalRun) {
        throw new Error('UI 捕获的 workflow runId 与 Journal 捕获身份不一致');
      }
      workflowRuns.set(step.name, unseen[0]);
      if (journalValues.has(step.name) && journalValues.get(step.name) !== unseen[0]) {
        throw new Error('UI workflow alias 与同名 Journal 身份不一致');
      }
      // 每个 UI alias 都同步保存 Journal 绑定，后续 cold 断言仍只读 Journal。
      journalValues.set(step.name, unseen[0]);
      return;
    }
    if (step.approvePermissions) await approveVisiblePermission(stepIndex);
    await pause(150);
  } while (Date.now() < deadline);
  throw new Error('原 UI 未显示本轮工作流 runId');
}
async function assertWorkflowBindings(step) {
  const runId = workflowRunId(step);
  const events = workflowRunEvents(journalEvents(await journalRecords()), runId);
  const started = events.filter((event) => event.payload.record.eventType === 'run-started');
  if (started.length !== 1 || !Array.isArray(step.nodeIds) || step.nodeIds.length !== 2) {
    throw new Error('需要唯一冻结启动事实和两个预期 actor 节点');
  }
  const frozen = started[0].payload.record;
  const expected = frozen.payload;
  const provider = expected.models?.provider;
  const canonicalProject = await realpath(project);
  const canonicalCwd = await realpath(expected.cwd);
  if (canonicalCwd !== canonicalProject || expected.parentSessionId !== started[0].session
      || expected.models?.planEnabled !== step.planEnabled || provider?.providerId !== selectedProviderId
      || provider?.model !== model || provider?.protocol !== 'open_ai_chat_completions'
      || !/^sha256:[a-f0-9]{64}$/i.test(provider?.configFingerprint ?? '')) {
    throw new Error('工作流启动未冻结本次授权的工作区、模型、协议或 Plan');
  }
  const bound = events.filter((event) => event.payload.record.eventType === 'actor-bound');
  if (bound.length !== 2 || new Set(bound.map((event) => event.session)).size !== 2) {
    throw new Error('缺少两份不同 actor 的唯一绑定事实');
  }
  const remaining = new Set(step.nodeIds);
  const facts = [];
  for (const event of bound) {
    const record = event.payload.record;
    const payload = record.payload;
    const actualProvider = payload.models?.provider;
    if (record.actorSessionId !== event.session || record.toolCallId !== frozen.toolCallId
        || record.launchInputId !== frozen.launchInputId || payload.runId !== runId
        || payload.parentSessionId !== started[0].session || payload.cwd !== expected.cwd
        || payload.planEnabled !== step.planEnabled || payload.models?.planEnabled !== step.planEnabled
        || payload.nodeAddress?.node_id !== payload.nodeId || !remaining.delete(payload.nodeId)
        || !Array.isArray(payload.nodeAddress.invocation) || payload.nodeAddress.invocation.length
        || Object.keys(provider).length !== Object.keys(actualProvider ?? {}).length
        || Object.entries(provider).some(([key, value]) => !Object.is(actualProvider?.[key], value))) {
      throw new Error('actor 的身份、节点或冻结配置与父运行不一致');
    }
    facts.push({ actorSessionId: event.session, nodeId: payload.nodeId, parentSessionId: payload.parentSessionId,
      cwd: payload.cwd, providerId: actualProvider.providerId, model: actualProvider.model,
      protocol: actualProvider.protocol, planEnabled: payload.planEnabled });
  }
  workflowBindings.set(step.workflowRun, facts);
}
async function assertWorkflowSuccessor(step) {
  const predecessorRunId = workflowRunId({ workflowRun: step.predecessorWorkflowRun });
  const successorRunId = journalValues.get(step.successorJournalRun);
  if (!successorRunId) throw new Error('successor 尚未由 Journal 捕获唯一 runId');
  if (step.requireSelectedModel !== true) throw new Error('successor 断言必须明确要求本次授权模型');
  const facts = assertWorkflowSuccessorFrozenFacts(journalEvents(await journalRecords()), {
    predecessorRunId,
    successorRunId,
    expectedProviderId: selectedProviderId,
    expectedModel: model,
    expectedProtocol: step.expectedProtocol ?? 'open_ai_chat_completions',
    ...(Object.hasOwn(step, 'planEnabled') ? { expectedPlanEnabled: step.planEnabled } : {}),
  });
  workflowFrozenRuns.set(step.name ?? successorRunId, facts);
}
async function screenshot(name) {
  const visible = await cdp.evaluate("document.body.innerText + Array.from(document.querySelectorAll('input,textarea')).map(e => e.value).join('\\n')");
  if (secrets.some((secret) => visible.includes(secret))) throw new Error('当前页面含敏感值，拒绝保存截图');
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png' });
  const filename = String(name).replace(/[^a-zA-Z0-9_-]/g, '_');
  await writeFile(join(output, `${filename}.png`), Buffer.from(shot.data, 'base64'));
}

async function pageProbe(step) {
  if (!step.name || typeof step.expression !== 'string' || !step.expression.trim()) {
    throw new Error('页面 probe 缺少名称或表达式');
  }
  const value = await cdp.evaluate(step.expression);
  const { filename, bytes } = serializePageProbe(step.name, value, redact);
  await writeFile(join(output, `${filename}.json`), bytes);
  pageProbeObservations.set(step.name, {
    file: `${filename}.json`,
    sizeBytes: bytes.byteLength,
    sha256: createHash('sha256').update(bytes).digest('hex'),
  });
}

async function browserSurface(step) {
  // CDP 端口属于本次启动的原生程序；另连真实子 WebView 取证，而非用主页面
  // 的空占位 div 或地址栏文本冒充网页已经加载。此截图不等价于整窗合成截图。
  const expected = expand(step.url);
  const fixtureUrl = new URL(expected);
  if (!['http:', 'https:'].includes(fixtureUrl.protocol) || !['127.0.0.1', 'localhost'].includes(fixtureUrl.hostname)) {
    throw new Error('浏览器验收页面必须使用显式本地夹具地址');
  }
  const deadline = Date.now() + (step.timeoutMs ?? 30000);
  const readTargets = async () => (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json());
  const readCurrentTarget = async () => exactCdpTarget(await readTargets(), expected);
  const readSurfaceIdentity = async () => {
    const surfaces = await cdp.evaluate("window.__TAURI_INTERNALS__.invoke('browser_surface_state')");
    const matches = surfaces.filter((surface) => surface.url === expected);
    if (matches.length > 1) throw new Error('本地页面对应多个原生表面，不能确定 CDP target 身份');
    if (!matches[0]) return null;
    const surface = matches[0];
    if (typeof surface.tabId !== 'string' || !surface.tabId
        || !Number.isSafeInteger(surface.generation) || surface.generation <= 0) {
      throw new Error('原生浏览器表面缺少有效 tabId 或 generation');
    }
    return { tabId: surface.tabId, generation: surface.generation };
  };
  let replacementCount = 0;
  while (Date.now() < deadline) {
    const target = await waitForStableCdpTarget({
      readTargets, expected, deadline, pause,
    });
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    let child;
    let retryForReplacement = false;
    let surface;
    try {
      try {
        await new Promise((resolveSocket, rejectSocket) => {
          const onOpen = () => { socket.removeEventListener('error', onError); resolveSocket(); };
          const onError = (error) => { socket.removeEventListener('open', onOpen); rejectSocket(error); };
          socket.addEventListener('open', onOpen, { once: true });
          socket.addEventListener('error', onError, { once: true });
        });
      } catch (error) {
        const currentTarget = await readCurrentTarget().catch(() => null);
        retryForReplacement = Boolean(currentTarget && !sameCdpTarget(target, currentTarget));
        if (!retryForReplacement) throw error;
      }
      if (!retryForReplacement) {
        child = new Cdp(socket);
        // WebView2 可能在 websocket 建立后换代；同 URL 不代表仍是同一个 target。
        const connectedTarget = await readCurrentTarget();
        if (!connectedTarget) throw new Error('CDP target 在 websocket 建立后消失');
        if (!sameCdpTarget(target, connectedTarget)) retryForReplacement = true;
      }
      if (!retryForReplacement) {
        surface = await readSurfaceIdentity();
        while (!surface && Date.now() < deadline) {
          const currentTarget = await readCurrentTarget();
          if (!currentTarget) throw new Error('CDP target 在等待原生浏览器表面时消失');
          if (currentTarget && !sameCdpTarget(target, currentTarget)) {
            retryForReplacement = true;
            break;
          }
          await pause(Math.min(150, Math.max(0, deadline - Date.now())));
          surface = await readSurfaceIdentity();
        }
        if (!surface && !retryForReplacement) throw new Error('CDP target 没有对应的原生浏览器表面');
      }
      if (!retryForReplacement) {
        try {
          await child.send('Page.enable'); await child.send('Runtime.enable');
        } catch (error) {
          // 只有再次确认 target 已被生命周期替换时才重选；稳定 target 的超时原样抛出。
          const currentTarget = await readCurrentTarget().catch(() => null);
          retryForReplacement = Boolean(currentTarget && !sameCdpTarget(target, currentTarget));
          if (!retryForReplacement) throw error;
        }
      }
      if (!retryForReplacement) {
        const currentTarget = await readCurrentTarget();
        if (!currentTarget) throw new Error('CDP target 在启用协议后消失');
        if (currentTarget && !sameCdpTarget(target, currentTarget)) retryForReplacement = true;
      }
      if (!retryForReplacement) {
        const confirmedSurface = await readSurfaceIdentity();
        if (!confirmedSurface) throw new Error('CDP target 在启用协议后失去对应的原生浏览器表面');
        if (confirmedSurface.tabId !== surface.tabId || confirmedSurface.generation !== surface.generation) {
          retryForReplacement = true;
        } else {
          surface = confirmedSurface;
        }
      }
      if (!retryForReplacement) {
        // CDP target 的 URL 会先于文档正文更新；在原有预算内等待真实页面标记，
        // 避免把刚开始导航的空白文档误判成最终内容，也保存失败时实际载入的正文。
        let state;
        do {
          state = await child.evaluate("({url:location.href,title:document.title,text:document.body?.innerText??'',viewport:{width:innerWidth,height:innerHeight},deviceScaleFactor:devicePixelRatio})");
          if (state.text.includes(expand(step.contains))) break;
          await pause(150);
        } while (Date.now() < deadline);
        if (secrets.some((secret) => state.text.includes(secret))) throw new Error('子 WebView 含敏感值，拒绝保存截图');
        const filename = String(step.name ?? 'native-browser-surface').replace(/[^a-zA-Z0-9_-]/g, '_');
        await writeFile(join(output, `${filename}.json`), redact(JSON.stringify({
          ...state,
          nativeSurface: surface,
          cdpTargetId: cdpTargetIdentity(target),
        }, null, 2)));
        if (!state.text.includes(expand(step.contains))) throw new Error('子 WebView 内容断言失败');
        const shot = await child.send('Page.captureScreenshot', { format: 'png' });
        await writeFile(join(output, `${filename}.png`), Buffer.from(shot.data, 'base64'));
        // 可选动作仍是真实鼠标点击，用于验证网页链接触发 Rust new-window 事件，
        // 而不是直接调用浏览器 service 或篡改主界面的标签状态。
        if (step.clickSelector) await click(step.clickSelector, child);
      }
    } finally {
      child?.close();
      if (!child) socket.close();
    }
    if (!retryForReplacement) return;
    replacementCount += 1;
    if (replacementCount > 1) throw new Error('原生子 WebView target 在一次验收中重复换代');
  }
  throw new Error('原生子 WebView target 在验收预算内未稳定');
}

async function browserSurfaceState(step) {
  // 只读诊断从原生 COM 控制器读取实际可见性，不能以 DOM 占位状态代替。
  const expected = expand(step.url);
  const fixtureUrl = new URL(expected);
  if (!['http:', 'https:'].includes(fixtureUrl.protocol) || !['127.0.0.1', 'localhost'].includes(fixtureUrl.hostname)
      || typeof step.visible !== 'boolean') throw new Error('必须指定本地浏览器页面和布尔可见性');
  const deadline = Date.now() + (step.timeoutMs ?? 10000);
  let state;
  do {
    const surfaces = await cdp.evaluate("window.__TAURI_INTERNALS__.invoke('browser_surface_state')");
    const matches = surfaces.filter((surface) => surface.url === expected);
    if (matches.length > 1) throw new Error('本地页面对应多个原生表面，不能确定本次验收身份');
    state = matches[0];
    if (state?.visible === step.visible) break;
    await pause(150);
  } while (Date.now() < deadline);
  if (state?.visible !== step.visible) throw new Error('原生浏览器表面可见性断言失败');
  if (!state.owner?.workspaceKey || !state.owner?.sessionId
      || !Number.isSafeInteger(state.generation) || state.generation <= 0) {
    throw new Error('原生浏览器表面缺少有效 owner 或 generation');
  }
  // 同一表面的显隐不能悄悄切换身份；popup 必须继承业务 owner 并取得独立原生代次。
  const prior = browserSurfaceIdentities.get(expected);
  if (prior && (prior.generation !== state.generation
      || JSON.stringify(prior.owner) !== JSON.stringify(state.owner))) {
    throw new Error('浏览器显隐过程中原生表面的身份发生变化');
  }
  if (step.sameOwnerAs) {
    const related = browserSurfaceIdentities.get(step.sameOwnerAs);
    if (!related || JSON.stringify(related.owner) !== JSON.stringify(state.owner)
        || related.generation === state.generation) {
      throw new Error('popup 未继承同一 owner 或复用了原页面的 native generation');
    }
  }
  if (step.visible && !(state.physicalBounds.width > 0 && state.physicalBounds.height > 0)) {
    throw new Error('可见浏览器表面缺少有效物理尺寸');
  }
  const filename = String(step.name ?? 'native-browser-visibility').replace(/[^a-zA-Z0-9_-]/g, '_');
  browserSurfaceIdentities.set(expected, { owner: state.owner, generation: state.generation });
  if (step.name) browserSurfaceIdentities.set(step.name, { owner: state.owner, generation: state.generation });
  await writeFile(join(output, `${filename}.json`), redact(JSON.stringify(state, null, 2)));
}

// 多个原生窗口会争夺焦点并污染资源采样；不同端口也不能代表桌面验收已隔离。
// 原子租约只约束本 checkout 的测试，不影响用户程序；异常退出留下的租约须核对 PID 后清理。
const nativeLeasePath = join(tmpdir(), `keencode-native-live-${createHash('sha256').update(root.toLowerCase()).digest('hex').slice(0, 16)}.lock`);
const nativeLease = await open(nativeLeasePath, 'wx').catch((error) => {
  if (error.code === 'EEXIST') throw new Error('本项目已有原生验收租约，请等待该运行结束；异常退出须先核对租约 PID');
  throw error;
});
await nativeLease.writeFile(JSON.stringify({ pid: process.pid, binarySha256, output }));

try {
  // 启动时固定隔离根与测试快照，失败诊断无需猜测最近创建的临时目录。
  // 仅记录非敏感身份和摘要，供应商配置与模型请求正文不进入此文件。
  await writeFile(join(output, 'run-context.json'), JSON.stringify({
    binarySha256, planSha256, isolation, model, protocol: 'chat_completions',
    runtimeArtifactLimit: runtimeArtifactLimit ?? null,
  }, null, 2));
  await launch();
  if (!Array.isArray(plan.steps) || !plan.steps.length) throw new Error('必须提供真实界面验收步骤');
  for (let index = 0; index < plan.steps.length; index++) {
    const step = plan.steps[index]; const started = Date.now();
    try {
      switch (step.action) {
        case 'click': await click(step.selector); break;
        // 分组和任务 ContextMenu 必须由真实鼠标右键触发，不能 dispatch DOM 事件。
        case 'rightClick': await click(step.selector, cdp, 'right'); break;
        case 'clickIf': if (await cdp.evaluate(step.condition)) await click(step.selector); break;
        case 'clickText': await clickText(step.texts ?? step.text, step.selector, step.exact ?? true); break;
        case 'hover': await hover(step.selector); break;
        case 'captureTask': {
          if (!step.name || capturedTasks.has(step.name)) throw new Error('任务验收身份名称缺失或重复');
          const position = await elementPosition(step.selector);
          const id = position?.testId;
          if (!id?.startsWith('task-item-')) throw new Error('真实任务行缺少稳定身份');
          capturedTasks.set(step.name, id);
          break;
        }
        case 'clickCapturedTask': {
          const id = capturedTasks.get(step.name);
          if (!id) throw new Error('未捕获本次验收的真实任务身份');
          await click(`[data-testid=${JSON.stringify(id)}]`);
          break;
        }
        case 'type': {
          await click(step.selector);
          const text = expand(step.text);
          const typeTarget = await prepareContentEditableType(step.selector);
          await cdp.send('Input.insertText', { text });
          if (typeTarget) await waitForContentEditableText(step.selector, text, typeTarget.initialText);
          break;
        }
        case 'clear': await clearField(step.selector); break;
        case 'fileInput': {
          if (typeof step.path !== 'string' || step.path.trim().length === 0) {
            throw new Error('fileInput 缺少项目内夹具路径');
          }
          const target = resolve(project, step.path);
          if (!target.startsWith(project + '\\') && !target.startsWith(project + '/')) {
            throw new Error('fileInput 路径越界');
          }
          // 通过 CDP 给真实 input[type=file] 注入项目隔离夹具，仍触发来源组件的
          // 原生 change 事件；禁止把用户机器上的任意路径带入验收流程。
          await readFile(target);
          const document = await cdp.send('DOM.getDocument', { depth: -1 });
          const node = await cdp.send('DOM.querySelector', {
            nodeId: document.root.nodeId,
            selector: step.selector,
          });
          if (!node.nodeId) throw new Error('fileInput 未找到真实文件控件');
          await cdp.send('DOM.setFileInputFiles', { nodeId: node.nodeId, files: [target] });
          break;
        }
        case 'key': {
          // WebView2 的编辑动作需要真实虚拟键码，key 文本本身不能代表删除或导航已执行。
          const keyCode = { Enter: 13, Escape: 27, Backspace: 8, Delete: 46, Tab: 9,
            ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Home: 36, End: 35 }[step.key];
          if (!keyCode) throw new Error('不支持的原生验收按键');
          const key = { key: step.key, code: step.key, windowsVirtualKeyCode: keyCode };
          // HTML 表单的隐式提交需要 Enter 字符事件；只有 rawKeyDown 不会触发
          // 地址栏的原生 submit。PTY 的 textarea 和应用快捷键继续只接收控制键，
          // 避免额外字符造成重复命令或重复审批。
          const submitInput = step.key === 'Enter' && await cdp.evaluate(
            "document.activeElement instanceof HTMLInputElement && !!document.activeElement.form",
          );
          await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...key });
          if (submitInput) await cdp.send('Input.dispatchKeyEvent', {
            type: 'char', ...key, text: '\r', unmodifiedText: '\r',
          });
          await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...key });
          break;
        }
        case 'assert': if (!await cdp.evaluate(step.expression)) throw new Error('界面断言失败'); break;
        case 'wait': await waitFor(step.expression, step.timeoutMs, step.approvePermissions, index);
          if (index === 0 && firstInteractiveMs === undefined) firstInteractiveMs = Date.now() - launchStarted;
          break;
        case 'waitTurnInFlightAndStop': await waitForTurnInFlightAndStop(step, index); break;
        case 'measure': await measure(step.name ?? 'native', step.durationMs, step.settleMs, step.minimumActiveActors, step.activeSessionCount, workflowRunId(step)); break;
        case 'heapEvidence': await heapEvidence(step); break;
        case 'runtimeEvidence': await runtimeEvidence(step); break;
        case 'cpuProfile': await cpuProfile(step); break;
        case 'projectFileChange': await projectFileChange(step); break;
        case 'captureWorkflowRun': await captureWorkflowRun(step, index); break;
        case 'assertWorkflowBindings': await assertWorkflowBindings(step); break;
        case 'assertWorkflowSuccessor': await assertWorkflowSuccessor(step); break;
        case 'setup': await cdp.evaluate(step.expression); break;
        case 'diagnosticBaseline': await captureDiagnosticBaseline(step); break;
        case 'editorLaunchDiagnostic': await waitForEditorLaunchDiagnostic(step); break;
        case 'screenshot': await screenshot(step.name); break;
        case 'pageProbe': await pageProbe(step); break;
        case 'appearanceLayout': {
          // 只采集固定外观页的 DOM/CSS 几何，不读取表单值、配置或会话内容。
          const layout = await cdp.evaluate(`(() => { const root=document.querySelector('[data-testid=settings-page][data-active-section=appearance]'); if(!root)throw new Error('外观页尚未打开'); const rect=e=>{const r=e.getBoundingClientRect();const s=getComputedStyle(e);return {x:r.x,y:r.y,width:r.width,height:r.height,background:s.backgroundColor,border:s.borderColor,radius:s.borderRadius,fontSize:s.fontSize};};return {viewport:{width:innerWidth,height:innerHeight},deviceScaleFactor:devicePixelRatio,classes:document.documentElement.className,page:rect(root),cards:[...root.querySelectorAll('[data-slot=card]')].map(rect),headings:[...root.querySelectorAll('h1,h2,h3')].map(e=>({text:e.textContent,...rect(e)}))};})()`);
          await writeFile(join(output, 'appearance-layout.json'), JSON.stringify(layout, null, 2));
          break;
        }
        case 'browserSurface': await browserSurface(step); break;
        case 'browserSurfaceState': await browserSurfaceState(step); break;
        case 'archiveEvidence': await waitForArchiveEvidence(step); break;
        case 'nativeExited': await waitForNativeExit(step); break;
        case 'pdfEvidence':
        case 'pdfValidate': await pdfEvidence(step); break;
        case 'jsonEvidence': await jsonEvidence(step); break;
        case 'restart':
          if (Object.hasOwn(step, 'requestTimeoutMs')) configureRequestTimeout(step.requestTimeoutMs);
          await stop(); await launch(); break;
        case 'pageReload':
          // 只重载当前 renderer 页面；不重启 Rust 进程、不调用业务 RPC，也不注入会话状态。
          // 计划随后必须从隔离 Journal 读取同一 session/turn 的权威事实。
          await cdp.send('Page.reload', { ignoreCache: false });
          break;
        case 'file': {
          const target = resolve(project, step.path); if (!target.startsWith(project + '\\') && !target.startsWith(project + '/')) throw new Error('断言路径越界');
          if (!(await readFile(target, 'utf8')).includes(expand(step.contains))) throw new Error('真实项目文件断言失败'); break;
        }
        case 'journal': {
          if (!journalMatches(scopedJournalEvents(await journalRecords(), step), step)) throw new Error('权威 Journal 事件断言失败'); break;
        }
        case 'journalSelectionSideIdentity': {
          const identity = await waitForSelectionSideIdentity(step);
          const name = step.name ?? 'selection-side-identity';
          if (selectionSideIdentities.has(name)) throw new Error(`selection side identity 名称重复: ${name}`);
          selectionSideIdentities.set(name, identity);
          break;
        }
        case 'journalBaseline': {
          if (!step.name || !step.eventType) throw new Error('Journal 基线需要名称和事件类型');
          const allEvents = scopedJournalEvents(await journalRecords(), step);
          const events = allEvents.filter((event) => event.type === step.eventType);
          const interactionId = step.capturePermissionInteraction
            ? await cdp.evaluate("document.querySelector('[data-permission-option-kind=rejectOnce][data-permission-request-id]')?.getAttribute('data-permission-request-id') ?? null")
            : undefined;
          if (step.capturePermissionInteraction && !interactionId) {
            throw new Error('权限 Journal 基线缺少当前 interactionId');
          }
          journalBaselines.set(step.name, {
            eventType: step.eventType,
            commandIds: new Set(events.map((event) => event.payload?.receipt?.commandId).filter(Boolean)),
            interactionId,
            sequenceBySession: step.captureEventSequence ? journalSequenceBaseline(allEvents) : undefined,
          });
          break;
        }
        case 'journalCapture':
        case 'journalUnchanged': {
          // UI 行数相同不能证明没有副作用重放；直接比较同类权威事件的稳定摘要。
          if (!step.name || !step.eventType) throw new Error('Journal 比较需要名称和事件类型');
          const events = journalCaptureEvents(await journalRecords(), step);
          if (!events.length) throw new Error('缺少当前范围的权威 Journal 事件，不能建立恢复基线');
          if (step.exactCount !== undefined && events.length !== step.exactCount) {
            throw new Error('Journal 事实数量与计划绑定不一致');
          }
          const canonical = events.map((event) => JSON.stringify(event)).sort().join('\n');
          const sequenceBySession = new Map();
          for (const event of events) sequenceBySession.set(event.session, Math.max(sequenceBySession.get(event.session) ?? 0, event.sequence));
          const snapshot = { eventType: step.eventType, runId: workflowRunId(step) ?? null, count: events.length,
            sha256: createHash('sha256').update(canonical).digest('hex'),
            sequenceBySession: Object.fromEntries(sequenceBySession) };
          if (step.action === 'journalCapture') journalSnapshots.set(step.name, snapshot);
          else if (JSON.stringify(journalSnapshots.get(step.name)) !== JSON.stringify(snapshot)) {
            throw new Error('冷恢复期间权威 Journal 事件发生变化，不能确认完成节点未重放');
          }
          break;
        }
        case 'journalWait': {
          const deadline = Date.now() + (step.timeoutMs ?? 60000);
          while (!journalMatches(scopedJournalEvents(await journalRecords(), step), step)) {
            if (Date.now() >= deadline) throw new Error('等待真实 Journal 事件超时');
            if (step.approvePermissions) await approveVisiblePermission(index);
            await pause(200);
          }
          break;
        }
        case 'journalAbsent': {
          // 用于验证 renderer reload 没有把正在运行的同一 Turn 误转成取消。
          // 这是只读 Journal 断言，不能代替成功终态和同一 turn_id 的正向断言。
          if (journalMatches(scopedJournalEvents(await journalRecords(), step), step)) {
            throw new Error('权威 Journal 出现计划要求不存在的事件');
          }
          break;
        }
        default: throw new Error('未知验收动作');
      }
      results.push({ index, description: step.description ?? step.action, passed: true, elapsedMs: Date.now() - started });
      // 长时间的原生验收逐步报告实际完成点，避免模型请求期间只得到最终汇总。
      console.log(redact(JSON.stringify({ step: index, passed: true, description: step.description ?? step.action })));
    } catch (error) {
      results.push({ index, description: step.description ?? step.action, passed: false, error: redact(error.message) });
      console.log(redact(JSON.stringify({ step: index, passed: false, error: error.message })));
      throw error;
    }
  }
} catch (error) {
  process.exitCode = 1;
  results.push({ passed: false, error: redact(error.message) });
  if (cdp) {
    // 等待失败也保存 CPU 栈，避免最慢的路径恰好没有 profiling 证据。
    if (activeCpuProfile) {
      try { await cpuProfile({ phase: 'stop', name: 'failure-active' }); } catch { /* 保留原失败 */ }
    }
    try {
      // 失败快照保留真实 DOM 的路由/编辑状态，避免仅凭可见文案推测 ready。
      const state = await cdp.evaluate(`({text:document.body.innerText, controls:[...document.querySelectorAll('button,input,textarea,[contenteditable],[data-testid="v4-composer"],[data-testid="v4-model-config"],[role="tabpanel"],[data-side-pane-tab-id]')].map(e=>({tag:e.tagName,id:e.dataset.testid,label:e.getAttribute('aria-label'),text:e.textContent?.slice(0,100),disabled:e.disabled,attributes:Object.fromEntries(['contenteditable','data-e2e-lexical-bridge','data-input-routing','data-provider','data-model','data-state','data-side-pane-tab-id','aria-controls','aria-hidden'].map(key=>[key,e.getAttribute(key)]).filter(([,value])=>value!==null))}))})`);
      await writeFile(join(output, 'failure-state.json'), redact(JSON.stringify(state, null, 2)));
      await screenshot('failure');
    } catch { /* 失败页面已关闭时保留原始失败，不覆盖为截图失败。 */ }
  }
} finally {
  try {
  await stop();
  let diagnostics = '';
  try {
    diagnostics = await readFile(join(data, 'logs', 'keencode-desktop.log'), 'utf8');
    await writeFile(join(output, 'diagnostics.log'), redact(diagnostics.slice(-128 * 1024)));
  } catch (error) {
    if (error.code !== 'ENOENT') console.error(redact(`读取隔离诊断失败: ${error.message}`));
  }
  // 页面可能捕获 RPC 异常并只写宿主诊断；这种情况下操作步骤通过也不能交付。
  const protocolFaults = nativeProtocolFaults(diagnostics).map(redact);
  if (protocolFaults.length) {
    results.push({ passed: false, error: '隔离诊断包含前端协议故障', protocolFaultCount: protocolFaults.length });
    process.exitCode = 1;
  }
  const frontendFaults = nativeFrontendFaults(frontendErrors);
  if (frontendFaults.length) {
    results.push({ passed: false, error: '原生界面包含未处理的前端异常', frontendFaultCount: frontendFaults.length });
    process.exitCode = 1;
  }
  const report = { passed: process.exitCode !== 1, model, protocol: 'chat_completions', native: nativeConnected,
    runtimeArtifactLimit: runtimeArtifactLimit ?? null, runtimeObservations, cpuProfiles,
    sourceBaseline: '29628c9acdb81b703bbd4080c207a0e7ce5e276e', uiEnvironment,
    binary, binarySha256, planSha256, isolation, elapsedMs: Date.now() - measureStart, firstInteractiveMs, measurements, heapObservations, nativeHostProbes, permissionApprovals,
    journalSnapshots: Object.fromEntries(journalSnapshots),
    journalBaselines: Object.fromEntries([...journalBaselines].map(([name, baseline]) => [name, {
      eventType: baseline.eventType, commandIds: [...baseline.commandIds], interactionId: baseline.interactionId ?? null,
      ...(baseline.sequenceBySession ? { sequenceBySession: Object.fromEntries(baseline.sequenceBySession) } : {}),
    }])),
    workflowRuns: Object.fromEntries(workflowRuns),
    workflowBindings: Object.fromEntries(workflowBindings), workflowFrozenRuns: Object.fromEntries(workflowFrozenRuns),
    capturedTasks: Object.fromEntries(capturedTasks),
    journalBindings: Object.fromEntries(journalValues), selectionSideIdentities: Object.fromEntries(selectionSideIdentities),
    archiveObservations: Object.fromEntries(archiveObservations),
    binaryFixtureObservations, pdfObservations: Object.fromEntries(pdfObservations),
    jsonObservations: Object.fromEntries(jsonObservations), pageProbeObservations: Object.fromEntries(pageProbeObservations), nativeExitObservations,
    requestTimeoutHistory, diagnosticObservations: Object.fromEntries(diagnosticObservations), protocolFaults, frontendFaults, results };
  await writeFile(join(output, 'report.json'), redact(JSON.stringify(report, null, 2)));
  await writeFile(join(output, 'application.log'), redact(applicationOutput));
  await writeFile(join(output, 'frontend-errors.json'), JSON.stringify(frontendErrors, null, 2));
  console.log(JSON.stringify({ passed: report.passed, report: join(output, 'report.json'), completedSteps: results.filter((result) => result.passed).length }));
  } finally {
    await nativeLease.close();
    await unlink(nativeLeasePath);
  }
}
