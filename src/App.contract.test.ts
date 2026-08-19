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

describe("App Peri 3.6.5 事件投影契约", () => {
  it("后台更新不驱动主 streaming，并在 Session 过滤前解析 host 事件", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const listenerStart = source.indexOf(
      'listenAcp("acp://agent-event"',
    );
    const listenerEnd = source.indexOf(
      'listenAcp("acp://recovery-status"',
      listenerStart,
    );
    const listenerSource = source.slice(listenerStart, listenerEnd);

    expect(source).toContain(
      "shouldDriveMainSessionStreaming(params.update, sourceAgentId)",
    );
    expect(source).toMatch(
      /const sourceAgentId = resolveSessionUpdateSourceAgentId\(\s*view,\s*params\._peri\?\.sourceAgentId,\s*\)/s,
    );
    expect(listenerSource.indexOf("parseAgentEvent(params.event_json)")).toBeLessThan(
      listenerSource.indexOf("if (!params.sessionId) return"),
    );
    expect(listenerSource).toContain('event.type === "turn_suspended"');
    expect(source).toContain("maxRetries: view.retry.maxAttempts");
    expect(source).toContain("view.retry = null");
  });
});

describe("App 计划模式契约", () => {
  it("会话级开关贯穿发送链并在草稿转正时迁移", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    // 发送链：send → enqueue/executeSend → sessionSend。
    expect(source).toContain("planMode: planModeSelected");
    expect(source).toMatch(/sessionSend\(\{[^}]*planMode,/s);
    // 队列快照随 QueuedSend 持久。
    expect(source).toContain("planMode: false");
    // 草稿首发建立的会话继承开关。
    expect(source).toContain("setPlanModeSessionKey(sessionId)");
    // /plan slash 命令与 composer chip 均可切换。
    expect(source).toContain('case "plan"');
    expect(source).toContain("ComposerPlanModeChip");
    expect(source).toContain("ComposerPlanModeHint");
  });

  it("api 层显式透传 planMode 到 session_send", () => {
    const apiSource = readFileSync(
      new URL("./lib/acp/api.ts", import.meta.url),
      "utf8",
    );
    expect(apiSource).toContain("planMode: args.planMode ?? false");
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
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );
    const skinsSource = readFileSync(
      new URL("./styles/skins.css", import.meta.url),
      "utf8",
    );
    const wallpaperClearRule = skinsSource.indexOf(
      'html[data-wallpaper="1"][data-wallpaper-clear="1"] .sidebar,',
    );
    const wallpaperFallbackRule = skinsSource.indexOf(
      'html[data-wallpaper="1"] .settings-page.settings-page--fallback',
    );

    expect(source).toContain(
      'className="settings-page settings-page--fallback" aria-busy="true"',
    );
    expect(source).toContain("<Suspense fallback={settingsPageFallback}>");
    expect(source).not.toContain("<Suspense fallback={null}>");
    expect(cssSource).toMatch(
      /\.settings-page\.settings-page--fallback\s*\{[^}]*background:\s*var\(--bg-main\);/s,
    );
    expect(wallpaperClearRule).toBeGreaterThanOrEqual(0);
    expect(wallpaperFallbackRule).toBeGreaterThan(wallpaperClearRule);
    expect(skinsSource).toMatch(
      /html\[data-wallpaper="1"\] \.settings-page\.settings-page--fallback\s*\{[^}]*background:\s*var\(--bg-main\)\s*!important;/s,
    );
  });
});

describe("左侧栏空栏目与快捷入口契约", () => {
  it("无置顶或无项目任务时不渲染对应栏目，并在搜索下提供技能和插件入口", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain("{pinnedSessions.length > 0 ? (");
    expect(source).toContain("{orphanSessions.length > 0 ? (");
    expect(source).toContain('navigateSettings("skills")');
    expect(source).toContain('navigateSettings("market")');
    expect(source).toContain('tr("sidebar.skills")');
    expect(source).toContain('tr("sidebar.plugins")');
    expect(source).not.toContain("pinnedOpen && pinnedSessions.length > 0");
    expect(source).not.toContain("historyOpen && orphanSessions.length > 0");
  });
});

describe("App 新任务文本空态契约", () => {
  it("复用聊天线程的空态文案，并稳定放在居中输入区上方", () => {
    const appSource = readFileSync(
      new URL("./App.tsx", import.meta.url),
      "utf8",
    );
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );

    expect(appSource).not.toContain("suppressEmptyCopy={welcomeSession}");
    expect(cssSource).toMatch(
      /\.main__stage:has\(\.composer-wrap--welcome\) \.lobe-chat-empty\s*\{[^}]*position:\s*absolute;[^}]*top:\s*50%;[^}]*transform:\s*translateY\(calc\(-100% - 84px\)\);/s,
    );
  });
});

describe("App 搜索面板布局契约", () => {
  it("通过 body portal 居中覆盖工作台，不作为底部弹性布局项", () => {
    const appSource = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );
    const searchStart = appSource.indexOf("{/* 搜索面板挂载到 body");
    const searchEnd = appSource.indexOf("{/* 应用内确认与输入框", searchStart);
    const searchSource = appSource.slice(searchStart, searchEnd);

    expect(searchStart).toBeGreaterThanOrEqual(0);
    expect(searchEnd).toBeGreaterThan(searchStart);
    expect(searchSource).toContain("createPortal(");
    expect(searchSource).toContain('className="overlay search-overlay"');
    expect(searchSource).toContain("document.body");
    expect(cssSource).toMatch(
      /\.search-overlay\s*\{[^}]*position:\s*fixed;[^}]*align-items:\s*center;[^}]*justify-content:\s*center;/s,
    );
    expect(cssSource).toMatch(/\.search-panel\s*\{[^}]*margin:\s*0;/s);
    expect(cssSource).not.toContain(".overlay:has(> .search-panel)");
  });
});
