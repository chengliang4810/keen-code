import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  McpRuntimeDetails,
} from "./ExtensionsPanel";
import type { McpServerView } from "@/lib/extensionsUi";

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
