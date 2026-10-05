# 前端来源与运行时边界核验

核验时间：2026-10-04（Asia/Shanghai）。Source49 是当前来源与产物快照：源码和 `apps/ui/dist` 已使用 Native49 前端构建，Native49 covered native scopes 的第二轮 25/25、2007 steps 已通过。当前报告保留 OS/manual 与 release installer 限制，不把 covered scopes 通过扩展为未测能力的完整发布验收；本轮只更新来源映射、机器报告和历史索引，没有修改产品源码、dist 或原生二进制。当前机器明细见 `out/native-live/source-provenance-native49.json`、`out/native-live/runtime-summary-native49.json` 和 `out/native-live/retired-frontend-scan-native49.json`；Source48/45/44 及更早报告作为历史 artifact 保留。

Source45 是 Rust/API 容量变更后的最终前端来源重核。最终 `packages/ui/src` 来源树保持不变；RPC/生命周期适配使目标树计数变为 1149 个一致、163 个适配、8 个新增，来源侧裁剪 238 个，目标树聚合 SHA 为 `AD657DC76DBB484C1B0A15DEFDDF03C6072019E5DA9E12AF572864F316DA6E34`。最终 `apps/ui/dist` 共 3632 个文件，树 SHA 为 `E1636923BD2D803F0FE80743457C685F827B55E39177B88A362B718D41A197DF`；前端测试、typecheck、build、CSS、clean-room 和 design-system gate 均已通过。适配范围包括空 `thought` 配置省略、ChannelClient 初始化/释放竞态和 renderer unload 错误边界，均不改变来源 DOM/CSS。独立机器报告为 `out/native-live/source-provenance-native45.json`、`out/native-live/runtime-summary-native45.json`，退役标识扫描为 `out/native-live/retired-frontend-scan-native45.json`；修复前快照另存为相应 `*-pre-repair.json` 并标记过时。Native45 二进制已构建并绑定 SHA `BA843C08C192E5425AE637662F52047A6E6AF707060F9ED45F99E9E1FA39BD8A`（109,899,776 bytes，元数据 `out/native-live/native45-build.json`）；原生 precheck/full 尚未完成，不能将构建成功视为整体验收通过。Source44 canonical 报告继续作为上一构建的历史绑定。

Source48 是 Source49 之前的历史 provenance：其 UI/dist 复用 Source45，Native48 二进制 SHA 为 `6EFFE7F323A3B3EA66126893CF5457FA42C4EE53D3D7EF04435832E4BEEDC559`（110,075,392 bytes）。Source48 的 live/precheck 状态和三件套机器报告继续保留，但不覆盖当前 Source49 的来源树、dist 树或 Native49 绑定。

Source49 最终 provenance 使用 Native49 新前端构建；目标树为 1321 个文件，其中 1149 个与来源一致、163 个为目标适配、9 个为目标新增，来源侧裁剪 238 个。新增 `settings/subagentTools.ts` 纯规则文件，Subagents 设置保留 Rust tools 的继承、禁用全部和显式列表三态，DOM/CSS 不变。Native49 二进制绑定 SHA `DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`（110,075,392 bytes，元数据 `out/native-live/native49-build.json`），UI 构建标签为 `native49`；第二轮 covered native scopes 25/25、2007 steps 已通过，证据为 `out/native-live/native49-full-batch-second/batch-summary.json`，DOCX22/22 证据为 `out/native-live/native49-docx-final/report.json`，fresh pixel 证据为 `out/native-live/native49-full-batch-second/07-ui-layout-final/appearance-pixel-diff.json`。OS picker、系统托盘、原生通知、OS 坐标拖拽仍需人工或专用原生自动化，release installer 尚未验收。机器明细见 `out/native-live/source-provenance-native49.json`、`out/native-live/runtime-summary-native49.json` 和 `out/native-live/retired-frontend-scan-native49.json`。

## 固定来源与备份

| 项目 | 证据 |
| --- | --- |
| ZCode 来源 | `D:/projects/ZCode`，Git HEAD `29628c9acdb81b703bbd4080c207a0e7ce5e276e`，工作树无未提交文件；本轮核验，HEAD tree `e7458be062f467b465abc91509adb0558e023605` |
| 目标 HEAD | `9e4ebc9534917b48ffb2208cd847754dd0ad829e` |
| 目标替换备份 | `D:/projects/keen-code-backups/zcode-replacement-20261002-215853`，HEAD 与目标一致，manifest 6020 项 |
| 目标备份 manifest | SHA-256 `6C86EB5C53CD15B06FCA920B47264E992B8140CD9714370696DD5445B6F5F30E` |
| 目标备份范围 | `apps/ui/` 5647、`packages/` 256、`apps/desktop/` 53、`core/` 46、`third-party/` 8、`docs/` 4 |
| 前端旧备份 | `D:/projects/keen-code-backups/frontend-20260930-223519`，记录 530 个已验证 tracked 文件；来源字段指向历史主题仓库，不作为当前来源 |

## 实际复制范围

以固定来源工作树的 `packages/ui/src` 对当前 `packages/ui/src` 做逐文件 SHA-256 比对：Source49 当前 1321 个文件中 1149 个完全一致，163 个为目标适配修改，9 个为目标新增；来源剩余 238 个文件被裁剪。当前 `1149 + 163 + 9 = 1321`，来源侧 `1149 + 163 + 238 = 1550`。其中 `hooks/useTextSelection.ts` 的 React #185 递归引用比较修复属于适配修改。目标新增文件为 `ComposerWorktreeMenu.tsx`、`lib/ungroupTaskPersistence.ts`、`settings/LocalAutomationsSection.tsx`、`browser-use/NativeBrowserView.tsx`、`browser-use/nativeBrowserTargetRegistry.ts`、`app-shell/workflowWorkspaceEntry.ts`、`root/traySessionProjection.ts`、`lib/runtimeStyle.ts` 与 `settings/subagentTools.ts`，ResourceManager 的来源组件仍属于固定来源；主 renderer 挂载和 KeenCode RPC 接线属于目标适配。来源树聚合使用版本 `frontend-provenance-v1`：Node 默认 `Array.sort()` UTF-16 代码单元顺序、`/` 路径分隔、每行“路径 NUL 字符 + 文件 SHA-256 小写十六进制 + LF”，再以 UTF-8 计算 SHA-256。按该规范，来源树聚合 SHA-256 为 `FD3FF4B80D5179293D56581C52F23B85CE7CD249C240F68F21F563176F48FDD0`，目标树聚合 SHA-256 为 `36A8C94A8159ACF87959C7BB2A1E19157CBD7E982018D095B46888E34F69D9F5`。此前报告中的 `F869...`、`6FBC...` 不能由当前固定规范和树快照复现，按过期观察处理，不作为来源发生变化的证据。该统计反映当前 Source49 适配冻结后的源码快照；`tooling/scripts/native-live-browser-surface.mjs` 及其测试是 native CDP 目标稳定性 harness，位于浏览器运行时之外，不进入 dist。External Editor Host 若继续改变目标宿主文件，最终构建报告需重新记录宿主适配哈希。

Source43 已落地的 CSP 适配只解决 Tauri `style-src-elem` 对运行时 `<style>` 的阻断，不放宽 CSP：目标新增 `lib/runtimeStyle.ts` 只从已有静态 `style[nonce]` 读取样式 nonce，不读取脚本 nonce；`presentation/presentationPdfPrintExport.ts`、`previewPaneOfficeDocxContent.tsx`、`v4/conversationFindHighlightDom.ts` 和 `ModelTrajectorySearchHighlight.ts` 是四个适配调用方。DOCX 预览库在 detached 容器中生成的样式也在挂载前逐个复用同一静态 style nonce。该改动保持来源 DOM/CSS 结构，仅使 PDF、DOCX 和高亮样式在现有 CSP 下可用。

Source44 另增加打印页面几何适配：presentation/presentationPdfPrintExport.ts 在隐藏打印宿主写入页面像素尺寸元数据，apps/ui/src/tauriPlatform.ts 按 Rust 的 96 <= px <= 16384 边界发送嵌套 pageSize.widthPx/heightPx；没有打印宿主时保留无参兼容调用。该适配不改变来源 DOM、CSS 或可见布局，preimage 与前后 SHA 见 out/native-live/source44-frontend-preimage-manifest.json。

### 已落地功能的来源证据

下表是本次更新实际核对的固定来源文件集合；“适配”只表示 KeenCode 接线、文案或协议边界变化，不把适配代码归为来源代码。

| 功能范围 | 固定来源文件 | 目标实际状态 | SHA-256 结果 |
| --- | ---: | --- | --- |
| `resource-manager/` 资源管理器 | 10 | 10 个均保留；`ResourceManagerApp.tsx`、`resourceUsageView.ts`、`storageCategoryPresentation.ts`、`useStorageUsage.ts` 共 4 个适配，其余 6 个一致 | 6 一致、4 适配 |
| `store/mcpStore*` MCP 本地目录同步 | 5 | `mcpStore.ts`、`mcpStoreDesktop.ts`、`mcpStoreMigration.ts`、`mcpStoreHelpers.ts`、`mcpStoreStatusList.ts` 均保留；本地与远端统一走 Rust MCP 目录服务，缺失 bridge 改为显式错误，不再静默返回空列表 | 2 一致、3 适配 |
| 拖拽链路（workspace/file tree/workbench/group） | 9 个关键文件 | 全部保留，仍使用来源 DOM/拖拽规则 | 9 一致、0 适配 |
| 附件链路（composer/preview/telemetry/AI Elements） | 13 个关键文件 | 全部保留，含 `useWebElementContexts.ts`；RPC/lease 接线位于来源范围外的宿主适配层 | 13 一致、0 适配 |
| 工作流 SidePane 与 Sources CTA | 4 个来源文件 | 保留 SidePane 分区、工作区入口和 Sources 折叠/链接 DOM；其中 2 个文件接入 Journal/RPC 视图值 | 2 一致、2 适配 |

`v4/ConversationDraftEmptyState.tsx` 保留来源容器、尺寸、遮罩和渐隐语义；KeenCode 品牌适配以真实 `/logo.png` 做 SVG luminance mask，亮度阈值清除 PNG 内的深色方形底，仅以 `currentColor` 绘制 Keen 轮廓，未把 favicon 方块直接放入欢迎水印。`tooling/native-live/ui-settings-visual-plan.json` 覆盖 `zai-dark`、`zai-light`、`system`、en/zh 切换及设置导航后的草稿保留；工作流阶段的展开与 transcript 保留由 `zcode-desktop-plan.json` 覆盖。

云端反馈入口已按本地优先范围从产品调用图裁掉：帮助菜单的问题上报/产品建议、任务及分组菜单的“反馈问题”、会话订阅错误态的反馈按钮均不再渲染或订阅 `feedbackStore`；本地点踩/点赞、复制错误详情、重试和统一前端诊断仍保留。复制来源的 `feedbackStore`、反馈草稿 helper 以及未接入的 Feedback/client-config/help remote-config 实现已从目标前端源树裁掉；这些文件不构成兼容层。

General/Browser 设置的边界已按真实宿主能力收敛：`integratedTerminalShell` 经
`services.rs` 映射到 Rust `AppSettings.terminal_shell`，PTY 不再只读取旧的
`frontend-settings.json` 投影；HTTP 代理与 No Proxy 由 Rust 启动时消费，空值表示显式
清除。设置页已删除无 Tauri 消费方的 Browser Use 插件、证书策略和 Chrome 数据导入/清理
入口，默认插件集合与推荐语也不再插入该控制插件；内置浏览器的 toolbar、viewport/zoom
与受管 child WebView 仍属于 retained 范围。General 的 shell、HTTP Proxy、No Proxy、真实
PTY 和冷重启读回计划见 `tooling/native-live/general-settings-plan.json`；计划不含 endpoint
或凭据，当前来源集成 shell 选择器只暴露 Command Prompt/Git Bash，PowerShell 仅按真实
PTY 提示符和命令链路核验。

拖拽集合包括 `restrictVerticalDragWithinContainer.ts`、`taskWorkbenchDragPreview.ts`、
`workspaceFileDrag.ts`、`workspaceSidebarDrag.ts`、`usePromptEditorDragState.ts`、
`workbenchDragDrop.ts`、`workbenchPointerDragDrop.ts`、`group-drag-overlay.tsx` 和
`useWorkspaceFileTreeRowDragState.ts`。附件集合包括来源的
`ChatMediaAttachmentPreviewDialog.tsx`、`components/ai-elements/attachments.tsx`、
`lib/chatAttachments.ts`、composer upload/chip、transaction 和 telemetry 文件。

浏览器运行时源范围（`apps/ui/src` 与当前存在的全部 `packages/*/src`：`client`、`model-option-map`、`provider`、`rpc`、`services`、`shared`、`ui`）共 1646 个文件，其中 1591 个为文本文件。静态 `from`/`require` 与字面动态 `import()` 分别按 Electron、`node:*`、`fs/path/child_process/os/net/tls/worker_threads` 六类扫描，全部为 0；退役前端标识（具体名称与扫描口径见 [来源历史](source-history.md#native26-前端清理核验)）和 TS Agent 执行器也为 0。`packages/services/src/zcode-agent/zcodeAgent.ts` 是 Rust host 使用的纯 service descriptor/RPC 类型面，不启动 TypeScript Agent。源码中仍有少量兼容字段、性能指标名和协议注释包含 Electron 文本，但没有宿主 API 入口；这些文本与浏览器运行边界分开记录，不作为运行时残留计数。

## 依赖与产物扫描

- 当前 package manifests 没有直接 Electron 依赖；根开发依赖中的 `@types/node` 只服务构建、测试和类型检查。
- `pnpm-lock.yaml` 中的 `electron-to-chromium@1.5.394` 出现在 Browserslist 的转译目标元数据链路（3 行），不是应用运行时宿主；当前 lockfile SHA-256 为 `6DD089FFE1A7A5ECDE965BC76E77953240589516432260A7B1CA8591B29E6500`，该依赖重写属于前端替换阶段已有改动，本轮没有继续修改。
- 历史 Source44/48 前端产物、入口/CSS/worker 哈希和 Native44/48 绑定仍保留在各自机器报告中，不作为当前 Source49 产物结论。当前 Source49 的 `apps/ui/dist` 共 3632 个文件，树 SHA-256 为 `48D4C51238ADD775193CC94A73B2E058691BE341DAC778A117E90993495DDA43`；入口 `assets/index-LyxlK21f.js` 为 6,086,308 bytes，SHA-256 `0F7DE36CA54FF671D68E9A7C0403C163942440F4F74E79F96CFB0E10A05EF6C`；CSS `index-Dd_IpayY.css` 为 357,698 bytes，SHA-256 `6A1A77EBA67BDC057F40386A89080D87C881572EAC06913E4A102E909536FED7`；`diffs.worker-Dox-XN5I.js` 为 835,344 bytes，SHA-256 `AE0804500F327C259897D8246BE0C952B8D09C005273D4A580A336461598DF1C`；`index.html` 为 10,725 bytes，SHA-256 `843C2CB739A71F2126D138391C1085C619F7FA61F0E737592D1E0161675A4FC3`。产物扫描和第三方 guarded 兼容分支详见 `out/native-live/source-provenance-native49.json`。

凭据扫描见 `out/native-live/credential-scan-native42-prelive.json` 和
`out/native-live/credential-source-scan-source42.json`。本轮对 `out/` 证据逐文件做完整
`apiKey`/`baseUrl` 精确字节匹配，排除仅两个私有 provider 配置；5542 个文件扫描无匹配、
无扫描错误，BOM 实测为 UTF-8 1 个、无 BOM 5541 个、UTF-16/UTF-32 均为 0。产品源码与
文档扫描未命中私有值；历史 Native41 redaction manifest 及 post-redaction 结果保留并未
覆盖，历史验收目录中的发现单独记录。
- Vite 配置和测试夹具使用 `node:path`、`node:url`、`node:fs`，分类为构建/测试入口，不会进入浏览器业务 bundle。
- `packages/rpc` 原有的四个 `node --loader ts-node/esm` demo 和专用 `ts-node` 开发依赖已删除；四个文件作为裁剪项记录在 SOURCE-MAPPING，当前 package 不再提供 Node 示例入口。
- 当前目标树仍有 4 个退役来源标识命中，全部位于 `apps/desktop` 宿主历史/测试代码：工作树临时分支清理和终端环境测试；不在前端 source 或 dist 范围内。此前自动注入的 `apps/desktop/src/browser/annotation-guest.generated.js` 已删除，专用 annotation CDP/接线注册也已移除，不再把它归类为当前宿主残留。

源码和 bundle 中仍可看到少量 `Electron` 兼容类型、历史注释以及第三方库的环境检测字符串；它们不对应 Electron/Node 入口、进程启动或宿主实现。若验收标准要求删除所有兼容文本而非删除宿主运行时，该项仍需单独做协议清理，当前报告不把文本命中伪装为零。

本次来源适配的有意差异还包括：模型推理编辑器允许删除最后一个 chip，显式空的个人 `reasoning_efforts` 保持为 `[]` 而不回退目录默认；Windows Command Prompt 在 `terminal_inherit_system_profile=false` 时由真实 PTY 参数附加 `/d` 禁止 AutoRun；浏览器打开尺寸改用嵌套 `BrowserOpenBounds` DTO，并结合 owner/generation 校验拒绝旧 child WebView 的迟到回调。这些差异不改变来源 UI 的 DOM、CSS 或布局。

## 许可证与来源声明

- ZCode Apache-2.0 的 `LICENSE`、`NOTICE.md`、`SOURCE-MAPPING.md` 和设计基线已保留在 `third-party/zcode/`；当前 `SOURCE-MAPPING.md` SHA-256 为 `804ABA033E5F08D38A8ACDD54B7EED2DEFDCD969E60562B698B4A3131DDCC2B7`，设计基线包含 207 个规则文件条目，且仍与固定来源工作树逐文件匹配，SHA-256 为 `84743917D8853309EAE63D43411A5D19028FA37011AE305B65566BE0D7C083B9`。
- 固定来源的 `third-party/copied-components.json` 已原样复制到目标树；其中 35 个 AI Elements 派生文件全部存在，且每个文件头保留 Vercel、Apache-2.0 和本地修改说明，其余 11 个 `ai-elements` 文件不在该派生清单中。
- shadcn MIT、AI Elements Apache-2.0、VS Code IPC MIT、Superpowers MIT 四份适用上游许可证文本已原样复制到 `third-party/upstream/`，并与固定来源逐文件 SHA-256 校验一致。
- `packages/ui/src/components/ui/` 的 45 个文件不逐文件重复许可证头；其上游派生范围由 fixed manifest、上游 MIT 文本和 SOURCE-MAPPING 共同界定，目录中的 KeenCode/ZCode 文件不自动归属 shadcn。当前目标新增文件与 ResourceManager 挂载接线的归属边界见 SOURCE-MAPPING 的“当前目标新增与挂载接线”节。

## 当前验证

- Source49 当前机器报告与本节使用同一份来源统计：固定来源 HEAD `29628c9acdb81b703bbd4080c207a0e7ce5e276e`、来源工作树 clean、来源树 SHA `FD3FF4B80D5179293D56581C52F23B85CE7CD249C240F68F21F563176F48FDD0`、目标树 SHA `36A8C94A8159ACF87959C7BB2A1E19157CBD7E982018D095B46888E34F69D9F5`，计数为 `1149/163/238/9`。浏览器运行时静态扫描为 1646 个文件，退役前端、直接 Electron/Node 宿主/TSAgent 导入均为 0；产物扫描的第三方 PDF/DOCX 兼容文本仍按 guarded 分支单独记录。
- Source49 前端测试为 28 files/90 tests passed，脚本 74 passed/1 macOS skip，typecheck、Vite build 和 CSS 均通过；前端日志为 `out/frontend-tests-native49-final.log`、`out/frontend-build-native49-final.log` 和 `out/frontend-css-native49-final.log`。受影响 Rust tools/desktop 验证为 7 suites、1347 passed、9 ignored，strict feature Clippy 通过；这些数字不替代完整原生 live 验收。
- Native49 二进制构建已完成并绑定 SHA `DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，大小 110,075,392 bytes，元数据为 `out/native-live/native49-build.json`。第二轮 covered native scopes 25/25、2007 steps 全部通过；状态为 `passed_scoped_with_manual_gaps`，不能把 OS/manual 或 release installer 未测范围写成通过。
- Source49 退役前端扫描覆盖 5166 个文件，命中 0，路径存在性检查通过；三份最终机器报告均已绑定 Native49 second batch。第一轮 23/25、2 failures 仅作为历史 evidence 保留，不并入最终通过统计。

## 结论边界

Source49 当前源码与前端产物已证明没有退役来源标识、Electron API、Node 静态或字面动态宿主导入、TypeScript Agent 执行器；源码和产物中的 Electron/Node 兼容文本与第三方环境守卫不等同于运行时宿主实现。来源 SHA、目标替换备份、组件 fixed manifest、ResourceManager/MCP store/拖拽/附件复制、Worktree helper、Browser target-stability harness、Subagents 工具三态适配、Source49 机器报告和四份适用许可证均可复核。Native49 covered native scopes 第二轮已 25/25、2007 steps 通过，但 OS picker、系统托盘、原生通知、OS 坐标拖拽和 release installer 仍是明确未完成或需人工的边界；Source48/45/44 的 PDF、DOCX、像素和原生结果保留为历史 artifact。
