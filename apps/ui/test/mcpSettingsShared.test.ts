import assert from "node:assert/strict";
import { test } from "vitest";
import type { ZCodeMcpServer } from "@zcode/shared";
import {
  formToConfig,
  jsonDraftToForm,
  serverToForm,
  type FormState,
} from "../../../packages/ui/src/settings/mcpSettingsShared.js";

function form(patch: Partial<FormState>): FormState {
  return {
    name: "native-local-mcp",
    scope: "zcodeagentmcp",
    storageLevel: "user",
    type: "stdio",
    command: "powershell.exe",
    args: "-NoProfile",
    env: "",
    url: "",
    headers: "",
    timeoutMs: "",
    oauth: "",
    protocolVersion: "",
    ...patch,
  };
}

test("表单保存 stdio 配置时只输出 Rust canonical 字段", () => {
  assert.deepEqual(
    formToConfig(
      form({
        type: "stdio",
        args: '-NoProfile -File "C:\\isolated project\\mcp-fixture.ps1"',
      }),
    ),
    {
      command: "powershell.exe",
      args: ["-NoProfile", "-File", "C:\\isolated project\\mcp-fixture.ps1"],
      env: undefined,
    },
  );
});

test("无参数时保存空数组而不是空 token", () => {
  assert.deepEqual(formToConfig(form({ type: "stdio", args: "  " })), {
    command: "powershell.exe",
    args: [],
    env: undefined,
  });
});

test("MCP args 在 Rust 配置与表单之间保持空值、空格和嵌入引号", () => {
  const args = [
    "-NoProfile",
    "",
    "C:\\isolated project\\mcp-fixture.ps1",
    '{"path":"C:\\isolated project\\evidence.txt"}',
  ];
  const server: ZCodeMcpServer = {
    id: "native-local-mcp",
    name: "native-local-mcp",
    config: { command: "powershell.exe", args },
    enabled: true,
    source: "zcodeagentmcp",
    scope: "user",
  };

  const edited = serverToForm(server);
  assert.deepEqual(formToConfig(edited).args, args);
  const jsonEdited = jsonDraftToForm(
    JSON.stringify({ mcpServers: { "native-local-mcp": server.config } }),
    form({ args: "" }),
  );
  assert.deepEqual(formToConfig(jsonEdited).args, args);
});

test("表单解析显式空参数与 JSON 转义引号", () => {
  assert.deepEqual(
    formToConfig(
      form({ args: 'first "" "C:\\path with space" "{\\"key\\":\\"value\\"}"' }),
    ).args,
    ["first", "", "C:\\path with space", '{"key":"value"}'],
  );
});

test("表单保存 HTTP 配置时不把 UI 传输提示写入磁盘", () => {
  assert.deepEqual(
    formToConfig(
      form({
        type: "http",
        command: "",
        args: "",
        url: "http://127.0.0.1:8765/mcp",
      }),
    ),
    {
      url: "http://127.0.0.1:8765/mcp",
      headers: undefined,
    },
  );
});
