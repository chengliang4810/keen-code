import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  McpRuntimeDetails,
  normalizeConfigValue,
} from "./ExtensionsPanel";
import type { McpServerView } from "@/lib/extensionsUi";
import type { PluginUserConfigFieldDto } from "@/lib/api";

describe("McpRuntimeDetails", () => {
  it("展示连接状态、实际传输、工具数与 OAuth 状态", () => {
    const server = {
      name: "remote-docs",
      config: {
        name: "remote-docs",
        transport: "http",
        target: "https://example.com/mcp",
        enabled: true,
      },
      enabled: true,
      target: "https://example.com/mcp",
      transport: "streamable-http",
      runtimeStatus: "connected",
      toolsCount: 4,
      oauthStatus: "authorized",
      error: null,
    } satisfies McpServerView;

    const html = renderToStaticMarkup(
      <McpRuntimeDetails locale="en" server={server} />,
    );

    expect(html).toContain("data-mcp-status=\"connected\"");
    expect(html).toContain("Connected");
    expect(html).toContain("Transport: streamable-http");
    expect(html).toContain("4 tools");
    expect(html).toContain("OAuth authorized");
    expect(html).toContain("ext-badge--ok");
  });

  it("展示授权需求与逐 Server 错误", () => {
    const server = {
      name: "remote-search",
      config: null,
      enabled: true,
      target: null,
      transport: "http",
      runtimeStatus: "failed",
      toolsCount: 0,
      oauthStatus: "needs_authorization",
      error: "401 Unauthorized",
    } satisfies McpServerView;

    const html = renderToStaticMarkup(
      <McpRuntimeDetails
        locale="zh"
        server={server}
        error="浏览器打开失败"
      />,
    );

    expect(html).toContain("连接失败");
    expect(html).toContain("需要 OAuth 授权");
    expect(html).toContain("浏览器打开失败");
    expect(html).toContain("role=\"alert\"");
    expect(html).toContain("ext-badge--fail");
  });
});

describe("Plugin userConfig controls", () => {
  const selectField = {
    name: "mode",
    valueType: "select",
    title: "Mode",
    description: "How the plugin runs.",
    required: false,
    sensitive: false,
    multiple: false,
    default: "auto",
    value: "auto",
    enumValues: ["auto", "manual"],
  } satisfies PluginUserConfigFieldDto;

  it("只接受 enum 中的值，并过滤多选中的外部值", () => {
    expect(normalizeConfigValue(selectField, "manual")).toBe("manual");
    expect(normalizeConfigValue(selectField, "external")).toBeUndefined();
    expect(
      normalizeConfigValue(
        { ...selectField, multiple: true },
        ["manual", "external"],
      ),
    ).toEqual(["manual"]);
  });

  it("不再渲染原生 select，并同时使用统一单选与多选控件", () => {
    const source = readFileSync(new URL("./ExtensionsPanel.tsx", import.meta.url), "utf8");
    expect(source).not.toMatch(/<select\b/);
    expect(source).toContain("<SelectTrigger");
    expect(source).toContain("<MultiSelect");
    expect(source).toContain("field.multiple && field.valueType === \"select\"");
  });
});

describe("Plugin marketplace boundaries", () => {
  it("将插件安装与已安装插件管理放在独立页面", () => {
    const source = readFileSync(
      new URL("./ExtensionsPanel.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain('{tab === "market" && (');
    expect(source).toContain('{tab === "plugins" && (');
    expect(source).toContain("<ExtensionsBuildExtras");
    expect(source).toContain("已安装插件只在插件管理页展示");
  });
});
