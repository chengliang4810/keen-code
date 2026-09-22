#!/usr/bin/env node

/**
 * 隔离验证本地 CLI/headless Host 的跨进程生命周期。
 *
 * 默认模式不允许 Provider 环境变量进入测试 Host，因此不会触发真实模型请求。
 * --model 只在调用方明确选择时继承当前进程的 Provider 环境；脚本不把这些值
 * 放进参数、临时文件、日志或错误文本，也不转发 CLI 原始 stdout/stderr。
 */

import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import { randomBytes } from "node:crypto";
import { request as httpRequest } from "node:http";
import { createServer, connect as tcpConnect } from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DISCOVERY_FILE = "host.endpoint.json";
const PROVIDER_ENV_NAMES = [
  "KEENCODE_PROVIDER_BASE_URL",
  "KEENCODE_PROVIDER_MODEL",
  "KEENCODE_PROVIDER_API_KEY",
  "KEENCODE_PROVIDER_PROTOCOL",
  "KEENCODE_PROVIDER_RESPONSE_MODE",
  "OPENAI_BASE_URL",
  "OPENAI_MODEL",
  "OPENAI_API_KEY",
];

const DEFAULT_COMMAND_TIMEOUT_MS = 15_000;
const HOST_START_TIMEOUT_MS = 10_000;
const HOST_IDLE_TIMEOUT_MS = 40_000;
const OUTPUT_LIMIT_BYTES = 4 * 1024 * 1024;

class E2eFailure extends Error {
  constructor(message) {
    super(message);
    this.name = "E2eFailure";
  }
}

function parseArguments(argv) {
  const options = { binary: null, model: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--model") {
      options.model = true;
      continue;
    }
    if (argument === "--binary") {
      const binary = argv[index + 1];
      if (!binary || binary.startsWith("-")) {
        throw new E2eFailure("--binary 需要可执行文件路径");
      }
      options.binary = resolve(binary);
      index += 1;
      continue;
    }
    if (argument === "--help" || argument === "-h") {
      printUsage();
      process.exit(0);
    }
    throw new E2eFailure(`未知参数: ${argument}`);
  }
  return options;
}

function printUsage() {
  console.log(
    [
      "用法:",
      "  node scripts/headless-lifecycle-e2e.mjs [--binary PATH]",
      "  node scripts/headless-lifecycle-e2e.mjs --model [--binary PATH]",
      "",
      "默认模式验证无凭据的隔离 Host 生命周期；--model 由调用方明确允许当前进程",
      "中的 KEENCODE_PROVIDER_* / OPENAI_* 环境变量，且不会输出或持久化这些值。",
    ].join("\n"),
  );
}

function sanitizedEnvironment() {
  const environment = { ...process.env };
  for (const name of PROVIDER_ENV_NAMES) {
    delete environment[name];
  }
  return environment;
}

function modelEnvironment() {
  const environment = { ...process.env };
  const model = environment.KEENCODE_PROVIDER_MODEL || environment.OPENAI_MODEL;
  const apiKey = environment.KEENCODE_PROVIDER_API_KEY || environment.OPENAI_API_KEY;
  if (!model || !apiKey) {
    throw new E2eFailure(
      "--model 需要当前进程提供 Provider 模型标识和 API Key 环境变量",
    );
  }
  return environment;
}

async function pathExists(path) {
  try {
    await access(path, fsConstants.F_OK);
    return true;
  } catch {
    return false;
  }
}

function wait(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function reservePort() {
  return new Promise((resolvePromise, rejectPromise) => {
    const server = createServer();
    server.once("error", rejectPromise);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : null;
      server.close((error) => {
        if (error) {
          rejectPromise(error);
        } else if (!port) {
          rejectPromise(new Error("无法取得测试 Web 端口"));
        } else {
          resolvePromise(port);
        }
      });
    });
  });
}

function httpRequestOnce(port, pathname, options = {}) {
  const method = options.method ?? "GET";
  const body = options.body ?? "";
  const headers = {
    Host: `127.0.0.1:${port}`,
    Connection: "close",
    ...options.headers,
  };
  if (body && !headers["Content-Length"] && !headers["content-length"]) {
    headers["Content-Length"] = Buffer.byteLength(body);
  }
  return new Promise((resolvePromise, rejectPromise) => {
    const request = httpRequest(
      {
        host: "127.0.0.1",
        port,
        path: pathname,
        method,
        headers,
      },
      (response) => {
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.once("end", () => {
          resolvePromise({
            statusCode: response.statusCode ?? 0,
            headers: response.headers,
            body: Buffer.concat(chunks),
          });
        });
      },
    );
    request.once("error", rejectPromise);
    if (body) {
      request.write(body);
    }
    request.end();
  });
}

function cookieHeader(setCookie) {
  return (setCookie ?? [])
    .map((cookie) => cookie.split(";", 1)[0])
    .join("; ");
}

function waitForSocketClosed(socket, timeoutMs) {
  if (socket.destroyed) {
    return Promise.resolve();
  }
  return new Promise((resolvePromise, rejectPromise) => {
    const timer = setTimeout(() => {
      socket.destroy();
      rejectPromise(new E2eFailure("WebSocket 未在 stop 后关闭"));
    }, timeoutMs);
    socket.once("close", () => {
      clearTimeout(timer);
      resolvePromise();
    });
  });
}

function readWebSocketFrame(buffer) {
  if (buffer.length < 2) {
    return null;
  }
  const firstLength = buffer[1] & 0x7f;
  let offset = 2;
  let length = firstLength;
  if (firstLength === 126) {
    if (buffer.length < offset + 2) return null;
    length = buffer.readUInt16BE(offset);
    offset += 2;
  } else if (firstLength === 127) {
    if (buffer.length < offset + 8) return null;
    const longLength = buffer.readBigUInt64BE(offset);
    if (longLength > BigInt(Number.MAX_SAFE_INTEGER)) return null;
    length = Number(longLength);
    offset += 8;
  }
  const masked = (buffer[1] & 0x80) !== 0;
  if (masked) {
    if (buffer.length < offset + 4) return null;
    offset += 4;
  }
  if (buffer.length < offset + length) {
    return null;
  }
  return {
    opcode: buffer[0] & 0x0f,
    payload: buffer.subarray(offset, offset + length),
    consumed: offset + length,
  };
}

function connectWebSocket(port, cookies) {
  return new Promise((resolvePromise, rejectPromise) => {
    const socket = tcpConnect(port, "127.0.0.1");
    const key = randomBytes(16).toString("base64");
    let buffer = Buffer.alloc(0);
    let handshakeComplete = false;
    let settled = false;
    const messages = [];
    const waiters = [];
    const enqueue = (value) => {
      const waiter = waiters.shift();
      if (waiter) {
        clearTimeout(waiter.timer);
        waiter.resolve(value);
      } else {
        messages.push(value);
      }
    };
    const nextText = (timeoutMs) => {
      if (messages.length > 0) {
        return Promise.resolve(messages.shift());
      }
      return new Promise((resolveText, rejectText) => {
        const timer = setTimeout(() => {
          const index = waiters.findIndex((waiter) => waiter.resolve === resolveText);
          if (index >= 0) waiters.splice(index, 1);
          rejectText(new E2eFailure("WebSocket 响应超时"));
        }, timeoutMs);
        waiters.push({ resolve: resolveText, reject: rejectText, timer });
      });
    };
    const sendText = (value) => {
      const payload = Buffer.from(value, "utf8");
      const mask = randomBytes(4);
      let header;
      if (payload.length < 126) {
        header = Buffer.from([0x81, 0x80 | payload.length]);
      } else if (payload.length <= 0xffff) {
        header = Buffer.alloc(4);
        header[0] = 0x81;
        header[1] = 0x80 | 126;
        header.writeUInt16BE(payload.length, 2);
      } else {
        header = Buffer.alloc(10);
        header[0] = 0x81;
        header[1] = 0x80 | 127;
        header.writeBigUInt64BE(BigInt(payload.length), 2);
      }
      const masked = Buffer.from(payload);
      for (let index = 0; index < masked.length; index += 1) {
        masked[index] ^= mask[index % 4];
      }
      socket.write(Buffer.concat([header, mask, masked]));
    };
    const timer = setTimeout(() => {
      socket.destroy();
      if (!settled) rejectPromise(new E2eFailure("WebSocket 握手超时"));
    }, DEFAULT_COMMAND_TIMEOUT_MS);
    const fail = (error) => {
      clearTimeout(timer);
      if (!settled) {
        settled = true;
        socket.destroy();
        rejectPromise(error);
      }
    };
    socket.once("error", (error) => fail(error));
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (!handshakeComplete) {
        const separator = buffer.indexOf("\r\n\r\n");
        if (separator < 0) return;
        const response = buffer.subarray(0, separator).toString("ascii");
        if (!response.startsWith("HTTP/1.1 101")) {
          fail(new E2eFailure("WebSocket 未完成 101 握手"));
          return;
        }
        buffer = buffer.subarray(separator + 4);
        handshakeComplete = true;
      }
      while (handshakeComplete) {
        const frame = readWebSocketFrame(buffer);
        if (!frame) return;
        buffer = buffer.subarray(frame.consumed);
        if (frame.opcode === 1) {
          let value;
          try {
            value = JSON.parse(frame.payload.toString("utf8"));
          } catch {
            if (!settled) {
              fail(new E2eFailure("WebSocket ready 不是 JSON"));
            }
            return;
          }
          if (value.type === "ready" && !settled) {
            clearTimeout(timer);
            settled = true;
            resolvePromise({ socket, ready: value, nextText, sendText });
          } else if (settled) {
            enqueue(value);
          }
        }
      }
    });
    socket.once("close", () => {
      for (const waiter of waiters.splice(0)) {
        clearTimeout(waiter.timer);
        waiter.reject(new E2eFailure("WebSocket 已关闭"));
      }
    });
    socket.once("connect", () => {
      socket.write(
        [
          "GET /api/ws HTTP/1.1",
          `Host: 127.0.0.1:${port}`,
          "Upgrade: websocket",
          "Connection: Upgrade",
          "Sec-WebSocket-Version: 13",
          `Sec-WebSocket-Key: ${key}`,
          `Origin: http://127.0.0.1:${port}`,
          `Cookie: ${cookies}`,
          "",
          "",
        ].join("\r\n"),
      );
    });
  });
}

async function waitForPortClosed(port, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const open = await new Promise((resolvePromise) => {
      const socket = tcpConnect({ port, host: "127.0.0.1" });
      const done = (value) => {
        socket.destroy();
        resolvePromise(value);
      };
      socket.once("connect", () => done(true));
      socket.once("error", () => done(false));
    });
    if (!open) return;
    await wait(50);
  }
  throw new E2eFailure("Web Host stop 后端口仍在监听");
}

function spawnCaptured(command, args, options = {}) {
  const timeoutMs = options.timeoutMs ?? DEFAULT_COMMAND_TIMEOUT_MS;
  const environment = options.environment ?? sanitizedEnvironment();
  const label = options.label ?? command;

  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd: REPO_ROOT,
      env: environment,
      stdio: ["ignore", "pipe", "ignore"],
      windowsHide: true,
    });
    let stdout = "";
    let outputBytes = 0;
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill();
    }, timeoutMs);
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      outputBytes += Buffer.byteLength(chunk);
      if (outputBytes <= OUTPUT_LIMIT_BYTES) {
        stdout += chunk;
      }
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      rejectPromise(new E2eFailure(`${label} 启动失败: ${error.code || "process error"}`));
    });
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      if (timedOut) {
        rejectPromise(new E2eFailure(`${label} 超时`));
        return;
      }
      resolvePromise({ code, signal, stdout });
    });
  });
}

function parseJsonLines(output) {
  const records = [];
  for (const line of output.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    try {
      records.push(JSON.parse(trimmed));
    } catch {
      throw new E2eFailure("CLI 输出不是有效 NDJSON");
    }
  }
  return records;
}

function expect(condition, message) {
  if (!condition) {
    throw new E2eFailure(message);
  }
}

function recordOfType(records, type) {
  return records.find((record) => record && record.type === type);
}

function assertCliSuccess(result, label) {
  expect(result.code === 0, `${label} 失败，退出码 ${result.code ?? "unknown"}`);
  return parseJsonLines(result.stdout);
}

async function resolveCliBinary(explicitBinary) {
  if (explicitBinary) {
    expect(await pathExists(explicitBinary), "--binary 指向的文件不存在");
    return explicitBinary;
  }

  const configured = process.env.KEENCODE_CLI_BIN;
  if (configured) {
    const binary = resolve(configured);
    expect(await pathExists(binary), "KEENCODE_CLI_BIN 指向的文件不存在");
    return binary;
  }

  const binaryName = process.platform === "win32" ? "keencode.exe" : "keencode";
  const targetBinary = join(REPO_ROOT, "target", "debug", binaryName);
  if (!(await pathExists(targetBinary))) {
    const build = await spawnCaptured(
      process.platform === "win32" ? "cargo.exe" : "cargo",
      ["build", "-p", "keencode-cli", "--bin", "keencode"],
      { label: "cargo build", timeoutMs: 120_000, environment: sanitizedEnvironment() },
    );
    expect(build.code === 0, `cargo build 失败，退出码 ${build.code ?? "unknown"}`);
  }
  expect(await pathExists(targetBinary), "cargo build 后未找到 keencode 可执行文件");
  return targetBinary;
}

async function waitForDiscovery(dataRoot, timeoutMs) {
  const discoveryPath = join(dataRoot, DISCOVERY_FILE);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await pathExists(discoveryPath)) {
      try {
        const record = JSON.parse(await readFile(discoveryPath, "utf8"));
        if (
          record.ownerKind === "headless" &&
          typeof record.endpoint === "string" &&
          record.endpoint.length > 0 &&
          typeof record.hostId === "string" &&
          record.hostId.length > 0 &&
          typeof record.dataRootFingerprint === "string" &&
          record.dataRootFingerprint.length > 0
        ) {
          return record;
        }
      } catch {
        // Host publishes atomically; retry while the file is being replaced.
      }
    }
    await wait(50);
  }
  throw new E2eFailure("未在限定时间内发现 headless Host");
}

async function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null) {
    return child.exitCode;
  }
  return new Promise((resolvePromise, rejectPromise) => {
    const timer = setTimeout(() => {
      rejectPromise(new E2eFailure("headless Host 未在 idle 窗口内退出"));
    }, timeoutMs);
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolvePromise(code);
    });
  });
}

function spawnHost(binary, dataRoot, environment) {
  const child = spawn(binary, ["--data-root", dataRoot, "headless", "--json"], {
    cwd: REPO_ROOT,
    env: environment,
    stdio: ["ignore", "ignore", "ignore"],
    windowsHide: true,
  });
  return child;
}

async function runLifecycle(binary, environment) {
  const dataRoot = await mkdtemp(join(tmpdir(), "keencode-headless-e2e-"));
  const projectRoot = await mkdtemp(join(tmpdir(), "keencode-project-e2e-"));
  const staticRoot = await mkdtemp(join(tmpdir(), "keencode-web-static-e2e-"));
  await writeFile(staticRoot + "/index.html", "<!doctype html><title>KeenCode E2E</title>");
  const webPort = await reservePort();
  const webToken = "headless-web-e2e-token-20260921";
  const webEnvironment = {
    ...environment,
    KEENCODE_WEB_STATIC_ROOT: staticRoot,
    KEENCODE_WEB_TOKEN: webToken,
  };
  const host = spawnHost(binary, dataRoot, webEnvironment);
  let webSocket = null;
  let hostExitCode = null;
  try {
    await waitForDiscovery(dataRoot, HOST_START_TIMEOUT_MS);
    console.log("PASS host discovery");

    const listArgs = ["--data-root", dataRoot, "session", "list", "--json"];
    const listed = assertCliSuccess(
      await spawnCaptured(binary, listArgs, { environment, label: "session list" }),
      "session list",
    );
    expect(recordOfType(listed, "session_list"), "session list 未返回 session_list 记录");
    console.log("PASS initialize and session list");

    const started = assertCliSuccess(
      await spawnCaptured(
        binary,
        ["--data-root", dataRoot, "web", "start", "--port", String(webPort), "--json"],
        { environment: webEnvironment, label: "web start" },
      ),
      "web start",
    );
    const startedResult = recordOfType(started, "web")?.result;
    expect(startedResult?.state === "running", "web start 未返回 running 状态");
    expect(startedResult?.port === webPort, "web start 未使用请求的固定端口");
    console.log("PASS web start fixed port and custom token");

    const staticResponse = await httpRequestOnce(webPort, "/?hostMode=mobile-remote");
    expect(staticResponse.statusCode === 200, "Web 生产静态资源请求失败");
    expect(staticResponse.body.toString("utf8").includes("KeenCode E2E"), "静态 index.html 内容不匹配");

    const loginResponse = await httpRequestOnce(webPort, "/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token: webToken }),
    });
    expect(loginResponse.statusCode === 200, "Web 自定义 Token 登录失败");
    const cookies = cookieHeader(loginResponse.headers["set-cookie"]);
    expect(cookies.includes("keencode_session=") && cookies.includes("keencode_csrf="), "登录未返回认证 Cookie");

    const sessionResponse = await httpRequestOnce(webPort, "/api/auth/session", {
      headers: { Cookie: cookies },
    });
    expect(sessionResponse.statusCode === 200, "Web 认证 Session 查询失败");
    const capabilitiesResponse = await httpRequestOnce(webPort, "/api/capabilities", {
      headers: { Cookie: cookies },
    });
    expect(capabilitiesResponse.statusCode === 200, "Web capability 查询失败");

    webSocket = await connectWebSocket(webPort, cookies);
    expect(webSocket.ready.transport === "websocket", "WebSocket ready transport 不匹配");
    webSocket.sendText(
      JSON.stringify({
        jsonrpc: "2.0",
        id: "web-initialize",
        method: "initialize",
        params: { protocolVersion: 1, clientCapabilities: {} },
      }),
    );
    const initializeResponse = await webSocket.nextText(DEFAULT_COMMAND_TIMEOUT_MS);
    expect(
      initializeResponse.id === "web-initialize" && initializeResponse.result?.protocolVersion === 1,
      `WebSocket ACP initialize 响应不符合协议: ${JSON.stringify(initializeResponse)}`,
    );
    console.log("PASS web static/auth/capabilities/WebSocket ACP initialize");

    const duplicateStart = assertCliSuccess(
      await spawnCaptured(
        binary,
        ["--data-root", dataRoot, "web", "start", "--port", String(webPort), "--json"],
        { environment: webEnvironment, label: "web duplicate start" },
      ),
      "web duplicate start",
    );
    expect(recordOfType(duplicateStart, "web")?.result?.state === "running", "重复 web start 不幂等");
    const conflictingPort = await reservePort();
    const conflict = await spawnCaptured(
      binary,
      ["--data-root", dataRoot, "web", "start", "--port", String(conflictingPort), "--json"],
      { environment: webEnvironment, label: "web conflicting start" },
    );
    expect(conflict.code !== 0, "不同固定端口的重复 web start 未失败");
    console.log("PASS web status/idempotency/port conflict");

    const stoppedWeb = assertCliSuccess(
      await spawnCaptured(binary, ["--data-root", dataRoot, "web", "stop", "--json"], {
        environment: webEnvironment,
        label: "web stop",
      }),
      "web stop",
    );
    expect(recordOfType(stoppedWeb, "web")?.result?.state === "stopped", "web stop 未返回 stopped 状态");
    await waitForSocketClosed(webSocket.socket, DEFAULT_COMMAND_TIMEOUT_MS);
    webSocket = null;
    await waitForPortClosed(webPort, DEFAULT_COMMAND_TIMEOUT_MS);
    const stoppedStatus = assertCliSuccess(
      await spawnCaptured(binary, ["--data-root", dataRoot, "web", "status", "--json"], {
        environment: webEnvironment,
        label: "web status after stop",
      }),
      "web status after stop",
    );
    expect(recordOfType(stoppedStatus, "web")?.result?.state === "stopped", "web stop 后 status 不一致");
    console.log("PASS web stop closes WebSocket/listener");

    const detached = assertCliSuccess(
      await spawnCaptured(
        binary,
        [
          "--data-root",
          dataRoot,
          "run",
          "--cwd",
          projectRoot,
          "--detach",
          "--json",
          "隔离生命周期测试",
        ],
        { environment, label: "run --detach" },
      ),
      "run --detach",
    );
    const created = recordOfType(detached, "session_created");
    const admitted = recordOfType(detached, "detached");
    expect(created?.sessionId, "run --detach 未返回 session_created");
    expect(admitted?.sessionId === created.sessionId, "detached Session 标识不一致");
    expect(admitted?.turnId && admitted?.taskId, "detached 未返回 turn/task 身份");
    console.log("PASS session new and detached admission");

    const sessionId = created.sessionId;
    const attached = assertCliSuccess(
      await spawnCaptured(
        binary,
        ["--data-root", dataRoot, "session", "attach", sessionId, "--json"],
        { environment, label: "session attach" },
      ),
      "session attach",
    );
    expect(recordOfType(attached, "session_loaded"), "session attach 未返回 session_loaded");
    console.log("PASS session attach and journal replay");

    const stopped = assertCliSuccess(
      await spawnCaptured(
        binary,
        [
          "--data-root",
          dataRoot,
          "session",
          "stop",
          sessionId,
          "--turn",
          admitted.turnId,
          "--json",
        ],
        { environment, label: "session stop" },
      ),
      "session stop",
    );
    expect(recordOfType(stopped, "cancel_requested"), "session stop 未返回 cancel_requested");
    console.log("PASS session stop");

    hostExitCode = await waitForExit(host, HOST_IDLE_TIMEOUT_MS);
    expect(hostExitCode === 0, `headless Host idle 退出码异常: ${hostExitCode}`);
    expect(!(await pathExists(join(dataRoot, DISCOVERY_FILE))), "Host 退出后 discovery 未清理");
    console.log("PASS 30-second idle shutdown");
  } finally {
    if (webSocket) {
      webSocket.socket.destroy();
    }
    if (host.exitCode === null) {
      host.kill();
      await new Promise((resolvePromise) => host.once("exit", resolvePromise));
    }
    await Promise.all([
      rm(dataRoot, { recursive: true, force: true }),
      rm(projectRoot, { recursive: true, force: true }),
      rm(staticRoot, { recursive: true, force: true }),
    ]);
  }
}

async function runModelSmoke(binary, environment) {
  const dataRoot = await mkdtemp(join(tmpdir(), "keencode-model-e2e-"));
  const projectRoot = await mkdtemp(join(tmpdir(), "keencode-model-project-"));
  const host = spawnHost(binary, dataRoot, environment);
  try {
    await waitForDiscovery(dataRoot, HOST_START_TIMEOUT_MS);
    const result = await spawnCaptured(
      binary,
      [
        "--data-root",
        dataRoot,
        "run",
        "--cwd",
        projectRoot,
        "--json",
        "只回复 E2E_OK，不调用工具。",
      ],
      { environment, timeoutMs: 120_000, label: "model run" },
    );
    const records = assertCliSuccess(result, "model run");
    const completed = recordOfType(records, "completed");
    expect(completed?.stopReason === "end_turn", "真实模型未返回 end_turn");
    console.log("PASS isolated real-model smoke");
  } finally {
    if (host.exitCode === null) {
      host.kill();
      await new Promise((resolvePromise) => host.once("exit", resolvePromise));
    }
    await Promise.all([
      rm(dataRoot, { recursive: true, force: true }),
      rm(projectRoot, { recursive: true, force: true }),
    ]);
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const binary = await resolveCliBinary(options.binary);
  const environment = options.model ? modelEnvironment() : sanitizedEnvironment();
  if (options.model) {
    await runModelSmoke(binary, environment);
  } else {
    await runLifecycle(binary, environment);
  }
  console.log("E2E PASS");
}

main().catch((error) => {
  if (error instanceof E2eFailure) {
    console.error(`E2E FAIL: ${error.message}`);
  } else {
    console.error("E2E FAIL: 未知运行错误");
  }
  process.exitCode = 1;
});
