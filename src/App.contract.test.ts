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
    expect(source).toContain("maxAttempts: view.retry.maxAttempts");
    expect(source).toContain("view.retry = null");
  });
});

describe("App 当前会话投影隔离契约", () => {
  it("新建草稿时不复用上一会话的 ACP 视图", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const selectorStart = source.indexOf("const acpSessionView = useMemo(");
    const selectorEnd = source.indexOf(
      "const displayedSubagents = useMemo(",
      selectorStart,
    );
    const selectorSource = source.slice(selectorStart, selectorEnd);

    expect(selectorStart).toBeGreaterThanOrEqual(0);
    expect(selectorEnd).toBeGreaterThan(selectorStart);
    expect(selectorSource).toContain("acpWorkspace.sessions[session.sessionId]");
    expect(selectorSource).toContain("[acpWorkspace, session.sessionId]");
    expect(selectorSource).not.toContain("viewingSessionIdRef.current");
  });

  it("新建草稿隐藏摘要和右侧栏按钮并关闭摘要浮层", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toMatch(
      /\{session\.sessionId \? \(\s*<div className="main__top-actions">/,
    );
    expect(source).toContain("setSummaryOpen(false)");
  });

  it("摘要宽屏占位，窄屏降级为悬浮面板", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );

    expect(source).toContain('summaryOpen ? " main__stage--summary-open" : ""');
    expect(cssSource).toMatch(
      /@container \(min-width: 860px\)[\s\S]*?\.main__stage--summary-open > \.lobe-chat[\s\S]*?width: calc\(100% - 352px\);/,
    );
    expect(cssSource).toMatch(
      /\.main__stage--summary-open > \.composer-wrap--float,[\s\S]*?right: 352px;/,
    );
    expect(cssSource).toContain(
      "transition: width var(--motion-enter) var(--ease-out)",
    );
    expect(cssSource).toContain(
      "animation: summary-panel-in var(--motion-enter) var(--ease-out)",
    );
    expect(cssSource).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]*?\.summary-panel[\s\S]*?animation: none;/,
    );
  });

  it("摘要使用轻量工具卡片层级", () => {
    const componentSource = readFileSync(
      new URL("./components/ConversationSummaryPanel.tsx", import.meta.url),
      "utf8",
    );
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );
    const panelCss = cssSource.slice(
      cssSource.indexOf(".summary-panel {"),
      cssSource.indexOf("@keyframes summary-panel-in"),
    );
    const headerCss = cssSource.slice(
      cssSource.indexOf(".summary-panel__header {"),
      cssSource.indexOf(".summary-panel__title {"),
    );

    expect(panelCss).toContain("width: 320px");
    expect(panelCss).toContain("border-radius: var(--radius-composer)");
    expect(headerCss).not.toContain("border-bottom");
    expect(componentSource).not.toContain("summary-panel__header-icon");
    expect(componentSource).not.toContain('tr("summary.filesChanged"');
  });

  it("右侧资源面板与摘要面板互斥切换", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const start = source.indexOf("const previousAsideCollapsedRef");
    const end = source.indexOf("const summaryTriggerRef", start);
    const transitionSource = source.slice(start, end);

    expect(transitionSource).toContain("useLayoutEffect");
    expect(transitionSource).toContain(
      "if (previous === layout.asideCollapsed) return",
    );
    expect(transitionSource).toContain(
      "setSummaryOpen(layout.asideCollapsed && Boolean(session.sessionId))",
    );
    expect(source).toContain(
      "dismissOnOutsidePress={!layout.asideCollapsed}",
    );
  });
});

describe("App 添加项目契约", () => {
  it("选择源文件夹只在名称未手动编辑时填充默认名称", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const applySource = source.slice(
      source.indexOf("const applyAddProjectSource"),
      source.indexOf("const selectAddProjectSourceFromPaths"),
    );
    const resetSource = source.slice(
      source.indexOf("const resetAddProject"),
      source.indexOf("const openAddProject"),
    );
    const nameInputSource = source.slice(
      source.indexOf('id="add-project-name"'),
      source.indexOf('htmlFor="add-project-source"'),
    );

    expect(applySource).toContain("if (!addProjectNameEditedRef.current)");
    expect(resetSource).toContain("addProjectNameEditedRef.current = false");
    expect(nameInputSource).toContain("addProjectNameEditedRef.current = true");
  });

  it("只填写名称即可创建，已有目录保持可选", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const submitSource = source.slice(
      source.indexOf("const submitAddProject"),
      source.indexOf("const addProject ="),
    );

    expect(submitSource).toContain("api.projectCreate(");
    expect(submitSource).toContain("addProjectPath || null");
    expect(submitSource).not.toContain("!addProjectPath");
  });
});

describe("App 计划模式契约", () => {
  it("会话级开关贯穿发送链并在草稿转正时迁移", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    // 发送链：send → enqueue/executeSend → sessionSend。
    expect(source).toContain("planMode: planModeSelected");
    expect(source).toMatch(/sessionSend\(\{[^}]*planMode,/s);
    // 队列快照保存当前会话的真实计划模式，而不是固定关闭。
    expect(source.match(/planMode: planModeSelected/g)).toHaveLength(2);
    // 草稿首发建立的会话继承开关。
    expect(source).toContain("setPlanModeSessionKey(sessionId)");
    // /plan slash 命令与 composer chip 均可切换。
    expect(source).toContain('case "plan"');
    expect(source).toContain("ComposerPlanModeChip");
    expect(source).not.toContain("ComposerPlanModeHint");
  });

  it("计划 chip 与目标 chip 同显示逻辑，且两模式互斥", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    // 仅在计划模式激活时渲染 chip（目标模式同款条件渲染）。
    expect(source).toMatch(
      /\{planModeSessionKey === \(session\.sessionId \?\? "__draft__"\)\s*\?\s*\(\s*<ComposerPlanModeChip/,
    );

    // slash 入口互斥：开启任一模式时清掉另一模式的会话键。
    const goalStart = source.indexOf('case "goal"');
    const goalEnd = source.indexOf('case "plan"', goalStart);
    expect(source.slice(goalStart, goalEnd)).toContain(
      "setPlanModeSessionKey(null)",
    );
    const planStart = source.indexOf('case "plan"');
    const planEnd = source.indexOf('case "status"', planStart);
    expect(source.slice(planStart, planEnd)).toContain(
      "setGoalModeSessionKey(null)",
    );
  });

  it("api 层显式透传 planMode 到 session_send", () => {
    const apiSource = readFileSync(
      new URL("./lib/acp/api.ts", import.meta.url),
      "utf8",
    );
    expect(apiSource).toContain("planMode: args.planMode ?? false");
  });
});

describe("App Ultra 模式契约", () => {
  it("模型和思考程度面板共享互斥状态，迟到的关闭事件不影响新面板", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toMatch(
      /const \[composerPanel, setComposerPanel\] = useState<[\s\S]*?"model" \| "reasoning" \| null[\s\S]*?>\(null\)/,
    );
    expect(source).toContain('open={composerPanel === "model"}');
    expect(source).toContain('open={composerPanel === "reasoning"}');
    expect(source).toContain(
      'open ? "model" : current === "model" ? null : current',
    );
    expect(source).toMatch(
      /open\s*\? "reasoning"\s*:\s*current === "reasoning"\s*\? null\s*:\s*current/,
    );
  });

  it("与 Goal 和推理强度独立，并贯穿直接发送、队列和编辑重发", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const apiSource = readFileSync(
      new URL("./lib/acp/api.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain("<ComposerReasoningMenu");
    expect(source).toContain("ultra={");
    expect(source.match(/ultraMode: ultraModeSelected/g)).toHaveLength(2);
    expect(source).toMatch(/sessionSend\(\{[^}]*ultraMode,/s);
    expect(source).toContain("ultraMode: ultraModeSessionKey === sessionId");
    expect(source).toContain("setUltraModeSessionKey(sessionId)");
    expect(apiSource).toContain("ultraMode: args.ultraMode ?? false");

    const ultraToggleStart = source.indexOf("onUltra={(enabled)");
    const ultraToggle = source.slice(
      ultraToggleStart,
      source.indexOf("{hasStartedConversation", ultraToggleStart),
    );
    expect(ultraToggle).not.toContain("setGoalModeSessionKey");
    expect(ultraToggle).not.toContain("setPlanModeSessionKey");
  });
});

describe("App 编辑重发契约", () => {
  it("保留会话 id，归档旧分支，并在原会话发送编辑内容", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");
    const apiSource = readFileSync(new URL("./lib/acp/api.ts", import.meta.url), "utf8");

    expect(source).toContain("sessionPrepareEditLastUser");
    expect(source).toContain("updateSessionPreference(prepared.archivedBranchId, { archived: true })");
    expect(source).toMatch(/executeSend\(\{[\s\S]*?storedDisplay: content,[\s\S]*?targetSessionId: sessionId,/);
    expect(source).toContain("onEditLastUserMessage={editAndResendLastUserMessage}");
    expect(apiSource).toContain('"session_prepare_edit_last_user"');
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

describe("输入指令候选面板契约", () => {
  it("过滤时不重复显示输入内容和候选数量", () => {
    const source = readFileSync(
      new URL("./components/ComposerPlusPanel.tsx", import.meta.url),
      "utf8",
    );
    const cssSource = readFileSync(
      new URL("./styles/app.css", import.meta.url),
      "utf8",
    );

    expect(source).not.toContain("composer-plus__filter");
    expect(cssSource).not.toContain(".composer-plus__filter");
  });
});

describe("左侧栏空栏目与快捷入口契约", () => {
  it("项目会话默认显示 5 个并按 5 个追加", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain("visibleSessionsByProject[proj.id] ?? 5");
    expect(source).toMatch(
      /projSessions\.slice\(\s*0,\s*visibleSessionCount,\s*\)/,
    );
    expect(source).toContain("[proj.id]: visibleSessionCount + 5");
    expect(source).toContain("filter(([id]) => id !== proj.id)");
  });

  it("项目栏目默认展开、具体项目默认收起", () => {
    const source = readFileSync(new URL("./App.tsx", import.meta.url), "utf8");

    expect(source).toContain(
      "const [projectsOpen, setProjectsOpen] = useState(true);",
    );
    expect(source).toContain(
      "projection.projects.map((project) => [project.id, false])",
    );
  });

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
  it("仅在空白新任务中显示居中欢迎语，标题不回落到“新任务”", () => {
    const appSource = readFileSync(
      new URL("./App.tsx", import.meta.url),
      "utf8",
    );

    expect(appSource).toContain("const showWelcomeCopy =");
    expect(appSource).toContain("suppressEmptyCopy={!showWelcomeCopy}");
    expect(appSource).toContain("isPlaceholderSessionTitle(title");
    expect(appSource).toContain('tr("sidebar.newSession")');
    expect(appSource).toContain("IconNewChat");
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
