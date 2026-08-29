#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, statSync } from "node:fs";
import { cpus, platform, release, tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { summarizeBenchmark } from "./benchmark-statistics.mjs";

function positiveInteger(flag, fallback) {
  const index = process.argv.indexOf(flag);
  if (index < 0) return fallback;
  const value = Number.parseInt(process.argv[index + 1] ?? "", 10);
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${flag} 必须是正整数`);
  return value;
}

function option(flag) {
  const index = process.argv.indexOf(flag);
  return index < 0 ? null : process.argv[index + 1] ?? null;
}

const binaryArg = option("--binary");
if (!binaryArg) throw new Error("缺少 --binary <已构建的 KeenCode 二进制路径>");
const binary = isAbsolute(binaryArg) ? binaryArg : resolve(binaryArg);
if (!statSync(binary).isFile()) throw new Error(`二进制不存在：${binary}`);

const coldRuns = positiveInteger("--cold-runs", 10);
const warmRuns = positiveInteger("--warm-runs", 30);
const timeoutMs = positiveInteger("--timeout-ms", 30_000);
const scratch = mkdtempSync(join(tmpdir(), "keencode-startup-benchmark-"));

function sample(dataDir) {
  mkdirSync(dataDir, { recursive: true });
  return new Promise((resolveSample, reject) => {
    const child = spawn(binary, [], {
      env: {
        ...process.env,
        KEENCODE_BENCHMARK: "1",
        KEENCODE_BENCHMARK_DATA_DIR: dataDir,
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    let stderr = "";
    let measurement = null;
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`等待 frontend_interactive 超时（${timeoutMs}ms）`));
    }, timeoutMs);

    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
      const lines = stderr.split("\n");
      stderr = lines.pop() ?? "";
      for (const line of lines) {
        let event;
        try {
          event = JSON.parse(line);
        } catch {
          continue;
        }
        if (event.event !== "frontend_interactive" || !Number.isFinite(event.elapsedMs)) continue;
        clearTimeout(timer);
        measurement = event.elapsedMs;
        child.kill();
        return;
      }
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      if (measurement !== null) {
        resolveSample(measurement);
        return;
      }
      reject(new Error(`KeenCode 在可交互事件前退出：${code}`));
    });
  });
}

async function measure() {
  const cold = [];
  const warm = [];
  const dataDir = join(scratch, "warm");
  await sample(dataDir);
  for (let index = 0; index < Math.max(coldRuns, warmRuns); index += 1) {
    // 交替冷热样本，避免系统负载、温度和文件缓存随时间漂移污染分组结果。
    if (index < coldRuns) cold.push(await sample(join(scratch, `cold-${index}`)));
    if (index < warmRuns) warm.push(await sample(dataDir));
  }
  return {
    cold: summarizeBenchmark(cold),
    warm: summarizeBenchmark(warm),
  };
}

try {
  const git = spawnSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" });
  const measurements = await measure();
  const result = {
    environment: {
      commit: git.status === 0 ? git.stdout.trim() : "unknown",
      os: `${platform()} ${release()}`,
      arch: process.arch,
      cpu: cpus()[0]?.model ?? "unknown",
      logicalCpus: cpus().length,
      binary,
      binaryBytes: statSync(binary).size,
    },
    methodology: {
      coldRuns,
      warmRuns,
      coldDefinition: "每次使用新的 KeenCode 数据目录；不清理操作系统文件缓存",
      warmDefinition: "预热一次后复用同一 KeenCode 数据目录",
      milestone: "第二个 requestAnimationFrame 后前端 IPC 到达后端",
    },
    ...measurements,
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
