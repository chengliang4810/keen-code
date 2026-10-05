# ZCode 源码映射

2026-10-05 性能适配：来源 `Root.tsx`、`root/DiffsWorkerPoolProvider.tsx`、
`components/ui/code-viewer.tsx`、`diff-viewer.tsx` 保留 DOM 与样式，调整高亮池挂载
生命周期、两个 Worker 上限、16 项 AST 缓存，并移除内容驱动的 React key。
`lib/shikiHighlighter.ts` 使用有界缓存、精确内容键和在途合并；
`workspace-file-tree/useWorkspaceFileSearchIndex.ts`、`WorkspaceFileTree.tsx`、
`command-center/CommandCenterDialog.tsx` 调用 Rust `file.searchWorkspaceFiles`。
原 `workspace-file-search/` 的五个全量索引搬运/Worker 文件按 Rust 接线范围裁剪。
这些来源文件继续使用 ZCode 3.14.3 固定提交、Apache-2.0 许可证；
新增 `apps/desktop/src/frontend_rpc/workspace_search.rs` 为 KeenCode Rust 实现。

2026-10-04 设置入口适配：`WorkspaceSidebarFooter.tsx` 移除账户头像和用户名占位，
合并为设置下拉入口，首项打开完整设置；快捷主题、语言和缩放沿用原组件及服务。
`SettingsPage.tsx` 保留共享菜单与独立返回箭头；这些组件继续使用 ZCode 3.14.3
固定提交与 Apache-2.0 许可证，不修改来源仓库。
本次来源树 SHA256 为
`fd3ff4b80d5179293d56581c52f23b85ce7cd249c240f68f21f563176f48fdd0`，目标树为
`dafabf1db37c1dabc886c8ba604efe9380c16f741ef38ca09d7a0c9f413b8d89`；
仍为 1147 一致、165 适配、238 裁剪、9 新增。产物绑定见
`out/native-live/frontend-provenance-sidebar-settings-menu.json`。

2026-10-04 侧边栏入口适配：`DesktopTopOverlay.tsx` 与
`WorkspaceSidebar/WorkspaceSidebarCollapsedRail.tsx` 始终显示侧边栏图标，移除
Logo/hover 图标切换。`App.tsx`、`app-shell/WorkspaceShellLayout.tsx` 与
`app-shell/types.ts` 清理对应 Logo 参数；功能、tooltip 和快捷键沿用原契约。
这些来源组件继续使用下述 ZCode 3.14.3 Apache-2.0 基线。

侧栏切换图标适配时的来源核验（历史快照）：1550 个来源文件、1321 个目标文件，1147 一致、165 适配、
238 裁剪、9 新增。来源树 SHA256 为
`fd3ff4b80d5179293d56581c52f23b85ce7cd249c240f68f21f563176f48fdd0`，目标树 SHA256 为
`e26162b68955fa07c108cebec971d1baa16e0a7728f1142e3d60341961909291`；详见
`out/native-live/frontend-provenance-sidebar-icon.json`。下述 Source49 为历史冻结快照。

2026-10-04 手工反馈适配：`OccupationOnboarding.tsx` / `OnboardingHeader.tsx` 改为
单页记忆偏好；`interfaceMode.ts` 固定编程模式，`WorkspaceSidebarFooter.tsx` 与
`settingsPageHelpers.tsx` 移除模式切换。`ModelProviderSection.tsx` 和
`model-provider-section/Navigation.tsx` 仅展示自定义供应商列表，移除分组小标题。
这些文件继续来自下述 Apache-2.0 基线，其余主题与组件结构保持来源规则。

2026-10-04 工作树触发器修复：目标新增 `ComposerWorktreeMenu.tsx` 使用独立的工作树名称与图标，
避免与来源 `GitBranchSwitcher.tsx` 重复显示分支名；保留来源分支选择器及 Rust 工作树管理链路。

来源基线：ZCode 3.14.3，提交
`29628c9acdb81b703bbd4080c207a0e7ce5e276e`。

## Source49 冻结来源统计快照

核验时间：2026-10-04（Asia/Shanghai）。Source49 为此前映射快照；固定提交工作树干净；对
`packages/ui/src` 逐文件 SHA-256 比对结果为：目标 1321 个文件，其中 1149 个与来源
一致、163 个为目标适配、9 个为目标新增；固定来源 1550 个文件中有 238 个按产品边界
裁剪。目标新增文件为 `ComposerWorktreeMenu.tsx`、`lib/ungroupTaskPersistence.ts`、
`settings/LocalAutomationsSection.tsx`、`browser-use/NativeBrowserView.tsx`、
`browser-use/nativeBrowserTargetRegistry.ts`、`app-shell/workflowWorkspaceEntry.ts`、
`root/traySessionProjection.ts`、`lib/runtimeStyle.ts`、`settings/subagentTools.ts`。
按相对路径排序后拼接“路径 NUL 字符 + 文件 SHA-256 + 换行”计算的来源树聚合 SHA-256
为 `FD3FF4B80D5179293D56581C52F23B85CE7CD249C240F68F21F563176F48FDD0`，目标树聚合
SHA-256 为 `36A8C94A8159ACF87959C7BB2A1E19157CBD7E982018D095B46888E34F69D9F5`。计数
满足目标 `1149 + 163 + 9 = 1321`、来源 `1149 + 163 + 238 = 1550`。
当次固定来源组件清单 SHA-256 为
`4F349A99603900B2DCB9FE529A5D93CDCCD1A80296998138E7F00407FA6330EC`；ResourceManager
10 个文件、拖拽核对 9 个文件、附件核对 13 个文件均按下文逐项保留。General 的真实
shell/PTY、HTTP Proxy/No Proxy 冷重启验收计划为
`tooling/native-live/general-settings-plan.json`，不改变来源组件归属。

Source45 历史前端重核（2026-10-04 02:26 UTC）确认固定来源树未变；当时目标为 1149 个
一致、163 个适配、8 个新增，来源侧裁剪 238 个，目标树聚合 SHA 为
`AD657DC76DBB484C1B0A15DEFDDF03C6072019E5DA9E12AF572864F316DA6E34`。当前
`apps/ui/dist` 树 SHA 为 `E1636923BD2D803F0FE80743457C685F827B55E39177B88A362B718D41A197DF`。
新增适配仅覆盖空 `thought` 配置、ChannelClient 初始化/释放生命周期和 renderer unload
错误边界，不改变来源 DOM/CSS；前端测试、typecheck、build、CSS 和来源门禁均已通过。
workspace 离线测试为 3119 passed、0 failed、11 ignored，strict feature Clippy 通过；ignored
项不计入 passed。
详见 `out/native-live/source-provenance-native45.json` 和
`out/native-live/runtime-summary-native45.json`。修复前快照另存为相应
`*-pre-repair.json` 并标记过时；Native45 二进制已绑定 SHA
`BA843C08C192E5425AE637662F52047A6E6AF707060F9ED45F99E9E1FA39BD8A`，大小
109,899,776 bytes，元数据为 `out/native-live/native45-build.json`。原生 precheck/full
仍待完成，未将构建成功视为整体验收通过。退役标识的独立扫描见
`out/native-live/retired-frontend-scan-native45.json`，其第三方预览兼容文本仍按
provenance 的 guarded 兼容分支单独统计。

Source48 是当前之前的历史映射；其三件套与 Native48 SHA
`6EFFE7F323A3B3EA66126893CF5457FA42C4EE53D3D7EF04435832E4BEEDC559` 仍保留，不覆盖
Source49。

Source49 使用 Native49 新前端构建；来源树 SHA 保持
`FD3FF4B80D5179293D56581C52F23B85CE7CD249C240F68F21F563176F48FDD0`，目标树 SHA 为
`36A8C94A8159ACF87959C7BB2A1E19157CBD7E982018D095B46888E34F69D9F5`，计数为
1149 一致、163 适配、238 裁剪、9 新增。新增 `settings/subagentTools.ts` 及其测试属于
KeenCode 纯规则适配；Subagents 工具字段保留缺失/空数组/显式列表三态，Rust 三态适配
由受影响 tools/desktop 测试覆盖，来源 DOM/CSS 不变。`apps/ui/dist` 树 SHA 为
`48D4C51238ADD775193CC94A73B2E058691BE341DAC778A117E90993495DDA43`。Native49 已绑定
SHA `DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，大小
110,075,392 bytes，元数据为 `out/native-live/native49-build.json`，UI 构建标签为
`native49`。第二轮 covered native scopes 25/25、2007 steps 已通过；OS/manual 与
release installer 限制仍单独保留。
最终报告为 `out/native-live/source-provenance-native49.json`、
`out/native-live/runtime-summary-native49.json` 和
`out/native-live/retired-frontend-scan-native49.json`。
第二轮批次明细为 `out/native-live/native49-full-batch-second/batch-summary.json`，DOCX22/22
为 `out/native-live/native49-docx-final/report.json`，fresh pixel 为
`out/native-live/native49-full-batch-second/07-ui-layout-final/appearance-pixel-diff.json`。
OS picker、系统托盘、原生通知、OS 坐标拖拽仍需人工或专用原生自动化；release installer
尚未构建或验收。

Source43 已落地的 CSP 样式适配属于目标差异：`lib/runtimeStyle.ts` 从已有静态
`style[nonce]` 读取 Tauri 允许的 style nonce，四个适配调用方为
`presentation/presentationPdfPrintExport.ts`、`previewPaneOfficeDocxContent.tsx`、
`v4/conversationFindHighlightDom.ts` 和 `ModelTrajectorySearchHighlight.ts`。
DOCX 预览库在 detached 容器中生成的样式在挂载前逐个补同一 nonce；未读取脚本 nonce，
也未放宽 CSP。来源 DOM/CSS 和 PDF/DOCX 的预览结构保持不变。

Source44/48 的前端构建与 Native44/48 绑定改为历史索引；其 dist、入口/CSS/worker 哈希和原生
批次结果详见对应 `out/native-live/source-provenance-native44.json` 与
`out/native-live/runtime-summary-native44.json`，不覆盖当前 Source49 映射。

| 来源路径 | 目标路径 | 处理 | 许可证/归属 |
| --- | --- | --- | --- |
| `packages/ui/src/styles.css` | `packages/ui/src/styles.css` | 保留主题入口和语义令牌；只按目标产品删除不可达入口 | ZCode Apache-2.0 |
| `packages/ui/src/components/ui/` | `packages/ui/src/components/ui/` | 保留通用控件 DOM、ARIA 和变体 | ZCode；上游派生片段按组件清单声明 |
| `packages/ui/src/components/ai-elements/` | `packages/ui/src/components/ai-elements/` | 只保留聊天、工具、代码和结果展示所需组件 | ZCode；AI Elements 派生片段 Apache-2.0 |
| `packages/ui/src/i18n/` | `packages/ui/src/i18n/` | 保留 locale 结构；删除裁剪功能的键 | ZCode Apache-2.0 |
| `packages/ui/src/` 业务视图 | `packages/ui/src/` | 保留原 DOM/CSS，接入 KeenCode RPC；不移植来源运行时 | ZCode Apache-2.0 |

Source44 打印页面几何适配仅在 packages/ui/src/presentation/presentationPdfPrintExport.ts 写入页面像素元数据，并由 apps/ui/src/tauriPlatform.ts 按 96 <= px <= 16384 边界发送嵌套 pageSize.widthPx/heightPx；无打印宿主保持无参兼容调用。该适配不改变来源 DOM/CSS 布局，preimage 与 SHA 见 out/native-live/source44-frontend-preimage-manifest.json。

## 当前目标新增与挂载接线

固定来源提交中没有以下八个目标文件，因此它们属于 KeenCode 适配代码，不能按
ZCode 来源文件计入：

| 目标路径 | 作用 | 归属 |
| --- | --- | --- |
| `packages/ui/src/ComposerWorktreeMenu.tsx` | Composer 内的本地 Git worktree 创建、切换、handoff 和删除入口 | KeenCode |
| `packages/ui/src/lib/ungroupTaskPersistence.ts` | 取消任务分组后按 Rust 服务顺序保存顶层任务排序 | KeenCode |
| `packages/ui/src/settings/LocalAutomationsSection.tsx` | 本地自动化设置区，使用目标的 automation store 与 Rust 服务契约 | KeenCode |
| `packages/ui/src/browser-use/NativeBrowserView.tsx` | 浏览器能力的目标桌面视图入口 | KeenCode |
| `packages/ui/src/browser-use/nativeBrowserTargetRegistry.ts` | 受管 child WebView 的目标 owner/generation 注册表 | KeenCode |
| `packages/ui/src/app-shell/workflowWorkspaceEntry.ts` | 根据已加载 Journal 节点决定工作区 transcript CTA 是否可见 | KeenCode |
| `packages/ui/src/lib/runtimeStyle.ts` | 复用已有静态 style nonce，为 Tauri CSP 下的运行时样式提供安全适配 | KeenCode |

`packages/ui/src/resource-manager/` 的 10 个文件均存在于固定来源并按上表保留；
`apps/ui/src/main.tsx` 中将 `ResourceManagerApp` 挂入主 renderer，以及
`apps/desktop/src/frontend_rpc/desktop_controls.rs` 中的 Tauri DTO/桥接，属于
KeenCode 接线，不改变该目录内来源 UI 的 ZCode 归属。

### 已落地功能的逐文件核对

| 功能范围 | 固定来源文件 | 目标保留 | 适配情况 |
| --- | ---: | ---: | --- |
| `resource-manager/` | 10 | 10 | 6 个 SHA-256 一致；`ResourceManagerApp.tsx`、`resourceUsageView.ts`、`storageCategoryPresentation.ts`、`useStorageUsage.ts` 4 个只做 KeenCode host/RPC 接线 |
| workspace/file/workbench/group 拖拽 | 9 个关键文件 | 9 | SHA-256 全部一致，沿用来源 DOM 与拖拽行为 |
| composer/preview/telemetry/AI Elements 附件 | 13 个关键文件 | 13 | SHA-256 全部一致；上传、预览 lease 和 RPC 适配位于来源 UI 之外的目标接线层 |

工作流保留范围还包含来源的 `WorkflowRunSidePane.tsx`、
`WorkflowRunSidePaneSections.tsx`、`WorkflowRunArtifactsSection.tsx` 与
`components/ai-elements/sources.tsx`。这些文件的 SidePane 分区、工作区入口和
Sources CTA 均保留原 DOM/折叠行为，仅增加 KeenCode Journal/RPC 视图值接线；它们
属于来源文件的目标适配，不是新的替代视图。

`v4/ConversationDraftEmptyState.tsx` 保留来源水印的容器、尺寸、遮罩和渐隐语义；
品牌图形是有意的 KeenCode 适配：使用目标 `apps/ui/public/logo.png` 的 SVG
luminance mask 清除深色方形底，再以 `currentColor` 和来源级低对比度呈现 Keen
轮廓，不直接展示 favicon 或 PNG 背景。

云服务范围适配从产品调用图移除了来源的可见反馈入口：帮助菜单、任务菜单和会话
订阅错误态不再渲染 `feedbackStore` 的提交/需求/工单调用；本地点踩/点赞、错误
复制/重试和统一诊断链路保留。`feedbackStore.ts`、反馈草稿 helper 以及未接入的
Feedback/client-config/help remote-config 实现已从目标前端源树裁掉，不构成兼容层或
产品入口。

拖拽核对文件为 `lib/restrictVerticalDragWithinContainer.ts`、
`lib/taskWorkbenchDragPreview.ts`、`lib/workspaceFileDrag.ts`、
`lib/workspaceSidebarDrag.ts`、`prompt-editor/usePromptEditorDragState.ts`、
`v4/workbenchDragDrop.ts`、`v4/workbenchPointerDragDrop.ts`、
`workspace-grouped-tasks/group-drag-overlay.tsx` 和
`workspace-file-tree/useWorkspaceFileTreeRowDragState.ts`。附件核对集合包含
`ChatMediaAttachmentPreviewDialog.tsx`、`components/ai-elements/attachments.tsx`、
`lib/chatAttachments.ts`、`store/composerAttachmentUploadStore.ts`、v4 upload/chip、
transaction 和 telemetry 文件。完整计数、SHA-256 和构建产物证据见
`docs/frontend-source-runtime-provenance-20261003.md`、
`out/native-live/source-provenance-native44.json` 和
`out/native-live/source-provenance-runtime-20261003.json`；Native44 独立构建摘要为
`out/native-live/runtime-summary-native44.json`。

## Fixed-source 组件清单与实际范围

固定来源提交中的 `third-party/copied-components.json` 原始 SHA-256 为
`4F349A99603900B2DCB9FE529A5D93CDCCD1A80296998138E7F00407FA6330EC`。目标树的
同名清单仅将 Material Icon Theme 的 `roots` 映射到实际静态资源路径，并以
`sourceRoots` 保留原路径；许可证引用与原始导入版本未知的事实均保留。下表以目标
实际路径为准；“裁剪”表示目标树没有保留该来源范围，不表示来源清单中的文件被隐式带入产品。
当前目标清单 SHA-256 为 `3034623C5E8AFA28271BC982B7B45B2C9047CAD6BB8F45B3165C2E463C10C74F`。

| 清单条目 | 目标实际保留 | 明确裁剪/未复制 |
| --- | --- | --- |
| shadcn | `packages/ui/src/components/ui/` 45 个当前文件；`packages/ui/src/styles.css` | 同目录中的 KeenCode/ZCode 文件不按 shadcn 归属；仅上游派生片段适用 MIT |
| ai-elements | `packages/ui/src/components/ai-elements/` 46 个当前文件，其中 fixed manifest 列出并保留 35 个 Apache-2.0 派生文件 | `.agents/skills/ai-elements/` 整个来源根未复制；其余 11 个组件文件不由该清单声明为 AI Elements 派生 |
| Visual Studio Code IPC/common utilities | `packages/rpc/src/` 16 个 RPC 源文件；`packages/shared/src/zcode-protocol-v4/wire-codec.ts` | `packages/rpc/examples/` 四个 Node demo 已移除；目标产品不保留 Node 示例宿主 |
| Superpowers skill description adaptations | `packages/ui/src/lib/builtinSkillI18n.ts` | `apps/zcode-cli/packages/superpowers-plugin/LICENSE` 来源根未复制 |
| Fig autocomplete registry | 无当前目标路径 | `apps/zcode-cli/packages/core/src/tool/handlers/generated/bash-command-registry.ts` 整项裁剪 |
| Material Icon Theme | `apps/ui/public/material-icons/` 1146 个 SVG，由 `packages/ui/src/lib/fileDisplay.tsx` 使用；保留 MIT 许可证 | 原 desktop/web 路径未复制，统一映射到 Tauri 前端静态资源根 |
| agent-browser skills | 无当前目标路径 | `.agents/skills/agent-browser`、`.agents/skills/dogfood`、`.agents/skills/electron` 整项裁剪 |
| React Best Practices skill | 无当前目标路径 | `.agents/skills/react-best-practices` 整项裁剪 |

适用的五份上游许可证文本位于 `third-party/upstream/`，均与固定来源清单
SHA-256 一致：shadcn `1564074E13439397221FFD522E2E504D56561994A23D371AA5E3AD43E4F5423F`、
AI Elements `B4F9ADB7C568904834D0DD6CC98D16C390D21CA32FC17AE7A267715269BD5529`、
VS Code IPC `9480271317925265E806A9A196AAA33410A962FA9D4D1E248A4A5187BC8C9DF9`、
Superpowers `A37E0E9697144819E1D965176AC4AE5BC3FA02D11E7812036BBCADF6DAFE2400`、
Material Icon Theme `CDAB3014D4F69B49DDE2B85E81792208C72DE613AA6AED7F7A9B5C6609B89670`。

## 设计门禁来源特征

`design-baseline.json` 由 `tooling/scripts/generate-zcode-design-baseline.mjs`
从上述固定提交生成。它只列出固定来源中实际触发设计规则的文件、完整 SHA256、
规则 ID 和三行上下文特征 SHA256。门禁只有在文件完整哈希一致，或违规行的行哈希
与上下文特征同时一致时才放行；同一目录中新加的代码、改动后的字号/颜色、原生
控件和 inline style 不会因为位于 `packages/ui/src/` 就自动豁免。

生成/审查命令：

```powershell
node tooling/scripts/generate-zcode-design-baseline.mjs D:\projects\ZCode 29628c9acdb81b703bbd4080c207a0e7ce5e276e
node tooling/scripts/design-system-gate.mjs
```

本次固定来源设计基线包含 207 个规则文件条目，文件 SHA-256 为
`84743917D8853309EAE63D43411A5D19028FA37011AE305B65566BE0D7C083B9`。

`.stylelintrc.json` 只对 `packages/ui/src/onboarding/onboardingLogoSweep.css` 这一个
来源文件做精确忽略，以保留其原始 logo gradient；设计门禁仍按本清单的完整哈希和
行特征检查该文件，新增或修改的颜色不会被静默放行。

迁移期间的 `apps/ui/src/` 或根 `src/` 仅是暂存路径，必须在完成迁移后收敛到
`packages/ui/src/` 或在本表增加明确的临时原因。映射不是对整棵仓库的通用
忽略规则：设计门禁仍检查文字 token、颜色角色、inline style 和来源外新增
文件；clean-room 门禁只在本表和组件级清单声明的路径放行合法来源词。

## 差异记录

- RPC 使用 KeenCode `ChannelClient` VQL 100-204；v4 wire version 为 3，
  projection snapshot version 为 1。
- 每个窗口拥有 window-bound connection；Journal 是会话与执行事实源，
  snapshot 只恢复显示投影，active barrier 由 Rust host 管理。
- WorkflowDefinitionV1 是 Rust `serde` 的纯 JSON 契约，桌面不运行 Node 或
  JavaScript workflow runtime；UI 节点形状不等于执行 schema。
- `packages/services/src/zcode-agent/zcodeAgent.ts` 只保留 Rust host 的 service
  descriptor 与 RPC 类型，不是 TypeScript Agent 执行器；`packages/rpc/examples/`
  的四个 Node demo 和 `ts-node` 入口已裁剪。
- 官方账号、支付、云端、机器人、SSH、WSL、Docker、浏览器计算机控制以及其
  locale、菜单和资源入口均被裁剪。
- 模型推理编辑器允许删除最后一个 chip，并将显式空的个人
  `reasoning_efforts` 保持为 `[]`；目录能力校验仍要求完整目录项非空，避免空值
  回退到默认档位。
- Windows Command Prompt 在 `terminal_inherit_system_profile=false` 时由真实 PTY
  参数附加 `/d` 以禁止 AutoRun；其他保留 shell 的参数不改变。
- 浏览器打开尺寸使用嵌套 `BrowserOpenBounds` DTO 传给 Rust，配合 owner/generation
  校验，避免旧 child WebView 的迟到回调更新新 tab。

依赖快照：当前 `pnpm-lock.yaml` SHA-256 为
`6DD089FFE1A7A5ECDE965BC76E77953240589516432260A7B1CA8591B29E6500`。这是前端替换
阶段已有的 workspace 依赖重写；本轮仅刷新来源记录，没有继续修改 lockfile。
