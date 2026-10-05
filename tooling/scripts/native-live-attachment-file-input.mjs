#!/usr/bin/env node
/**
 * 通过真实 WebView2 CDP 文件输入注入附件，再委托现有原生验收 runner 执行计划。
 * 文件输入事件必须由 DOM.setFileInputFiles 触发，不能用页面脚本伪造 File 对象。
 */
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { journalEvents } from './native-live-journal.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const runner = join(root, 'tooling/scripts/native-live-e2e.mjs');
const pause = (milliseconds) => new Promise((done) => setTimeout(done, milliseconds));

function parseFlags(argv) {
  const flags = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith('--') || !value || ![
      '--plan', '--provider-config', '--binary', '--output', '--port', '--attachment-file',
    ].includes(name)) {
      throw new Error('需要 --plan 和 --provider-config，可选 --binary、--output、--port、--attachment-file');
    }
    flags.set(name, value);
  }
  return flags;
}

const flags = parseFlags(process.argv.slice(2));
const required = (name) => {
  const value = flags.get(name);
  if (!value) throw new Error(`缺少 ${name}`);
  return resolve(value);
};

const planPath = required('--plan');
const providerConfig = required('--provider-config');
const plan = JSON.parse(await readFile(planPath, 'utf8'));
const attachment = plan.attachment;
if (!attachment || typeof attachment.fileName !== 'string' || typeof attachment.marker !== 'string'
    || typeof attachment.content !== 'string' || attachment.content.includes(attachment.marker) === false) {
  throw new Error('计划必须声明包含唯一 marker 的 UTF-8 附件夹具');
}
if (!attachment.fileName || attachment.fileName !== attachment.fileName.split(/[\\/]/).pop()) {
  throw new Error('附件文件名不能包含目录');
}

const output = resolve(flags.get('--output') ?? join(root, 'out/native-live', `attachment-${randomUUID()}`));
await mkdir(output, { recursive: true });
const attachmentPath = flags.get('--attachment-file')
  ? resolve(flags.get('--attachment-file'))
  : join(output, attachment.fileName);
const attachmentStat = await (async () => {
  if (flags.get('--attachment-file')) {
    const existing = await stat(attachmentPath);
    if (!existing.isFile()) throw new Error('--attachment-file 必须是普通文件');
    if (attachmentPath.split(/[\\/]/).pop() !== attachment.fileName) {
      throw new Error('--attachment-file 的文件名必须与计划一致');
    }
    const bytes = await readFile(attachmentPath);
    if (!bytes.toString('utf8').includes(attachment.marker)) {
      throw new Error('--attachment-file 不包含计划要求的 marker');
    }
    return { bytes: bytes.length, generated: false };
  }
  const bytes = Buffer.from(attachment.content, 'utf8');
  await writeFile(attachmentPath, bytes);
  return { bytes: bytes.length, generated: true };
})();

const port = Number(flags.get('--port') ?? 9238);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('CDP 端口无效');

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.sequence = 0;
    this.pending = new Map();
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(String(data));
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }

  send(method, params = {}) {
    const id = ++this.sequence;
    return new Promise((resolveResult, rejectResult) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectResult(new Error(`CDP 超时: ${method}`));
      }, 20_000);
      this.pending.set(id, { resolve: resolveResult, reject: rejectResult, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (result?.exceptionDetails) {
      throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    }
    return result?.result?.value;
  }

  close() {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(new Error('CDP 已关闭'));
    }
    this.pending.clear();
    this.socket.close();
  }
}

async function connectPage() {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = targets.find((target) => target.type === 'page'
    && /tauri\.localhost|127\.0\.0\.1:1421/.test(target.url));
  if (!page) return null;
  const socket = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolveSocket, rejectSocket) => {
    socket.addEventListener('open', resolveSocket, { once: true });
    socket.addEventListener('error', rejectSocket, { once: true });
  });
  const cdp = new Cdp(socket);
  await cdp.send('Runtime.enable');
  await cdp.send('DOM.enable');
  return { cdp, url: page.url };
}

async function waitForAttachmentChip(cdp) {
  const fileName = JSON.stringify(attachment.fileName);
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const ready = await cdp.evaluate(`Array.from(document.querySelectorAll('[data-composer-attachment-kind]')).some((element) => {
      return element.textContent?.includes(${fileName}) && element.getAttribute('data-upload-status') !== 'failed';
    })`);
    if (ready) return true;
    await pause(100);
  }
  return false;
}

async function injectAttachment(child) {
  const deadline = Date.now() + 60_000;
  let lastError;
  let injectedOnce = false;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error('桌面程序在文件输入出现前退出');
    let connected;
    try {
      connected = await connectPage();
      if (!connected) {
        await pause(200);
        continue;
      }
      const { cdp } = connected;
      try {
        const document = await cdp.send('DOM.getDocument', { depth: -1, pierce: true });
        const rootNodeId = document?.root?.nodeId;
        if (!rootNodeId) throw new Error('CDP 未返回页面根节点');
        const input = await cdp.send('DOM.querySelector', {
          nodeId: rootNodeId,
          selector: 'input[type="file"]',
        });
        if (!input?.nodeId) {
          await pause(200);
          continue;
        }
        await cdp.send('DOM.setFileInputFiles', {
          nodeId: input.nodeId,
          files: [attachmentPath],
        });
        injectedOnce = true;
        if (!await waitForAttachmentChip(cdp)) {
          throw new Error('文件输入已设置，但 Composer 未出现附件 chip');
        }
        return { targetUrl: connected.url, injected: true };
      } finally {
        cdp.close();
      }
    } catch (error) {
      lastError = error;
      if (injectedOnce) throw error;
      await pause(200);
    }
  }
  throw new Error(`等待真实 Composer 文件输入超时: ${lastError?.message ?? '未发现 input[type=file]'}`);
}

async function waitForChild(child) {
  return new Promise((resolveResult) => {
    if (child.exitCode !== null) {
      resolveResult({ code: child.exitCode, signal: child.signalCode });
      return;
    }
    child.once('exit', (code, signal) => resolveResult({ code, signal }));
    child.once('error', (error) => resolveResult({ code: null, signal: null, error }));
  });
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return;
  child.kill();
  await pause(500);
  if (child.exitCode !== null || !Number.isInteger(child.pid)) return;
  await new Promise((done) => {
    const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], {
      windowsHide: true,
      stdio: 'ignore',
    });
    killer.once('exit', done);
    killer.once('error', done);
  });
}

async function journalRecords(dataRoot) {
  const found = [];
  async function walk(path) {
    for (const entry of await readdir(path, { withFileTypes: true })) {
      const file = join(path, entry.name);
      if (entry.isDirectory() && !entry.isSymbolicLink()) await walk(file);
      else if (entry.name === 'events.jsonl') {
        const lines = (await readFile(file, 'utf8')).split('\n').slice(0, -1).filter(Boolean);
        for (const line of lines) found.push(JSON.parse(line));
      }
    }
  }
  await walk(dataRoot);
  return found;
}

function persistedUserMessages(events) {
  return events.flatMap((event) => {
    if (event.type === 'message_added' && event.payload?.message) {
      return [{ event, message: event.payload.message }];
    }
    if (event.type === 'transcript_segment_committed' && Array.isArray(event.payload?.segment?.messages)) {
      return event.payload.segment.messages.map((message) => ({ event, message }));
    }
    return [];
  }).filter(({ message }) => message?.role === 'user' && message?.isMeta !== true);
}

async function inspectJournal(report) {
  if (!report?.isolation || typeof report.isolation !== 'string') {
    return { passed: false, error: '原生 runner 未提供隔离根路径' };
  }
  const data = resolve(report.isolation, 'data');
  const assetRoot = resolve(data, 'ui-attachments').toLowerCase();
  let events;
  try {
    events = journalEvents(await journalRecords(data));
  } catch (error) {
    return { passed: false, error: `Journal schema 校验失败: ${error.message}` };
  }
  const candidates = persistedUserMessages(events);
  const matches = candidates.filter(({ message }) => message.references?.some((reference) => {
    if (reference?.name !== attachment.fileName || typeof reference.path !== 'string') return false;
    if (reference.path.includes('://')) return false;
    const path = resolve(reference.path).toLowerCase();
    return path.startsWith(`${assetRoot}\\`) || path.startsWith(`${assetRoot}/`);
  }));
  return {
    passed: matches.length > 0,
    eventCount: events.length,
    userMessageCount: candidates.length,
    expectedFileName: attachment.fileName,
    expectedMarker: attachment.marker,
    matchedUserMessages: matches.map(({ event, message }) => ({
      session: event.session,
      sequence: event.sequence,
      messageId: message.messageId,
      role: message.role,
      references: message.references.map((reference) => ({
        name: reference.name,
        pathKind: typeof reference.path === 'string' && reference.path.includes('://') ? 'uri' : 'local',
        pathUnderIsolatedAttachmentStore: typeof reference.path === 'string'
          && (resolve(reference.path).toLowerCase().startsWith(`${assetRoot}\\`)
            || resolve(reference.path).toLowerCase().startsWith(`${assetRoot}/`)),
      })),
    })),
    error: matches.length > 0 ? undefined : '未找到带隔离 ui-attachments 引用的真实用户消息',
  };
}

const childArgs = [runner, '--plan', planPath, '--provider-config', providerConfig, '--output', output, '--port', String(port)];
for (const name of ['--binary']) {
  if (flags.has(name)) childArgs.push(name, resolve(flags.get(name)));
}
const child = spawn(process.execPath, childArgs, {
  cwd: root,
  windowsHide: true,
  stdio: 'inherit',
});
let injection;
let injectionError;
try {
  injection = await injectAttachment(child);
} catch (error) {
  injectionError = error;
  await stopChild(child);
}
const childResult = await waitForChild(child);

let runnerReport;
try {
  runnerReport = JSON.parse(await readFile(join(output, 'report.json'), 'utf8'));
} catch (error) {
  runnerReport = { passed: false, error: `无法读取 runner report.json: ${error.message}` };
}
const journal = await inspectJournal(runnerReport);
const result = {
  passed: !injectionError && childResult.code === 0 && runnerReport.passed === true && journal.passed === true,
  attachment: {
    fileName: attachment.fileName,
    bytes: attachmentStat.bytes,
    generated: attachmentStat.generated,
    marker: attachment.marker,
  },
  cdp: injection ?? { injected: false },
  runner: {
    exitCode: childResult.code,
    signal: childResult.signal,
    report: join(output, 'report.json'),
    binarySha256: runnerReport.binarySha256,
  },
  journal,
  ...(injectionError ? { error: injectionError.message } : {}),
};
await writeFile(join(output, 'attachment-journal-check.json'), JSON.stringify(result, null, 2));
console.log(JSON.stringify({
  passed: result.passed,
  report: join(output, 'attachment-journal-check.json'),
  injected: result.cdp.injected === true,
  journalReference: journal.passed === true,
}));
if (!result.passed) process.exitCode = 1;
