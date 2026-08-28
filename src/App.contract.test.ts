import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

function readSource(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}

vi.mock("@/components/ResourceViewer", () => ({
  ResourceViewer: () => null,
}));

import {
  parseElicitationPayload,
  toElicitationAnswers,
} from "./lib/elicitation";

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
    const eventSource = readSource("./hooks/acp-runtime/events.ts");
    const projectionSource = readSource("./hooks/acp-runtime/projection.ts");
    const listenerStart = eventSource.indexOf(
      'listenAcp("acp://agent-event"',
    );
    const listenerEnd = eventSource.indexOf(
      'listenAcp("acp://recovery-status"',
      listenerStart,
    );
    const listenerSource = eventSource.slice(listenerStart, listenerEnd);

    expect(eventSource).toContain(
      "shouldDriveMainSessionStreaming(params.update, sourceAgentId)",
    );
    expect(eventSource).toMatch(
      /const sourceAgentId = resolveSessionUpdateSourceAgentId\(\s*view,\s*params\._peri\?\.sourceAgentId,\s*\)/s,
    );
    expect(listenerSource.indexOf("parseAgentEvent(params.event_json)")).toBeLessThan(
      listenerSource.indexOf("if (!params.sessionId) return"),
    );
    expect(listenerSource).toContain('event.type === "turn_suspended"');
    expect(projectionSource).toContain("maxAttempts: view.retry.maxAttempts");
    expect(eventSource).toContain("view.retry = null");
  });
});

describe("App 当前会话投影隔离契约", () => {
  it("新建草稿时不复用上一会话的 ACP 视图", () => {
    const source = readSource("./App.tsx");
    const navigationSource = readSource("./hooks/useSessionNavigation.ts");
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
    expect(navigationSource).toContain("viewingSessionIdRef.current = null");
    expect(navigationSource).toContain("current.ui.setMessages([])");
  });

  it("离开空草稿时覆盖旧快照，避免恢复已清空内容", () => {
    const source = readSource("./hooks/useSessionNavigation.ts");
    const start = source.indexOf("const snapshotOutgoingDraft = useCallback(");
    const end = source.indexOf("const openSession = useCallback", start);
    const snapshotSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(snapshotSource).toContain(
      "draftNavigationSnapshotRef.current = snapshotDraftNavigation(",
    );
    expect(snapshotSource).not.toContain("hasDraftContent");
  });

  it("新建草稿隐藏摘要和右侧栏按钮并关闭摘要浮层", () => {
    const appSource = readSource("./App.tsx");
    const mainHeaderSource = readSource("./features/app/main/MainHeader.tsx");
    const navigationSource = readSource("./hooks/useSessionNavigation.ts");

    expect(mainHeaderSource).toMatch(
      /\{session\.sessionId \? \(\s*<div className="main__top-actions">/,
    );
    expect(appSource).toContain("setSummaryOpen(false)");
    expect(navigationSource).toContain("current.ui.closeSummary()");
  });

  it("摘要宽屏占位，窄屏降级为悬浮面板", () => {
    const source = readSource("./features/app/MainStage.tsx");
    const cssSource = readSource("./styles/app.css");

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
    const componentSource = readSource(
      "./components/ConversationSummaryPanel.tsx",
    );
    const cssSource = readSource("./styles/app.css");
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
    const appSource = readSource("./App.tsx");
    const conversationStageSource = readSource(
      "./features/app/main/ConversationStage.tsx",
    );
    const start = appSource.indexOf("const previousAsideCollapsedRef");
    const end = appSource.indexOf("const summaryTriggerRef", start);
    const transitionSource = appSource.slice(start, end);

    expect(transitionSource).toContain("useLayoutEffect");
    expect(transitionSource).toContain(
      "if (previous === layout.asideCollapsed) return",
    );
    expect(transitionSource).toContain(
      "setSummaryOpen(layout.asideCollapsed && Boolean(session.sessionId))",
    );
    expect(conversationStageSource).toContain(
      "dismissOnOutsidePress={!layout.asideCollapsed}",
    );
  });
});

describe("App 添加项目契约", () => {
  it("选择源文件夹只在名称未手动编辑时填充默认名称", () => {
    const source = readSource("./hooks/useProjectDialog.ts");
    const modalSource = readSource("./features/app/overlays/AddProjectModal.tsx");
    const applySource = source.slice(
      source.indexOf("const applyAddProjectSource"),
      source.indexOf("const selectAddProjectSourceFromPaths"),
    );
    const resetSource = source.slice(
      source.indexOf("const resetAddProject"),
      source.indexOf("const openAddProject"),
    );
    const nameInputSource = modalSource.slice(
      modalSource.indexOf('id="add-project-name"'),
      modalSource.indexOf('htmlFor="add-project-source"'),
    );

    expect(applySource).toContain("if (!addProjectNameEditedRef.current)");
    expect(resetSource).toContain("addProjectNameEditedRef.current = false");
    expect(nameInputSource).toContain("effectiveNameEditedRef.current = true");
  });

  it("只填写名称即可创建，已有目录保持可选", () => {
    const source = readSource("./hooks/useProjectDialog.ts");
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
    const draftSource = readSource("./hooks/session-turn/useSessionDraftSend.ts");
    const sendSource = readSource("./hooks/session-turn/useSessionSend.ts");
    const composerSource = readSource("./hooks/useComposerController.ts");
    const stageSource = readSource("./features/app/main/ComposerToolbar.tsx");

    // 发送链：send → enqueue/executeSend → session_send。
    expect(draftSource).toContain("const planMode = planModeSessionKey === key");
    expect(draftSource).toMatch(/sendQueue\.enqueue\(\{[\s\S]*?planMode,/s);
    expect(draftSource).toMatch(/executeSend\(\{[\s\S]*?planMode,/s);
    expect(sendSource).toContain("planMode = false");
    expect(sendSource).toMatch(/api\.send\(\{[\s\S]*?planMode,/s);
    // 队列快照保存当前会话的真实计划模式，而不是固定关闭。
    // 草稿首发建立的会话继承开关。
    expect(sendSource).toContain(
      "if (planMode) setPlanModeSessionKey(resolvedSessionId)",
    );
    // /plan slash 命令与 composer chip 均可切换。
    expect(composerSource).toContain('case "plan"');
    expect(stageSource).toContain("ComposerPlanModeChip");
    expect(stageSource).not.toContain("ComposerPlanModeHint");
  });

  it("计划 chip 与目标 chip 同显示逻辑，且两模式互斥", () => {
    const composerSource = readSource("./hooks/useComposerController.ts");
    const stageSource = readSource("./features/app/main/ComposerToolbar.tsx");

    // 仅在计划模式激活时渲染 chip（目标模式同款条件渲染）。
    expect(stageSource).toContain("const sessionKey = session.sessionId ?? \"__draft__\";");
    expect(stageSource).toMatch(
      /\{planModeSessionKey === sessionKey \?\s*\(\s*<ComposerPlanModeChip/,
    );

    // slash 入口互斥：开启任一模式时清掉另一模式的会话键。
    const goalStart = composerSource.indexOf('case "goal"');
    const goalEnd = composerSource.indexOf('case "plan"', goalStart);
    expect(composerSource.slice(goalStart, goalEnd)).toContain(
      "setPlanModeSessionKey(null)",
    );
    const planStart = composerSource.indexOf('case "plan"');
    const planEnd = composerSource.indexOf('case "status"', planStart);
    expect(composerSource.slice(planStart, planEnd)).toContain(
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
    const composerSource = readSource("./hooks/useComposerController.ts");
    const stageSource = readSource("./features/app/main/ComposerToolbar.tsx");

    expect(composerSource).toMatch(
      /const \[composerPanel, setComposerPanel\] = useState<[\s\S]*?"model" \| "reasoning" \| null[\s\S]*?>\(null\)/,
    );
    expect(stageSource).toContain('open={composerPanel === "model"}');
    expect(stageSource).toContain('open={composerPanel === "reasoning"}');
    expect(stageSource).toContain(
      'open ? "model" : current === "model" ? null : current',
    );
    expect(stageSource).toMatch(
      /open\s*\? "reasoning"\s*:\s*current === "reasoning"\s*\? null\s*:\s*current/,
    );
  });

  it("与 Goal 和推理强度独立，并贯穿直接发送、队列和编辑重发", () => {
    const draftSource = readSource("./hooks/session-turn/useSessionDraftSend.ts");
    const sendSource = readSource("./hooks/session-turn/useSessionSend.ts");
    const editSource = readSource("./hooks/session-turn/useSessionEditResend.ts");
    const stageSource = readSource("./features/app/main/ComposerToolbar.tsx");
    const apiSource = readSource("./lib/acp/api.ts");

    expect(stageSource).toContain("<ComposerReasoningMenu");
    expect(stageSource).toContain("ultra={");
    expect(draftSource).toMatch(/sendQueue\.enqueue\(\{[\s\S]*?ultraMode,/s);
    expect(draftSource).toMatch(/executeSend\(\{[\s\S]*?ultraMode,/s);
    expect(sendSource).toMatch(/api\.send\(\{[\s\S]*?ultraMode,/s);
    expect(sendSource).toContain(
      "if (ultraMode) setUltraModeSessionKey(resolvedSessionId)",
    );
    expect(editSource).toContain("ultraMode: ultraModeSessionKey === sessionId");
    expect(apiSource).toContain("ultraMode: args.ultraMode ?? false");

    const ultraToggleStart = stageSource.indexOf("onUltra={(enabled)");
    const ultraToggle = stageSource.slice(
      ultraToggleStart,
      stageSource.indexOf("{hasStartedConversation", ultraToggleStart),
    );
    expect(ultraToggle).not.toContain("setGoalModeSessionKey");
    expect(ultraToggle).not.toContain("setPlanModeSessionKey");
  });
});

describe("App 编辑重发契约", () => {
  it("保留会话 id，归档旧分支，并在原会话发送编辑内容", () => {
    const lifecycleSource = readSource("./hooks/useSessionLifecycleActions.ts");
    const editSource = readSource("./hooks/session-turn/useSessionEditResend.ts");
    const stageSource = readSource("./features/app/main/ConversationStage.tsx");
    const apiSource = readSource("./lib/acp/api.ts");

    expect(lifecycleSource).toContain(
      "prepareEditLastUser: sessionPrepareEditLastUser",
    );
    expect(editSource).toContain("api.prepareEditLastUser");
    expect(editSource).toContain(
      "updateSessionPreference(prepared.archivedBranchId, { archived: true })",
    );
    expect(editSource).toMatch(
      /executeSend\(\{[\s\S]*?storedDisplay: content,[\s\S]*?targetSessionId: sessionId,/,
    );
    expect(stageSource).toContain("onEditLastUserMessage={editAndResendLastUserMessage}");
    expect(apiSource).toContain('"session_prepare_edit_last_user"');
  });
});

describe("App 启动工作台契约", () => {
  it("先开放工作台，再异步恢复列表，不使用会话状态充当启动门禁", () => {
    const appSource = readSource("./App.tsx");
    const sidebarSource = readSource("./hooks/sidebar/useSidebarLists.ts");
    const refreshStart = sidebarSource.indexOf("const refreshLists = useCallback");
    const refreshEnd = sidebarSource.indexOf("const refreshSessions", refreshStart);
    const refreshSource = sidebarSource.slice(refreshStart, refreshEnd);
    const readyIndex = refreshSource.indexOf("setAppBooting(false)");
    const listIndex = refreshSource.indexOf("await Promise.all");

    expect(refreshStart).toBeGreaterThanOrEqual(0);
    expect(refreshEnd).toBeGreaterThan(refreshStart);
    expect(readyIndex).toBeGreaterThanOrEqual(0);
    expect(listIndex).toBeGreaterThan(readyIndex);
    expect(refreshSource).not.toContain("sessionGetState");
    expect(appSource).not.toContain("appGate");
    expect(appSource).not.toContain("@/components/RuntimeGate");
  });
});

describe("App 顶栏布局契约", () => {
  it("右侧文件栏显示时不在主标题栏重复预留窗口按钮宽度", () => {
    const appSource = readSource("./features/app/MainStage.tsx");
    const cssSource = readSource("./styles/app.css");
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
    const appSource = readSource("./App.tsx");
    const updateSource = readSource("./hooks/useAppUpdate.ts");
    const sidebarSource = readSource("./features/app/Sidebar.tsx");
    const userMenuSource = readSource("./components/UserMenu.tsx");
    const modalSource = readSource("./features/app/overlays/AppUpdateModal.tsx");

    expect(updateSource).toContain("const CHECK_INTERVAL_MS = 30 * 60 * 1000");
    expect(updateSource).toContain("window.setInterval(silentCheck, CHECK_INTERVAL_MS)");
    expect(updateSource).toContain("window.clearInterval(timer)");
    expect(updateSource).toContain("setStatus(await checkForUpdate())");
    expect(sidebarSource).toContain("<UserMenu {...user} />");
    expect(userMenuSource).toContain("updateAvailable: boolean;");
    expect(userMenuSource).toContain("{updateAvailable ? (");
    expect(updateSource).toContain("api.APP_UPDATE_STATUS_EVENT");
    expect(userMenuSource).toContain("onClick={onUpdate}");
    expect(appSource).toContain('title: tr("settings.updateConfirmTitle")');
    expect(appSource).toContain("<AppUpdateModal");
    expect(appSource).toContain("open={appUpdateProgressOpen}");
    expect(appSource).toContain("install={installAppUpdate}");
    expect(modalSource).toContain("onInstall={install}");
    expect(modalSource).toContain(
      "onClose={() => setOpen(false)}",
    );
    expect(appSource).not.toContain("keepUpdateProgressOpen");
    expect(appSource).not.toContain("showClose={false}");
    expect(updateSource).not.toContain("if (!manual && status.available)");
    expect(appSource).not.toContain('title: tr("app.updateTitle")');
  });
});

describe("设置页按需加载契约", () => {
  it("首次加载设置代码时保持设置页背景，避免窗口短暂露出黑色底层", () => {
    const source = readSource("./features/app/SettingsRoute.tsx");
    const cssSource = readSource("./styles/app.css");
    const skinsSource = readSource("./styles/skins.css");
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
    const projectSource = readSource("./features/app/sidebar/ProjectTree.tsx");
    const sidebarSource = readSource("./hooks/sidebar/useSidebarActions.ts");

    expect(projectSource).toContain("visibleSessionsByProject[project.id] ?? 5");
    expect(projectSource).toMatch(
      /projectSessions\.slice\(\s*0,\s*visibleSessionCount,\s*\)/,
    );
    expect(projectSource).toContain("[project.id]: visibleSessionCount + 5");
    expect(sidebarSource).toContain("filter(([id]) => id !== project.id)");
  });

  it("项目栏目默认展开、具体项目默认收起", () => {
    const controllerSource = readSource("./hooks/useSidebarController.ts");
    const listsSource = readSource("./hooks/sidebar/useSidebarLists.ts");

    expect(controllerSource).toContain(
      "const [projectsOpen, setProjectsOpen] = useState(true);",
    );
    expect(listsSource).toContain(
      "projection.projects.map((project) => [project.id, false])",
    );
  });

  it("无置顶或无项目任务时不渲染对应栏目，并在搜索下提供技能和插件入口", () => {
    const pinnedSource = readSource(
      "./features/app/sidebar/PinnedSessionList.tsx",
    );
    const historySource = readSource(
      "./features/app/sidebar/HistorySessionList.tsx",
    );
    const navigationSource = readSource(
      "./features/app/sidebar/SidebarNav.tsx",
    );

    expect(pinnedSource).toContain("if (pinnedSessions.length === 0) return null");
    expect(historySource).toContain("if (orphanSessions.length === 0) return null");
    expect(navigationSource).toContain('navigateSettings("skills")');
    expect(navigationSource).toContain('navigateSettings("market")');
    expect(navigationSource).toContain('tr("sidebar.skills")');
    expect(navigationSource).toContain('tr("sidebar.plugins")');
    expect(pinnedSource).not.toContain("pinnedOpen && pinnedSessions.length > 0");
    expect(historySource).not.toContain("historyOpen && orphanSessions.length > 0");
  });
});

describe("App 新任务文本空态契约", () => {
  it("仅在空白新任务中显示居中欢迎语，标题不回落到“新任务”", () => {
    const appSource = readSource("./App.tsx");
    const headerSource = readSource("./features/app/main/MainHeader.tsx");
    const conversationSource = readSource(
      "./features/app/main/ConversationStage.tsx",
    );

    expect(appSource).toContain("const showWelcomeCopy =");
    expect(appSource).toContain("const showWelcomeCopy = welcomeSession;");
    expect(conversationSource).toContain("suppressEmptyCopy={!showWelcomeCopy}");
    expect(headerSource).toContain("isPlaceholderSessionTitle(title");
    expect(headerSource).toContain('tr("sidebar.newSession")');
    expect(headerSource).toContain("IconNewChat");
  });
});

describe("App 搜索面板布局契约", () => {
  it("通过 body portal 居中覆盖工作台，不作为底部弹性布局项", () => {
    const searchSource = readSource(
      "./features/app/overlays/SessionSearchPortal.tsx",
    );
    const cssSource = readSource("./styles/app.css");

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

describe("App 装配层边界契约", () => {
  it("App.tsx 保持为小型装配层", () => {
    const source = readSource("./App.tsx");
    expect(source.split(/\r?\n/).length).toBeLessThan(2000);
  });

  it("主舞台和侧栏按职责分组，浮层直接装配且不保留纯转发层", () => {
    const appSource = readSource("./App.tsx");
    const mainSource = readSource("./features/app/MainStage.tsx");
    const sidebarSource = readSource("./features/app/Sidebar.tsx");

    expect(mainSource).toContain("export interface MainStageProps");
    expect(mainSource).toContain("<MainHeader {...header}");
    expect(sidebarSource).toContain("export interface SidebarProps");
    expect(sidebarSource).toContain("<UserMenu {...user} />");
    expect(appSource).not.toContain("AppOverlays");
    expect(appSource).toContain("<AddProjectModal");
    expect(appSource).toContain("<AppUpdateModal");
    expect(appSource).toContain("<SessionContextMenu");
    for (const source of [mainSource, sidebarSource]) {
      expect(source).not.toContain("Pick<");
    }
  });
});
