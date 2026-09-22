import { spawn } from "node:child_process";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const MAX_CONCURRENCY = 6;
let temporarySequence = 0;

function fail(message) {
  throw new Error(message);
}

export function validateManifest(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("manifest 必须是对象");
  const allowed = new Set(["outputDirectory", "concurrency", "runner", "defaults", "tasks"]);
  for (const key of Object.keys(value)) if (!allowed.has(key)) fail(`manifest 包含未知字段: ${key}`);
  if (!isAbsolute(value.outputDirectory ?? "")) fail("outputDirectory 必须是绝对路径");
  const concurrency = value.concurrency ?? MAX_CONCURRENCY;
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > MAX_CONCURRENCY) {
    fail(`concurrency 必须是 1-${MAX_CONCURRENCY} 的整数`);
  }
  if (!Array.isArray(value.tasks) || value.tasks.length === 0) fail("tasks 不能为空");
  const ids = new Set();
  for (const [index, task] of value.tasks.entries()) {
    if (!task || typeof task !== "object" || Array.isArray(task)) fail(`tasks[${index}] 必须是对象`);
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(task.id ?? "")) fail(`tasks[${index}].id 无效`);
    if (ids.has(task.id)) fail(`任务 id 重复: ${task.id}`);
    ids.add(task.id);
    if (!isAbsolute(task.cwd ?? "")) fail(`任务 ${task.id} 的 cwd 必须是绝对路径`);
    if (!Array.isArray(task.prompts) || task.prompts.length === 0 || task.prompts.some((p) => typeof p !== "string" || !p.trim())) {
      fail(`任务 ${task.id} 的 prompts 必须是非空字符串数组`);
    }
  }
  return { ...value, concurrency };
}

async function atomicJson(path, value) {
  const temporary = `${path}.${process.pid}.${temporarySequence++}.tmp`;
  await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  await rename(temporary, path);
}

export function serializedJsonWriter(path) {
  let chain = Promise.resolve();
  return (value) => {
    const snapshot = structuredClone(value);
    chain = chain.then(() => atomicJson(path, snapshot));
    return chain;
  };
}

function runProcess(binary, request, stdoutPath, stderrPath, environment) {
  return new Promise((resolveProcess) => {
    const detached = process.platform !== "win32";
    const child = spawn(binary, [], {
      detached,
      env: environment,
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout = [];
    const stderr = [];
    let hardTimedOut = false;
    const stopTree = (signal) => {
      try {
        if (detached && child.pid) process.kill(-child.pid, signal);
        else child.kill(signal);
      } catch {}
    };
    const hardTimeout = setTimeout(() => {
      hardTimedOut = true;
      stopTree("SIGTERM");
      setTimeout(() => stopTree("SIGKILL"), 5_000).unref();
    }, request.timeoutMs + 30_000);
    hardTimeout.unref();
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.on("error", (error) => {
      clearTimeout(hardTimeout);
      resolveProcess({ exitCode: null, signal: null, error: String(error), hardTimedOut, stdout, stderr });
    });
    child.on("close", (exitCode, signal) => {
      clearTimeout(hardTimeout);
      resolveProcess({ exitCode, signal, error: null, hardTimedOut, stdout, stderr });
    });
    child.stdin.end(JSON.stringify(request));
  }).then(async (result) => {
    const stdout = Buffer.concat(result.stdout);
    const stderr = Buffer.concat(result.stderr);
    await Promise.all([
      writeFile(stdoutPath, stdout, { mode: 0o600 }),
      writeFile(stderrPath, stderr, { mode: 0o600 }),
    ]);
    return { ...result, stdout: stdout.toString("utf8"), stderr: stderr.toString("utf8") };
  });
}

async function main() {
  const manifestPath = process.argv[2];
  if (!manifestPath) fail("用法: node scripts/benchmark-batch.mjs /absolute/path/manifest.json");
  if (!process.env.KEENCODE_BENCH_API_KEY) fail("缺少 KEENCODE_BENCH_API_KEY");
  const manifest = validateManifest(JSON.parse(await readFile(resolve(manifestPath), "utf8")));
  const output = manifest.outputDirectory;
  await mkdir(output, { recursive: false, mode: 0o700 });
  const binary = manifest.runner ?? resolve("target/debug/examples/keencode-bench");
  if (!isAbsolute(binary)) fail("runner 必须是绝对路径");
  const summary = {
    schemaVersion: 1,
    startedAt: new Date().toISOString(),
    finishedAt: null,
    concurrency: manifest.concurrency,
    runner: binary,
    tasks: [],
  };
  await atomicJson(join(output, "summary.json"), summary);
  const writeSummary = serializedJsonWriter(join(output, "summary.json"));
  let cursor = 0;
  async function worker() {
    while (cursor < manifest.tasks.length) {
      const task = manifest.tasks[cursor++];
      const taskRoot = join(output, task.id);
      const storage = join(taskRoot, "runtime");
      await mkdir(taskRoot, { recursive: false, mode: 0o700 });
      const request = { ...manifest.defaults, ...task, storage };
      delete request.id;
      const startedAt = new Date();
      await atomicJson(join(taskRoot, "request.json"), { ...request, apiKey: undefined });
      const processResult = await runProcess(
        binary,
        request,
        join(taskRoot, "process.stdout.log"),
        join(taskRoot, "process.stderr.log"),
        process.env,
      );
      let runnerResult = null;
      try {
        runnerResult = JSON.parse(processResult.stdout.trim());
      } catch {}
      const record = {
        id: task.id,
        cwd: task.cwd,
        startedAt: startedAt.toISOString(),
        finishedAt: new Date().toISOString(),
        elapsedMs: Date.now() - startedAt.getTime(),
        exitCode: processResult.exitCode,
        signal: processResult.signal,
        spawnError: processResult.error,
        hardTimedOut: processResult.hardTimedOut,
        status:
          processResult.exitCode === 0 && runnerResult
            ? "completed"
            : processResult.hardTimedOut || processResult.exitCode === 124
              ? "timed_out"
              : "failed",
        result: runnerResult,
        evidenceDirectory: taskRoot,
      };
      summary.tasks.push(record);
      await atomicJson(join(taskRoot, "result.json"), record);
      await writeSummary(summary);
      process.stderr.write(`[${summary.tasks.length}/${manifest.tasks.length}] ${task.id}: ${record.status}\n`);
    }
  }
  await Promise.all(Array.from({ length: Math.min(manifest.concurrency, manifest.tasks.length) }, () => worker()));
  summary.finishedAt = new Date().toISOString();
  await writeSummary(summary);
  const failed = summary.tasks.filter((task) => task.status !== "completed").length;
  process.stderr.write(`完成 ${summary.tasks.length} 题，Runner 失败或超时 ${failed} 题。\n`);
  process.exitCode = failed === 0 ? 0 : 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error}\n`);
    process.exitCode = 1;
  });
}
