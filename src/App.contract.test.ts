import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/components/ResourceViewer", () => ({
  ResourceViewer: () => null,
}));

import {
  parseElicitationPayload,
  toElicitationAnswers,
} from "./App";

/** 构造当前 ACP 顶层 form elicitation 测试载荷。 */
function currentElicitation() {
  return {
    method: "elicitation/create",
    rpcId: 41,
    params: {
      mode: "form",
      sessionId: "session-1",
      message: "请补充信息",
      requestedSchema: {
        type: "object",
        properties: {
          target: {
            type: "string",
            description: "部署到哪里？",
            oneOf: [{ const: "server", title: "服务器" }],
          },
          checks: {
            type: "array",
            description: "选择检查项",
            items: {
              anyOf: [
                { const: "typecheck", title: "类型检查" },
                { const: "test", title: "测试" },
              ],
            },
          },
        },
      },
    },
  };
}

describe("App ACP elicitation 契约", () => {
  it("解析当前顶层 form schema 并保留字段标识", () => {
    expect(parseElicitationPayload(currentElicitation())).toEqual({
      rpcId: 41,
      sessionId: "session-1",
      questions: [
        {
          id: "target",
          question: "部署到哪里？",
          options: [{ id: "server", label: "服务器" }],
        },
        {
          id: "checks",
          question: "选择检查项",
          options: [
            { id: "typecheck", label: "类型检查" },
            { id: "test", label: "测试" },
          ],
          multiSelect: true,
        },
      ],
    });
  });

  it("按字段标识回送选项值并把多选转换成数组", () => {
    const payload = parseElicitationPayload(currentElicitation());
    expect(payload).not.toBeNull();
    expect(
      toElicitationAnswers(payload!, {
        target: "服务器",
        checks: "类型检查, 测试",
      }),
    ).toEqual({ target: "server", checks: ["typecheck", "test"] });
  });
});

describe("App 启动工作台契约", () => {
  it("先开放工作台，再异步恢复列表，不使用会话状态充当启动门禁", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const refreshStart = source.indexOf("const refreshLists = useCallback");
    const refreshEnd = source.indexOf(
      "/** 将 acpWorkspace",
      refreshStart,
    );
    const refreshSource = source.slice(refreshStart, refreshEnd);
    const readyIndex = refreshSource.indexOf("setAppBooting(false)");
    const listIndex = refreshSource.indexOf("await Promise.all");

    expect(refreshStart).toBeGreaterThanOrEqual(0);
    expect(refreshEnd).toBeGreaterThan(refreshStart);
    expect(readyIndex).toBeGreaterThanOrEqual(0);
    expect(listIndex).toBeGreaterThan(readyIndex);
    expect(refreshSource).not.toContain("sessionGetState");
    expect(source).not.toContain("appGate");
    expect(source).not.toContain("@/components/RuntimeGate");
  });
});

describe("App 顶栏布局契约", () => {
  it("右侧文件栏显示时不在主标题栏重复预留窗口按钮宽度", () => {
    const appSource = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );

    expect(appSource).toContain(
      'layout.asideCollapsed ? " main--aside-hidden" : ""',
    );
    expect(cssSource).toContain(
      ".platform-win .main--aside-hidden .main__top",
    );
    expect(cssSource).not.toMatch(/\.platform-win \.main__top,\r?\n/);
  });
});

describe("App 自动更新入口契约", () => {
  it("启动后静默检查、每半小时复查，并只在发现版本时显示更新按钮", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain(
      "const APP_UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000",
    );
    expect(source).toContain(
      "window.setInterval(check, APP_UPDATE_CHECK_INTERVAL_MS)",
    );
    expect(source).toContain("window.clearInterval(timer)");
    expect(source).toContain("await checkForAppUpdate()");
    expect(source).toContain(
      "updateAvailable={appUpdateStatus?.available === true}",
    );
    expect(source).toContain("api.APP_UPDATE_STATUS_EVENT");
    expect(source).toContain("onUpdate={requestAppUpdateInstall}");
    expect(source).toContain('title: tr("settings.updateConfirmTitle")');
    expect(source).toContain("open={appUpdateProgressOpen}");
    expect(source).toContain(
      "onClose={() => setAppUpdateProgressOpen(false)}",
    );
    expect(source).not.toContain("keepUpdateProgressOpen");
    expect(source).not.toContain("showClose={false}");
    expect(source).not.toContain("if (!manual && status.available)");
    expect(source).not.toContain('title: tr("app.updateTitle")');
  });
});

describe("设置页按需加载契约", () => {
  it("首次加载设置代码时保持设置页背景，避免窗口短暂露出黑色底层", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain('className="settings-page" aria-busy="true"');
    expect(source).toContain("<Suspense fallback={settingsPageFallback}>");
    expect(source).not.toContain("<Suspense fallback={null}>");
  });
});
