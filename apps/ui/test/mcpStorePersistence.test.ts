import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "vitest";
import type {
  McpServerConfig,
  NativeMcpServerRecord,
  SaveCliMcpToUserDirectoryRequest,
} from "@zcode/shared";
import {
  setMcpStoreDirectoryService,
  setMcpStorePlatform,
  useMcpStore,
} from "../../../packages/ui/src/store/mcpStore.js";
import type { McpDirectoryService } from "../../../packages/ui/src/store/mcpStoreDesktop.js";

const workspacePath = "C:/isolated/native-mcp-project";

interface MemoryStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function createStorage(): MemoryStorage {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
}

function createDirectoryService(options?: { failWrites?: () => boolean }): {
  service: McpDirectoryService;
  records: NativeMcpServerRecord[];
  writes: SaveCliMcpToUserDirectoryRequest[];
} {
  const records: NativeMcpServerRecord[] = [];
  const writes: SaveCliMcpToUserDirectoryRequest[] = [];
  const service: McpDirectoryService = {
    loadMcpFromUserDirectory: async (request) => {
      const projectPath = request?.workspacePath;
      return {
        servers: records
          .filter((record) =>
            projectPath ? record.projectPath === projectPath : !record.projectPath,
          )
          .map((record) => ({ ...record, config: { ...record.config } })),
      };
    },
    saveMcpToUserDirectory: async (payload) => {
      writes.push(payload);
      if (options?.failWrites?.()) {
        throw new Error("fixture MCP persistence failure");
      }
      const index = records.findIndex(
        (record) =>
          record.source === payload.source &&
          record.name === payload.name &&
          record.projectPath === payload.projectPath,
      );
      if (payload.action === "delete") {
        if (index >= 0) records.splice(index, 1);
        return;
      }
      if (payload.action === "set-enabled") {
        if (index >= 0) records[index]!.enabled = payload.enabled;
        return;
      }
      const next: NativeMcpServerRecord = {
        source: payload.source,
        scope: payload.projectPath ? "workspace" : "user",
        name: payload.name,
        config: payload.config ?? {},
        enabled: true,
        ...(payload.projectPath ? { projectPath: payload.projectPath } : {}),
      };
      if (index >= 0) records[index] = next;
      else records.push(next);
    },
  };
  return { service, records, writes };
}

const originalWindow = (globalThis as { window?: unknown }).window;
const originalStorage = (globalThis as { localStorage?: unknown }).localStorage;

beforeEach(() => {
  const storage = createStorage();
  (globalThis as { window?: unknown }).window = { localStorage: storage };
  (globalThis as { localStorage?: unknown }).localStorage = storage;
  setMcpStorePlatform(null);
  setMcpStoreDirectoryService(null);
  useMcpStore.setState({
    config: {
      mcp: { mcpServers: {} },
      zcodeagentmcp: { mcpServers: {}, projects: {} },
    },
    nativeServers: [],
    servers: [],
    statusSnapshots: {},
    currentProjectPath: "",
    currentWorkspaceIdentity: undefined,
    enabledStates: {},
    deletedPreloadMcpServers: new Set(),
    isConfigLoaded: false,
    currentSessionId: null,
  });
  useMcpStore.getState().loadConfig();
});

afterEach(() => {
  (globalThis as { window?: unknown }).window = originalWindow;
  (globalThis as { localStorage?: unknown }).localStorage = originalStorage;
  setMcpStorePlatform(null);
  setMcpStoreDirectoryService(null);
});

test("本地 workspace 通过 Rust mcp-sync 持久化并重载 user/workspace scope", async () => {
  const fixture = createDirectoryService();
  setMcpStoreDirectoryService(fixture.service);

  assert.equal(
    await useMcpStore
      .getState()
      .ensureLoadedForWorkspace(workspacePath, fixture.service, "local:C:/isolated/native-mcp-project"),
    true,
  );
  await useMcpStore
    .getState()
    .addScopedMcpServer("zcodeagentmcp", "native-user", { command: "user-mcp" });
  await useMcpStore
    .getState()
    .addScopedMcpServer(
      "zcodeagentmcp",
      "native-workspace",
      { command: "workspace-mcp" },
      workspacePath,
    );

  assert.deepEqual(
    fixture.writes.map(({ name, projectPath }) => ({ name, projectPath })),
    [
      { name: "native-user", projectPath: undefined },
      { name: "native-workspace", projectPath: workspacePath },
    ],
  );

  useMcpStore.setState({ nativeServers: [], servers: [] });
  assert.equal(await useMcpStore.getState().loadMcpFromUserDirectory(fixture.service), true);
  assert.deepEqual(
    useMcpStore
      .getState()
      .nativeServers.map(({ name, scope, projectPath }) => ({ name, scope, projectPath })),
    [
      { name: "native-user", scope: "user", projectPath: undefined },
      { name: "native-workspace", scope: "workspace", projectPath: workspacePath },
    ],
  );
});

test("远程 workspace identity 不会合并本机 user scope", async () => {
  const fixture = createDirectoryService();
  fixture.records.push(
    {
      source: "zcodeagentmcp",
      scope: "user",
      name: "native-user",
      config: { command: "user-mcp" },
      enabled: true,
    },
    {
      source: "zcodeagentmcp",
      scope: "workspace",
      name: "native-workspace",
      config: { command: "workspace-mcp" },
      enabled: true,
      projectPath: workspacePath,
    },
  );
  setMcpStoreDirectoryService(fixture.service);

  assert.equal(
    await useMcpStore
      .getState()
      .ensureLoadedForWorkspace(
        workspacePath,
        fixture.service,
        "remote:ssh:example.test:22:builder:/workspace",
      ),
    true,
  );
  assert.deepEqual(
    useMcpStore.getState().nativeServers.map(({ name, scope }) => ({ name, scope })),
    [{ name: "native-workspace", scope: "workspace" }],
  );
});

test("本地 MCP 持久化失败时不会更新 store 内存状态", async () => {
  let failWrites = true;
  const fixture = createDirectoryService({ failWrites: () => failWrites });
  setMcpStoreDirectoryService(fixture.service);
  await useMcpStore.getState().ensureLoadedForWorkspace(workspacePath, fixture.service);

  await assert.rejects(
    useMcpStore
      .getState()
      .addScopedMcpServer("zcodeagentmcp", "write-fails", { command: "fixture" }),
    /fixture MCP persistence failure/,
  );
  assert.equal(useMcpStore.getState().servers.length, 0);

  failWrites = false;
  await useMcpStore
    .getState()
    .addScopedMcpServer("zcodeagentmcp", "write-fails", { command: "fixture" });
  const server = useMcpStore.getState().servers[0];
  assert.ok(server);

  failWrites = true;
  await assert.rejects(
    useMcpStore.getState().toggleServer(server.id, false),
    /fixture MCP persistence failure/,
  );
  assert.equal(useMcpStore.getState().getServer(server.id)?.enabled, true);
});

test("缺少 MCP 持久化服务时显式失败而不创建本地条目", async () => {
  await useMcpStore.getState().ensureLoadedForWorkspace(workspacePath);
  await assert.rejects(
    useMcpStore
      .getState()
      .addScopedMcpServer("zcodeagentmcp", "no-service", { command: "fixture" }),
    /MCP 持久化服务不可用/,
  );
  assert.equal(useMcpStore.getState().servers.length, 0);
});
