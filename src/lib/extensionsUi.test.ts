import { describe, expect, it } from "vitest";
import {
  filterPluginsByLoadState,
  mcpRuntimePhaseTone,
  mcpRuntimeStatusTone,
  mergeMcpServers,
  mergeInspectErrors,
  normalizePluginInstallSource,
  parseMcpOAuthCallbackInput,
  pluginProvidesLine,
  pluginRowKey,
  pluginStatusTone,
  projectMcpOAuthUiAction,
  shortPathLabel,
  skillMetaLine,
  skillSourceTone,
  sortMcpByName,
  sortPluginsByName,
  sortSkillsByName,
} from "./extensionsUi";

describe("MCP runtime helpers", () => {
  it("合并静态配置与运行态，并保留只存在于一侧的 Server", () => {
    const configured = [
      {
        name: "zeta",
        transport: "stdio" as const,
        target: "zeta-command",
        enabled: false,
      },
      {
        name: "Alpha",
        transport: "stdio" as const,
        target: "alpha-command",
        enabled: true,
      },
    ];
    const rows = mergeMcpServers(configured, {
      initPhase: "ready",
      servers: [
        {
          name: "Alpha",
          status: "connected",
          transport: "http",
          toolsCount: 3,
          oauthStatus: "authorized",
          error: null,
        },
        {
          name: "orphan",
          status: "failed",
          transport: "http",
          toolsCount: 0,
          oauthStatus: "needs_authorization",
          error: "401 Unauthorized",
        },
      ],
    });

    expect(rows.map((row) => row.name)).toEqual(["Alpha", "orphan", "zeta"]);
    expect(rows[0]).toMatchObject({
      config: configured[1],
      transport: "http",
      runtimeStatus: "connected",
      toolsCount: 3,
      oauthStatus: "authorized",
    });
    expect(rows[1]).toMatchObject({
      config: null,
      enabled: true,
      runtimeStatus: "failed",
      error: "401 Unauthorized",
    });
    expect(rows[2]).toMatchObject({
      config: configured[0],
      enabled: false,
      runtimeStatus: "disabled",
      toolsCount: 0,
    });
  });

  it("映射连接状态与初始化阶段的界面色调", () => {
    expect(mcpRuntimeStatusTone("connected")).toBe("ok");
    expect(mcpRuntimeStatusTone("failed")).toBe("fail");
    expect(mcpRuntimeStatusTone("disconnected")).toBe("muted");
    expect(mcpRuntimePhaseTone("ready")).toBe("ok");
    expect(mcpRuntimePhaseTone("failed")).toBe("fail");
    expect(mcpRuntimePhaseTone("initializing")).toBe("muted");
  });

  it("将 Host 级 OAuth 事件投影为浏览器打开或状态刷新", () => {
    expect(
      projectMcpOAuthUiAction({
        type: "oauth_needed",
        value: {
          server_name: "remote",
          auth_url: "https://example.com/authorize",
        },
      }),
    ).toEqual({
      type: "open_authorization",
      serverName: "remote",
      authorizationUrl: "https://example.com/authorize",
    });
    expect(
      projectMcpOAuthUiAction({
        type: "oauth_failed",
        value: { server_name: "remote", error: "denied" },
      }),
    ).toEqual({ type: "refresh", serverName: "remote", error: "denied" });
    expect(
      projectMcpOAuthUiAction({
        type: "oauth_completed",
        value: { server_name: "remote" },
      }),
    ).toEqual({ type: "refresh", serverName: "remote", error: null });
    expect(projectMcpOAuthUiAction({ type: "compact_started" })).toBeNull();
  });

  it("解析完整回调 URL、查询串与单独授权码", () => {
    const authorizationUrl =
      "https://login.example.com/authorize?client_id=demo&state=expected-state";
    expect(
      parseMcpOAuthCallbackInput(
        "http://127.0.0.1:3456/callback?code=url-code&state=expected-state",
        authorizationUrl,
      ),
    ).toEqual({ ok: true, code: "url-code", state: "expected-state" });
    expect(
      parseMcpOAuthCallbackInput(
        "code=query-code&state=expected-state",
        authorizationUrl,
      ),
    ).toEqual({ ok: true, code: "query-code", state: "expected-state" });
    expect(parseMcpOAuthCallbackInput("raw-code", authorizationUrl)).toEqual({
      ok: true,
      code: "raw-code",
      state: "expected-state",
    });
  });

  it("拒绝缺少参数或 state 不匹配的 OAuth 回调", () => {
    const authorizationUrl =
      "https://login.example.com/authorize?state=expected-state";
    expect(parseMcpOAuthCallbackInput("", authorizationUrl)).toEqual({
      ok: false,
      error: "empty",
    });
    expect(
      parseMcpOAuthCallbackInput("?state=expected-state", authorizationUrl),
    ).toEqual({ ok: false, error: "missing_code" });
    expect(parseMcpOAuthCallbackInput("raw-code", null)).toEqual({
      ok: false,
      error: "missing_state",
    });
    expect(
      parseMcpOAuthCallbackInput(
        "?code=demo&state=unexpected-state",
        authorizationUrl,
      ),
    ).toEqual({ ok: false, error: "state_mismatch" });
  });
});

describe("skillSourceTone", () => {
  it("只映射当前合约的三种 Skill 来源", () => {
    expect(skillSourceTone("user")).toBe("user");
    expect(skillSourceTone("project")).toBe("project");
    expect(skillSourceTone("plugin")).toBe("plugin");
  });
});

describe("skillMetaLine", () => {
  it("builds skill meta", () => {
    expect(
      skillMetaLine({
        source: "user",
        userInvocable: true,
      }),
    ).toBe("user · user-invocable");
    expect(
      skillMetaLine({ source: "project", userInvocable: false }),
    ).toBe("project");
  });
});

describe("sort helpers", () => {
  it("sorts skills and mcp by name case-insensitively", () => {
    expect(sortSkillsByName([{ name: "zeta" }, { name: "Alpha" }]).map((s) => s.name)).toEqual([
      "Alpha",
      "zeta",
    ]);
    expect(sortMcpByName([{ name: "b" }, { name: "a" }]).map((s) => s.name)).toEqual([
      "a",
      "b",
    ]);
  });
});

describe("shortPathLabel", () => {
  it("returns short paths unchanged", () => {
    expect(shortPathLabel("/tmp/a")).toBe("/tmp/a");
  });

  it("truncates long paths keeping basename tail", () => {
    const long =
      "/Users/someone/Library/Application Support/com.keencode.desktop/skills/my-skill/SKILL.md";
    const label = shortPathLabel(long, 40);
    expect(label.startsWith("…")).toBe(true);
    expect(label.length).toBeLessThanOrEqual(40);
    expect(label.includes("SKILL.md") || label.includes("my-skill")).toBe(true);
  });

  it("handles empty", () => {
    expect(shortPathLabel("")).toBe("");
    expect(shortPathLabel(null)).toBe("");
  });
});

describe("mergeInspectErrors", () => {
  it("returns null when both empty", () => {
    expect(mergeInspectErrors(null, null, null)).toBeNull();
    expect(mergeInspectErrors("", "", null)).toBeNull();
  });

  it("dedupes identical messages", () => {
    expect(mergeInspectErrors("same", "same", null)).toBe("same");
  });

  it("joins distinct non-cli errors", () => {
    expect(mergeInspectErrors("a", "b", null)).toBe("a · b");
  });

  it("includes the plugins error", () => {
    expect(mergeInspectErrors("a", "b", "c")).toBe("a · b · c");
  });
});

describe("plugin helpers", () => {
  it("sorts plugins by name", () => {
    expect(
      sortPluginsByName([{ name: "zeta" }, { name: "Alpha" }]).map((p) => p.name),
    ).toEqual(["Alpha", "zeta"]);
  });

  it("maps the current load state", () => {
    expect(pluginStatusTone(false)).toBe("disabled");
    expect(pluginStatusTone(true)).toBe("enabled");
  });

  it("生成插件 Skill 数量和唯一行键", () => {
    expect(
      pluginProvidesLine({
        provides: { skills: 14 },
      }),
    ).toBe("14 skills");
    expect(
      pluginProvidesLine({
        provides: { skills: 0 },
      }),
    ).toBe("");
    expect(() => pluginProvidesLine({ provides: null })).toThrow(
      "插件 provides 缺失",
    );
    expect(
      pluginRowKey({
        name: "cloudflare",
      }),
    ).toBe("cloudflare");
    expect(pluginRowKey({ name: "solo" })).toBe("solo");
  });

  it("filters by load state", () => {
    const rows = [
      { name: "a", enabled: true },
      { name: "b", enabled: false },
      { name: "c", enabled: true },
    ];
    expect(filterPluginsByLoadState(rows, "all").map((p) => p.name)).toEqual([
      "a",
      "b",
      "c",
    ]);
    expect(filterPluginsByLoadState(rows, "enabled").map((p) => p.name)).toEqual([
      "a",
      "c",
    ]);
    expect(filterPluginsByLoadState(rows, "disabled").map((p) => p.name)).toEqual([
      "b",
    ]);
  });

  it("normalizes a local path or marketplace selector", () => {
    expect(normalizePluginInstallSource("  demo@local-tools  ")).toBe(
      "demo@local-tools",
    );
    expect(normalizePluginInstallSource("/tmp/plugin")).toBe("/tmp/plugin");
    expect(normalizePluginInstallSource("")).toBeNull();
    expect(normalizePluginInstallSource("   ")).toBeNull();
  });
});
