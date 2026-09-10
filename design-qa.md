# 2026-09-10 子 Agent 默认不限轮数

- 基线：`6a7db3ef56ff1ead797df396194dfea6c3333612` 的前端源码；`output/playwright/agent-limits-20260910/before-source.zip`，SHA-256 `a56477191369e9147d0504540e4921e7fdaa10488d07c5d470ac777f42f92ea8`。此前未提交改动仅涉及 Rust，不影响该前端基线。
- 仅把创建表单初始化、取消后重置、保存后重置的 `maxTurns` 从 `"20"` 改为 `""`；沿用现有空值提交 `null` 的语义，无布局、样式、组件或依赖修改。
- 浏览器验证：macOS Chromium、1280×820、deviceScaleFactor=1、中文、浅色，直接复用真实 `AgentsPanel` 和样式，以合成 Tauri 调用桩隔离用户数据。夹具为同目录 `form.html` / `form.tsx`，运行 `pnpm run dev` 后访问 `http://127.0.0.1:1421/output/playwright/agent-limits-20260910/form.html`；打开创建弹窗比较 `before.png` / `after.png`。基线可从压缩包在隔离副本重建并复用同一夹具。
- 像素结果：RGB 任一通道差 >16 的像素为 90 个，占 0.008575%；未掩码，全部差异位于 `(435,597)–(451,609)` 的旧默认值 `20` 文本区域。差异图 `diff.png`。最大轮数输入框为 x=425、y=586.125、430×32，其他像素完全一致。
- 真实表单交互确认：默认留空提交 `maxTurns:null`；显式输入 3 提交 `maxTurns:3`；保存后重开为空，输入 7 后取消并重开也为空。未在用户数据目录创建 Agent。`pnpm run typecheck` 与 AgentsPanel 的 21 项 Vitest 通过。
- 原生桌面验收未完成：当前原生 UI 控制 API 禁用，没有启动或重启桌面应用，也未进行真实模型长任务或 Windows 实机验收。浏览器截图不作为原生验收证据。

---

# 2026-09-07 macOS 导航留白与图标对齐

- 修改前源码快照：`output/design-qa/sidebar-spacing-20260907/before-source.zip`，包含当时工作区 src/public。
- macOS 顶部行恢复 40px，行后保留 8px，导航起点由 74px 提前至 48px；收起按钮中心仍为 20px。四个导航项使用左右 6px 外部留白、7px 内边距及 18px 图标，图标中心 x=22px，与原生关闭按钮配置 x=16px 加半径 6px 对齐。Windows 不变。
- 验证：`pnpm run lint:css` 通过。`pnpm run typecheck` 未通过：无关的 `src/lib/extensionsUi.test.ts:417–426` 测试数据缺少 `AvailablePluginDto.installed` 字段，本次未修改该模块。顶部规则的平台选择器优先级高于短窗口规则，短窗口也保持 40px 标题栏。
- 原生验收未完成：当前原生 UI 控制 API 禁用，无法取得同状态前后截图和像素差。源码快照可在隔离副本重建；分别运行 `pnpm dev:desktop`，保持 macOS、相同视口/deviceScaleFactor、浅色与侧栏展开状态，比较导航起点与四个图标中心，检查悬停、点击、收起/展开和窗口拖动。

---

# 2026-09-07 新建对话统一导航样式

- 修改前源码快照：`output/design-qa/sidebar-new-session-20260907/before-source.zip`（当前工作区 src/public，包含上一项标题栏修复）。
- 新建对话直接复用搜索、技能的 `Button` 与 `nav-new` 样式，删除不再使用的 `nav-new--primary` 两组规则；点击行为不变。有意差异为取消边框、底色、居中和加粗，高度由 38px 统一为 36px，下方导航相应上移 2px。
- `pnpm run typecheck`、`pnpm run lint:css` 通过。
- 原生计算机控制 API 禁用，未完成原生前后截图及像素差验证。已保存可在隔离副本重建的源码；复现时分别运行 `pnpm dev:desktop`，保持 macOS、相同视口和 deviceScaleFactor、浅色及侧栏展开状态，检查四个导航项对齐、悬停和点击。

---

# 2026-09-07 macOS 侧栏收起按钮对齐修复

- 基线：`841f3d2d9948c30a70ef00554a5ccd8c64aa092c` 加工作区已有修改；修改前 `src/`、`public/` 快照：`output/design-qa/sidebar-alignment-20260907/before-source.zip`。
- 仅 macOS 的 `.sidebar-chrome` 改为顶部对齐并移除顶部内边距。按钮中心从 36px 恢复到原标题栏的 20px（顶部外边距 6px + 按钮高度 28px / 2）；行高、外边距及下方导航位置不变，Windows 样式不变。短窗口下也保持同一按钮位置。
- 验证：`pnpm run lint:css`、`pnpm run typecheck` 通过。核实 Tooltip 不添加布局包装，按钮沿用 28px 高度。
- 原生验收缺口：当前会话原生计算机控制 API 禁用，无法重建 macOS 原生窗口并拍摄同状态前后截图，尚未完成原生像素差异验收。
- 复现：将快照解压到隔离副本并复用依赖，分别运行 `pnpm dev:desktop`；同为 macOS、1280×820、相同 deviceScaleFactor、浅色、展开侧栏，比较收起按钮与红黄绿按钮中心，确认下方导航位置不变，并检查收起/展开及窗口拖动。

---

# 2026-09-07 首页品牌与思考强度入口精简

- 基线提交：`41c7de57b4efe22b45b87e567df30c93d6e7ebb3`；修改前 `src/`、`public/` 快照：`output/design-qa/welcome-cleanup-20260907/before-source.zip`。
- 有意差异：移除左上角闪电和 KeenCode 文字，保留品牌容器的盒模型与窗口拖动区域；移除欢迎标题前闪电；未选中有效模型时，整个思考强度菜单不渲染。选中模型后的行为保持原样。
- 本次未修改 CSS。组件回归检查覆盖无模型时菜单关闭和打开两种状态，以及选中模型后显示当前强度。
- 原生验收缺口：本会话的原生计算机控制 API 已禁用，不能重建相同原生窗口状态并取得前后截图。源码基线已保存，但没有完成本次原生像素差异检查，不以历史截图替代。
- 待复现：在该源码快照及修改版分别运行 `pnpm dev:desktop`，保持相同视口、deviceScaleFactor、中文、无项目、无模型及空草稿状态，截图比较上述三个区域，并确认标题栏拖动和侧栏收起仍正常。

---

# 2026-09-07 Harness TPS 与用量明细调整

## 源码核实与实现

- 参考源码仍为 DeepSeek Harness `d347e703908d0406b7a7ef80e3a0e594d86b2215`。`packages/client/ui-chat/src/client/contract/turn-metrics.ts:42-50,74-95` 明确按请求求 `decodeMs = max(0, completedTime - firstTokenTime)`，再计算 `sum(outputTokens) / (sum(decodeMs)/1000)`。只累计同时有输出量与计时的请求，单项零毫秒仍参与累计，仅总分母为零时不产生 TPS。
- `conversation-nodes/event-projection.ts:169-179` 将非空正文、推理文本、工具名或非空参数视为首输出；`conversation-nodes/assistant.ts:194-206` 从事件时间保存起止点。`chat/message-chrome.ts:67-70` 规定 >=10 TPS 取整，小于10最多一位小数；`TurnUsagePanel.tsx:214-222` 将速度放在总用时下面。
- KeenCode 在实际 SSE 事件流首段输出至 MessageEnd 之间使用本机单调时钟计时，发出一次 DecodeTiming 并保存到 ResponseMetadata，再随 ModelRoundCompleted 进入 Journal。实时与冷回放使用同一 model_usage_reported 映射，将配对的输出量和 decodeDurationMs 交给前端聚合；不使用整轮耗时，不混入首 Token 等待或工具执行时间。HTTP 返回 JSON 时即使请求指定 stream，也不把解析时间当成解码速度。
- 用时明细顺序：总用时、输出速度（TPS）、首 Token 延迟。无记录时明确显示未报告；历史数据未采集输出计时，不能补造历史 TPS。用量弹层删除缓存写入 Token；底层仍保留供应商计数以维持准确的总用量口径。所有轮次继续采用上一轮确认的 hover/focus 显示规则。

## 验证与基线

- 修改前源码快照 `output/design-qa/turn-tps-20260907/before-source.zip`，SHA-256 `70241F4FBB4E24651A283DBC4E2776170EABCE99E77E7DBACE3EF8584ED38483`。该快照为 main f19227f 加此前未提交的用量/用时及悬浮修改。
- 同目录 before/after.html、before-fixture/fixture.tsx 在同一原生 1280×820 页面、浅色、相同合成两轮对话下渲染真实 ConversationThread。基线 src 的 @/ 仅重写至解压副本 URL。示例经生产 reducer：2300 output / 11.92s = 192.95…，界面按 Harness 规则显示 **193 tokens/s**。
- 运行 pnpm.cmd run dev，设置 KEENCODE_BENCHMARK=1、KEENCODE_BENCHMARK_DATA_DIR=<产物目录>/data、WEBVIEW2_USER_DATA_FOLDER=<产物目录>/webview，执行 pnpm.cmd exec tauri dev --no-watch --config <产物目录>/native.json，通过页面底部测试链接切换版本并打开用时面板。
- 原生截图 native-before-time.jpg / native-after-time.jpg 均为1282×822，未调整 DPI 或缩放。RGB任意通道差值>16的像素占比0.2732%，未掩码，变化为新增 TPS 行引起面板向上增高和文字位置调整，包含鼠标差异。另存 native-after-usage.jpg，确认无缓存写入项。并排图、差异图及统计保存在同目录。
- 35项Node测试、源码门禁及131文件/1216项Vitest通过；typecheck/build通过，保留已有chunk尺寸及混合动态导入提示，lint:css与diff检查通过。
- Rust通过：ACP63、Agent291、Model37、Provider110、持久模型轮次13、桌面实时/回放映射1，共515项。另定向复测本地真实HTTP的SSE计时与缓冲JSON不计时。测试覆盖缺失字段、零分母、重复样本、配对聚合、计时保存与冷回放。
- 验收使用合成数据及本机模拟HTTP服务，未调用真实模型消耗额度。未验收macOS/Linux，未进行新的内存或启动基准测量；无新增依赖、后台任务或定时轮询。

---

# 2026-09-07 统计栏统一悬浮显示

- 按用户补充要求取消最新轮常显，删除 actionsVisible 属性和对应 CSS 例外。复制、用量、用时统一在对应消息 hover/focus 时显示，移出后隐藏；保留隐藏占位防止排版跳动及无 hover 设备的可访问性降级。
- 基线为上一次实现的未提交源码，快照 `output/design-qa/turn-metrics-hover-20260907/before-source.zip`。复用上次两轮合成对话 fixture，浏览器同一 1280×720、DPR 1、浅色状态，保存 before.png、after.png、hover.png、diff.png。RGB 任一通道差值 >16 的像素占比 0.1110%，有意差异是最后一轮操作栏隐藏。
- 实测四条消息的操作栏默认 opacity 全为 0；悬浮最后一条回复后仅该条为 1；移到空白区后全部恢复 0。
- typecheck、28 项相关组件测试、lint:css 通过。本次仅调整前端显隐条件，未重复上轮原生 WebView 验收，不将本次浏览器截图作为新的原生验收证据；开发版通过 HMR 应用修改。

---

# 2026-09-07 每轮用量与用时

## 源码与统计口径

- 同步 main：`cafcf1c` 快进至 `f19227f`。参考 DeepSeek Harness `d347e703908d0406b7a7ef80e3a0e594d86b2215` 的 `TurnUsagePanel.tsx`、`TurnUsagePanel.module.css`、`TurnTailNodeView.tsx`、`MessageIconActions.module.css` 和 `token-format.ts`，直接适配源码参数。
- 每轮最终 Assistant footer 提供两个独立入口。最新轮常显，历史轮 hover/focus 显示；无 hover 设备常显。28px 高、28px 圆角、13px/24px 字体、15px 图标、6px 8px padding；弹层 16px padding、12px 圆角、300–440px 宽，窗口边距 12px。
- 后端从持久化 ModelRoundCompleted 投递独立 model_usage_reported 事件，前端按根 Turn 聚合、按 observationId 去重。实时和冷回放走同一链路；总用时使用 Journal 事件时间，本机首 Token 观测独立补写。未知字段不冒充零，缓存/推理不重复计入总量。
- Messages 的原始 input_tokens 不含缓存，Adapter 统一成含缓存的输入总量；Chat Completions / Responses 保留既有输入口径。总量优先远端 total，其次完整 input + output；未提供 decode 时间，不显示伪造 TPS。

## 基线与复现

- 精确基线：`output/design-qa/turn-metrics-20260907/before-source.zip`，由 `git archive f19227f src public` 创建，SHA-256 `0BE636FCE66F7B5E75D2E8F9740FDF015F4CA933A89B7DF91CE87898BF031400`。
- 同目录 `before.html` / `after.html`、`before-fixture.tsx` / `fixture.tsx` 渲染相同两轮合成数据，直接导入各版本 ConversationThread。基线 src 的 `@/` 仅重写到解压副本绝对 URL，防止串入修改后的组件。两页都固定浅色，并用 bg-app 为透明原生 WebView 提供背景。
- 运行 `pnpm.cmd run dev`，然后设置 KEENCODE_BENCHMARK=1、KEENCODE_BENCHMARK_DATA_DIR 为该目录的 data、WEBVIEW2_USER_DATA_FOLDER 为 webview，执行 `pnpm.cmd exec tauri dev --no-watch --config D:\projects\keen-code\output\design-qa\turn-metrics-20260907\native.json`。使用页面右下角测试链接切换版本，不写入真实用户会话。
- Windows 原生同一窗口截图 1282×822（页面 1280×820），未改变 DPI/缩放；`native-before.jpg`、`native-after.jpg`、`native-usage.jpg`、`native-time.jpg`。像素差 RGB 任意通道 >16 的占比 **0.315%**，未掩码，包含鼠标指针/高亮差异；变化集中于最新轮统计栏。并排图 `native-comparison.png`，差异图 `native-diff.png`，数据 `pixel-differences.json`。该数值不作为错误率。

## 验证结果及边界

- 完整前端：35 项 Node 测试、源码门禁、131 文件 / 1213 项 Vitest 全部通过。新拉取的 release 测试补充 CRLF 归一化；Windows 验证在 PATH 前加入本机 `D:\app\Git\bin` 使用 Git Bash，未修改系统 PATH。
- typecheck、lint:css、前端 build 和 diff 检查通过。build 保留已有大 chunk 与混合动态导入警告。
- Rust：keencode-acp 63 项、desktop agent_runtime 96 项、keencode-provider 108 项通过；新增事件 serde、整数边界、回放一致性、多请求累加、缺失字段及 Messages 缓存统计均覆盖。
- 浏览器核对 28px/13px/24px/28px 计算样式；最新/历史操作栏透明度符合预期。两个弹层、Esc 关闭与触发器焦点恢复通过。680×620、DPR 1 下弹层宽 300px、padding 16px，无横向溢出，临时视口已恢复。
- 原生实际点击用量、用时入口并核实明细。界面验收使用合成数据；真实 Journal 的实时/回放由测试覆盖，未消耗真实供应商额度进行联网推理，未验收 macOS/Linux。未重新测量内存/启动性能；新增唯一直接依赖 @radix-ui/react-popover 复用现有 Radix 基础依赖，不增加后台轮询。

---

# 2026-09-07 全窗口设置的返回入口

## 调整与基线

- 全窗口设置按页面导航表达返回行为：移除正文标题右侧的关闭图标，左侧原“设置”标题位置改为“← 返回应用”。复用现有 shadcn Button、返回图标、翻译、样式及 `onBack`，不增加依赖或新导航状态。
- 返回按钮位于导航滚动区外，目录滚动到底部时仍可见；保留原有导航条目、正文布局及字体间距。680px 及以下隐藏桌面返回入口，使用下拉导航左侧的既有返回按钮，始终只显示一个页面返回入口。
- 基线为 `4d408cb8053e0fead068a6b8097770939835fdd6` 加此前未提交修改；修改前完整 `src/`、`public/` 快照：`output/design-qa/settings-return-20260907/before-source.zip`，SHA-256：`C8CB37478FEC95010F2866740DF3F27594DFCD0AFC0E311896D5FEFC701E9E11`。
- 原生前后截图为同一 Windows 开发进程、同一普通窗口、常规设置、浅色与薄雾皮肤、导航和正文滚动顶部，均为 `1282 × 822`。未改变系统 DPI 或 WebView 缩放。鼠标位置、悬停及滚动条显隐差异未掩码。

## 本次验证

- `pnpm.cmd run typecheck`、`pnpm.cmd run lint:css`、`pnpm.cmd exec vitest run src/components/SettingsPage.test.ts` 通过，共 9 项设置页测试。`git diff --check` 通过。
- Windows 原生实际滚动导航并点击“返回应用”，确认回到工作台且窗口继续运行；随后重新打开设置，停留在调整后的常规页。
- 浏览器 `680 × 620`、DPR 1：桌面返回入口隐藏，窄窗口返回按钮位于 `(16, 48)`，无正文关闭图标、无横向溢出；点击返回工作台通过，临时视口已恢复。
- 同尺寸原生前后截图的 RGB 任意通道差值大于 16 的像素占比为 **1.440%**。已检查并排对照，变化集中于返回入口及鼠标、滚动条状态，不将差异比例视为错误率。
- 产物目录：`output/design-qa/settings-return-20260907/`，包含 `native-settings-before.jpg`、`native-settings-after.jpg`、`native-settings-nav-scrolled.jpg`、`native-settings-comparison.png`、`native-settings-diff.png`、`pixel-differences.json` 与 `browser-settings-680.png`。
- 复现：运行 `pnpm.cmd run dev:desktop` 并进入常规设置，保持相同窗口尺寸、主题、皮肤与滚动位置截图；运行 `python output/design-qa/settings-return-20260907/compare_screenshots.py` 重算像素差。本次未修改后端，未重复模型服务或跨平台验收。

---

# 2026-09-07 设置页全窗口显示与自建 HTTP Provider

## 本次变更与基线

- 用户要求设置页全屏显示。本次将其解释为占满应用窗口：取消居中的 800px 面板、外部留白、外圆角和阴影，保留 Harness 字体与控件令牌、188px 设置导航、960px 正文最大阅读宽度。
- 顶部 40px 保留为窗口拖动区，导航与正文从 54px 开始，返回按钮进入正文标题行。不改变操作系统的全屏状态。
- 基线提交：`4d408cb8053e0fead068a6b8097770939835fdd6` 加本会话前一阶段尚未提交的 Harness 主题改写。修改前完整 `src/`、`public/` 快照为 `output/design-qa/settings-fullscreen-20260907/before-source.zip`，SHA-256：`FC24ADF2FE0AF6D55E0AEE961BCE955F3B12BE9100F34E18F71ACF1433302041`。
- 原生前后状态相同：Windows、普通窗口、常规设置、浅色、两个滚动区域均在顶部；窗口截图均为 `1282 × 822`。本次未改变系统 DPI 或 WebView 缩放，像素比较不缩放图片。鼠标位置及系统窗口边缘的微小变化未做掩码处理。

## 验证结果

- `pnpm.cmd run typecheck`、`pnpm.cmd run lint:css` 通过；`SettingsPage.test.ts` 与 `layout.test.ts` 共 18 项测试通过。
- `cargo test --manifest-path Cargo.toml -p keencode-provider`：108 项通过。覆盖三种模型协议的显式 HTTP/HTTPS、远程域名、内网 IPv4、IPv6、回环及无认证配置，并保留非法 URL、端点越界、凭据与 Header 边界验证。
- 开发版重新编译并启动成功；日志包含 `runtime_ready` 与 `frontend_interactive`。原生模型设置可见三家供应商，恢复的 HTTP 供应商记录与原备份逐字段一致。
- 原生确认设置入口、普通窗口与最大化窗口布局，以及标题栏双击最大化 / 还原；窗口控制按钮与正文返回按钮分离。
- 浏览器在 `1280 × 820`、DPR 1 时，设置页与容器均为 `(0, 0, 1280, 820)`，外圆角 `0px`、阴影 `none`；字体栈仍为 Harness 原栈。
- 浏览器在 `680 × 620`、DPR 1 时，设置页为 `(0, 0, 680, 620)`，下拉导航位于 40px 标题栏下。常规与外观分区均无横向溢出，下拉切换与返回应用通过；已恢复浏览器视口覆盖设置。
- 浏览器捕获的 error / warn 列表为空。`git diff --check`、修改 Rust 文件的 `rustfmt --check` 通过。

## 产物与像素对照

目录：`output/design-qa/settings-fullscreen-20260907/`。

- `native-settings-before.jpg` / `native-settings-after.jpg`：本次真实原生窗口的常规设置前后截图。
- `native-settings-diff.png` / `native-settings-comparison.png`：像素差及并排对照。RGB 任一通道差值大于 16 的像素占比为 **50.558%**，表示居中面板改为全窗口的有意布局变化，不是错误率。
- `native-settings-maximized.jpg`：最大化后的全窗口设置。
- `browser-settings-1280.png`、`browser-settings-680.png`、`browser-appearance-680.png`、`browser-settings-680.json`：固定视口截图与布局数据。

复现：在该源码状态执行 `pnpm.cmd run dev:desktop`，进入常规设置并保持顶部滚动位置，以相同窗口尺寸截图；执行 `python output/design-qa/settings-fullscreen-20260907/compare_screenshots.py` 重算像素差。浏览器使用同一开发服务和 `#/settings/general`，分别设置上述固定视口检查布局。

## HTTP 行为与验证边界

原有 Provider 核心将非回环 HTTP 全部拒绝，导致无 TLS 的自建代理阻止应用启动。现在按用户配置的 HTTP/HTTPS 协议原样连接，不自动升级或降级；HTTPS 继续使用客户端的默认证书校验。HTTP 不提供传输加密。

本次没有向真实模型服务发送推理请求，没有验证该代理实际响应、macOS / Linux 原生窗口或安装包性能预算。开发编译仍出现已有的 PDB 输出重名与链接器提示；未为本次修改扩大打包或构建配置范围。

---

# 2026-09-07 Harness 源码主题改写验收

本次以参考前端源码的 CSS 声明为实现依据；截图用于检查最终渲染和记录有意变化。源码映射、尺寸表和适配边界见 [Harness 主题说明](docs/frontend-harness-theme.md)。

## 固定版本与环境

- KeenCode 修改前提交：`4d408cb8053e0fead068a6b8097770939835fdd6`；开始时工作树干净。
- DeepSeek Harness 固定源码：`d347e703908d0406b7a7ef80e3a0e594d86b2215`。三份主题 CSS 通过 `scripts/sync-harness-theme.mjs` 从该提交读取。
- 源码快照：`output/design-qa/harness-20260907/baseline-source.zip`，包含修改前的 `src/` 和 `public/`；SHA-256：`7D38AB188E3FCD702F3F479615BD72FC82F07155E9B633A9D6137C3C4CB14147`。
- 环境：Windows，项目 Vite 开发服务 `http://127.0.0.1:1421/`，Codex in-app Chromium；浏览器主要截图均为 `1440 × 1000 CSS px`、`deviceScaleFactor = 1`，截图尺寸也为 `1440 × 1000 px`。
- 同状态：中文、无项目、无模型、无消息、空草稿；首页侧栏使用相同的已保存 260px 宽度，设置使用常规页并保持滚动位置在顶部。
- 辅助参考实例：隔离目录运行 `@deepseek-ai/dsh@0.1.2-rc.1 web --host 127.0.0.1 --port 3087 --no-open`，没有配置模型密钥。npm 包与固定 Git 提交未验证为同一构建，不能用它替代源码参数核对。
- 原生实例：正在运行的 `src-tauri/target/debug/keencode-desktop.exe`，通过 computer-use 实际点击、切换主题、打开和关闭菜单。原生截图为窗口捕获，尺寸 `1282 × 822 px`，不与浏览器截图混合计算差异。

## 检查结果

- `pnpm.cmd run typecheck`、`pnpm.cmd run lint:css`、`pnpm.cmd test`、`pnpm.cmd run build` 通过；Vitest 为 131 个文件、1205 项测试。完整测试命令中的 Node 脚本测试及源码边界检查也通过。
- 旧正文 15px 的样式断言更新为参考源码的 14px，并保留有序 / 无序列表可见标记的验证。
- 构建仍报告部分 chunk 大于 500kB；本次未进行打包拆分或宣称安装包性能预算通过。
- 浏览器核对了完整字体栈、正文 14px/24px、标题 26px/32px、输入圆角 22px、输入宽度公式、34px 发送按钮、800px 设置面板、188px 设置导航，以及 ToggleGroup 外观选项的 180px flex 基准 / 20px 与 32px 内边距。
- 浏览器逐项打开常规、外观、模型、个性化、技能、插件、子智能体、插件市场、MCP、归档设置、已归档对话、请求记录、用量统计、关于。模型编辑表单也检查了布局；未填写密钥或保存提供商。
- 中文长草稿达到 336px 高度后在输入区内滚动，与欢迎标题不重叠。输入命令菜单的方向键选择和 Esc 关闭通过；侧栏可折叠并恢复，补齐了恢复按钮的无障碍名称。
- 在实际 `680 × 620` 最小窗口视口检查首页及下拉式设置导航，在 `900 × 700` 检查模型编辑区；内容无横向溢出。DOM 的 `innerWidth` / `innerHeight` / `devicePixelRatio` 用于确认视口已生效；窄窗口截图使用同一浏览器的 CDP 截图接口。
- 已捕获的浏览器 console error / warn 列表为空，记录在 `browser-logs.json`。
- 原生确认首页、设置导航、深浅色切换、命令菜单打开与 Esc 关闭。结束时恢复原来的“跟随系统”主题偏好和首页。

## 本次截图与像素差

所有产物位于 `output/design-qa/harness-20260907/`，该目录是本地验收产物，不作为源码依赖。

| 状态 | 修改前 / 修改后 | RGB 任意通道差值大于 16 的像素占比 |
| --- | --- | --- |
| 首页浅色 | `before-home-light.png` / `after-home-light.png` | 2.445% |
| 首页深色 | `before-home-dark.png` / `after-home-dark.png` | 8.076% |
| 常规设置浅色 | `before-settings-light.png` / `after-settings-light.png` | 60.985% |
| 常规设置深色 | `before-settings-dark.png` / `after-settings-dark.png` | 46.736% |

对应的 `diff-*.png`、`comparison-*.png` 和 `pixel-differences.json` 已生成并检查。百分比表示这次主题改写相对旧 KeenCode 的变化量，不是与 Harness 的相似度或错误率。设置由全屏分区改为居中面板，面积变化是本次授权的有意差异。

补充产物：`after-appearance-*.png`、`after-long-draft.png`、`after-composer-menu-light.png`、`after-create-project-light.png`、各设置分区 `after-*-light.png`、`after-home-minimum-dark.png`、`after-settings-minimum-dark.png`、`after-model-form-900-dark.png`、`computed-*.json`、`route-audit.json`，以及 `native-home-*.png`、`native-appearance-*.png`、`native-settings-light.png`、`native-composer-menu-dark.png`。

## 重建与验证方式

```powershell
# 当前工作树：检查并启动前端（已有服务时直接复用）
pnpm.cmd run typecheck
pnpm.cmd run lint:css
pnpm.cmd test
pnpm.cmd run build
pnpm.cmd run dev -- --host 127.0.0.1 --port 1421

# 从固定提交恢复旧 src/public 到独立目录，不覆盖当前工作树
git archive --format=zip --output=<基线目录>/baseline-source.zip 4d408cb8053e0fead068a6b8097770939835fdd6 src public

# 重新比较本地已有的同尺寸截图
python output/design-qa/harness-20260907/compare_screenshots.py
```

浏览器按上面的语言、数据状态、侧栏宽度和滚动位置打开首页 / 常规设置；分别切换浅色、深色，使用 `1440 × 1000`、DPR 1 捕获。原生使用现有开发窗口，并在每次动作后重新读取控件树和截图。

## 验证边界

本次原生验收只有修改后的真实交互截图。修改前的原生截图缺失，保留并验证了 Git 源码快照和浏览器基线，但没有重建另一份旧版原生程序；因此没有声称完成原生前后像素对照或发布验收。

未验证真实模型调用、已有大数据会话、运行中终端、全量文件预览类型，以及 macOS / Linux 原生窗口。Rust / ACP 未修改；没有用前端检查替代 Rust 测试或全部业务端到端测试。安装包体积、启动速度、CPU、内存本轮未重新测量。

---

# 历史 Design QA 记录

> 以下旧记录未复验。旧记录引用的 25 个 `output/` 截图和像素差文件未纳入当前工作树，以下“通过”仅记录历史检查结论，不能作为本次原生桌面或固定视口像素验收证据。本次证据见本文顶部 2026-09-07 记录。

# 添加项目面板 Design QA

## 对比目标

- 输入设计稿：原始输入未纳入仓库。
- 最终实现截图：`output/design-qa/add-project-550-after.png`
- 并排对比：`output/design-qa/add-project-comparison.png`
- 状态：浅色界面；创建项目面板已打开；名称与源文件夹为空；名称输入框聚焦；创建按钮禁用。

## 视口与密度归一化

- 源图：1099 × 645 px，来自 macOS Retina 截图；按 2 倍密度归一化为 550 × 323 px。
- 实现：550 × 323 CSS px，`deviceScaleFactor = 1`，截图为 550 × 323 px。
- 对比时使用归一化源图 `output/design-qa/reference-550.png`，避免把 Retina 物理像素误判为两倍尺寸的 CSS 面板。

## 可见结果

- 信息层级与参考一致：标题、项目名称、源文件夹、文件夹选择/拖入区域、取消、创建项目。
- 面板、输入框、拖入区域和底部操作区在归一化视口中的位置与比例一致，没有裁切、溢出或控件遮挡。
- 项目名称默认获得焦点，焦点边界可见；创建按钮在源文件夹为空时保持禁用。
- 图标使用项目现有 Tabler 图标；没有用 CSS 图形、文本符号或占位素材代替。
- 品牌文案统一为 `KeenCode`，并保留 KeenCode 的玻璃表面、圆角、颜色与按钮令牌。

完整面板在 550 × 323 对比图中已能清楚判断字体、间距、颜色、图标和文案，因此不需要额外局部裁切。

## 交互与运行检查

- 侧栏“添加项目”、搜索面板“添加项目”、对话项目菜单“添加项目”均打开同一面板。
- Escape 可关闭面板；每次打开后名称输入框重新获得焦点，关闭后焦点归还侧栏、搜索或对话项目菜单的稳定入口。
- 左侧栏折叠后通过 `Cmd/Ctrl+K` 打开搜索再进入添加面板，关闭后焦点回到可见的对话输入框，不会落到隐藏侧栏。
- 项目可拖到目标行前后排序，也可用 `Alt+↑/↓` 排序；键盘排序结果通过现有状态提示区播报。
- 创建进入忙碌态时，输入框改为只读、提交按钮保留焦点并暴露 `aria-disabled`，焦点不会逃出面板。
- Web/Vite 环境点击源文件夹会显示“需要在 Tauri 窗口中选择目录”，不会误提交；创建按钮仍禁用。
- 控制台 error/warn：0。
- 原生目录选择、Tauri 系统文件夹拖入、项目排序重启持久化未操作原生窗口，属于桌面验收边界。

## 对比历史

1. 初次实现：`output/design-qa/add-project-550-before.png`。发现模态框焦点被关闭按钮抢走，关闭按钮出现不符合参考的焦点圈；空状态还多了一行提示文本。
2. 修正：`GlassModal` 使用显式初始焦点标记聚焦项目名称，并在关闭后把焦点归还入口；输入框增加可见焦点边界，同时移除空状态的重复提示行。
3. 复验：`output/design-qa/add-project-550-after.png`。上述差异已消除；随后补齐键盘排序播报和忙碌态焦点保持，没有剩余 P0、P1、P2 问题。

## 剩余 P3

- KeenCode 现有皮肤比参考图偏粉、标题字重略轻；这是复用当前产品令牌的有意差异，不阻塞交付。

historical result: passed; current release: not reverified

---

# 后台 Shell 摘要面板 Design QA

## 对比目标

- 默认态输入设计稿：原始输入未纳入仓库。
- 实现截图：`output/playwright/summary-shell-rest.png`（未悬浮）、`output/playwright/summary-shell-hover.png`（悬浮）。
- 同屏对比：`output/playwright/summary-shell-comparison.png`。
- 状态：浅色界面；摘要已打开；当前会话有一个名为 `pnpm dev:desktop` 的后台 Shell。

## 视口与密度归一化

- 浏览器视口：900 × 700 CSS px，`deviceScaleFactor = 1`。
- 源图：623 × 172 px；实现聚焦区：298 × 77 px。
- 同屏对比把源图归一化为 298 × 82 px，并将实现留白到相同尺寸后并排；没有把源图密度误判为组件尺寸。

## Findings

- 没有剩余 P0、P1、P2 问题。栏目位于现有摘要面板内部，没有创建第二个浮层；无后台 Shell 时整个栏目隐藏。
- 字体与文案：沿用 KeenCode 当前字体、字重与密度；标题为“后台进程”，命令摘要保持原文并在窄面板中单行截断。
- 间距与布局：未悬浮时只显示左侧终端图标和命令摘要；悬浮后才出现右侧停止控件，没有水平溢出。
- 颜色与令牌：命令行默认透明；悬浮或键盘焦点进入时才显示弱底色与阴影，全部复用现有语义令牌。
- 图像与图标：本组件不需要位图；终端与实心停止图标均来自项目现有 Tabler 图标库。
- 内容与交互：仅展示当前根 Session 的后台 Shell；点击停止后调用既有 `background_task_cancel`，刷新后栏目消失。

## 对比历史

1. 初次实现使用现有描边停止图标，与参考图中的实心方块存在明显差异。
2. 通过共享图标模块改为 Tabler 实心停止图标。
3. 修正后同屏对比中，标题、灰色圆角行、终端图标、命令与实心停止控件的层级一致。
4. 用户补充默认态后，移除命令行常驻表面和停止按钮；灰色表面、阴影及停止按钮只在悬浮或键盘聚焦时出现，触屏设备保留可操作的停止入口。

## 运行检查

- Playwright 实测未悬浮截图中没有表面、阴影或停止图标，悬浮命令行后这三项同时出现。
- 通过浏览器无障碍树实际点击“停止后台进程”，模拟取消接口完成后栏目消失。
- 组件交互无错误；控制台仅有本机端口 1422 已被占用导致的开发态 Vite HMR 连接错误，不影响组件渲染或操作。

historical result: passed; current release: not reverified

---

# 新建对话欢迎区 Design QA

## 对比目标

- 输入设计稿：下半部分的新建对话状态；原始输入未纳入仓库。
- 实现全景：`output/design-qa/new-chat-welcome-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-welcome-comparison.jpg`。
- 状态：浅色 Dream 皮肤；新建对话；无项目、无可用模型；输入为空；发送按钮禁用。

## 视口与归一化

- 源图：2071 × 1331 px；本次目标仅采用下半部分的“欢迎语 + 项目选择 + 输入区”结构。
- 实现：1280 × 720 CSS px；浏览器报告 `devicePixelRatio = 2`，截图输出已归一化为 1280 × 720 px。
- 聚焦对比将源图 `(180, 700)–(1680, 1331)` 与实现 `(140, 150)–(1140, 510)` 分别等比缩放并留白到 1280 × 600 px，再左右并排；没有把设计稿的其他区域误作同一视口。

## Findings

- 没有剩余 P0、P1、P2 问题。新建对话现在使用居中欢迎语，项目选择与 Composer 位于同一外层卡片，层级和垂直节奏与参考一致。
- 字体与文案：欢迎语使用当前产品字体与字重，并采用 KeenCode 自有任务导向文案；按时段问候不在功能要求内。
- 间距与布局：外层卡片实测 864 × 164 px；项目栏 40 px、Composer 850 × 110 px；欢迎语底部到卡片顶部约 50 px。现有对话仍走底部浮动 Composer，不使用欢迎卡片样式。
- 颜色与令牌：边框、表面、阴影和圆角全部复用 `--border-subtle`、`--bg-elevated`、`--shadow-pop` 与 `--radius-composer`，没有新增局部硬编码颜色。
- 图像与图标：本状态没有位图或插画；文件夹、加号、模型、思考程度和发送按钮继续使用项目现有图标组件。
- 文案与内容：项目目录、输入占位、模型和发送状态均保持 KeenCode 现有交互与模型语义。

## 交互与运行检查

- 点击“项目目录”可打开项目菜单，空项目状态下“添加项目”入口可见。
- 输入内容后欢迎语隐藏；清空输入后欢迎语恢复，避免长草稿与标题争抢空间。
- 浏览器控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

## 对比历史

1. 初次实现同时给外层欢迎卡片和内层 Composer 使用浮层阴影，视觉上存在重复层级。
2. 修正后只保留外层卡片阴影，内层 Composer 通过边框区分输入区域。
3. 复验截图与聚焦同屏对比中没有剩余 P0、P1、P2 差异。

## Follow-up Polish

- P3：输入设计稿的欢迎标题更大、输入区更高；KeenCode 保留已确认的 110 px Composer 高度和当前信息密度，避免本次适配反向改变已有对话输入体验。

historical result: passed; current release: not reverified

---

# Composer 浮层卡片 Design QA

## 对比目标

- 输入设计稿：居中输入卡片；原始输入未纳入仓库。
- 实现全景：`output/design-qa/composer-card-after.png`。
- 实现局部：`output/design-qa/composer-card-after-focus.png`。
- 同屏对比：`output/design-qa/composer-reference-vs-after.png`。
- 浏览器视口：`680 × 620` CSS px，截图密度为 1。
- 参考原图：`2317 × 852` px；参考区裁切为 `(330, 270)–(1680, 510)`，归一化为 `675 × 120` px。
- 状态：浅色 Dream 皮肤；新对话；无项目、无可用模型；输入为空；发送按钮禁用。

## 可见结果

- Composer 实测 `648 × 110px`，圆角 `24px`，内边距 `12px`；相较调整前的 `12px` 圆角与弱阴影，窄栏中的卡片已达到确认的比例和悬浮层次。
- 加号保留 `32 × 32px` 命中区域，但静止态背景改为透明；发送按钮继续承担右侧主操作，不改变现有功能和图标资产。
- 阴影与边框直接复用 `--shadow-pop`、`--glass-border`，浅色和暗色皮肤均由现有令牌控制，没有新增局部颜色。
- 字体、字号、行高和文案保持项目现有 Composer 体系；Composer 不显示工具权限或审批控件。
- 本组件没有位图、插画或品牌图像；可见图标继续使用项目现有图标库，没有使用 CSS 图形或文本符号代替。

## 交互与运行检查

- “添加”按钮可打开现有 Composer 菜单，Escape 可关闭。
- 浏览器控制台 error/warn：0。
- 30 项聚焦测试、类型检查与 CSS 检查通过。

## 对比历史

1. 调整前：`output/playwright/composer-height-102.png`。输入高度已够用，但 `12px` 圆角、几乎不可见的浅色阴影和常驻灰底加号仍使卡片显得扁平。
2. 修正：新增 Composer 语义圆角令牌；改用现有浮层阴影和玻璃边框；纵向空间增至 `110px`；移除加号静止态底色。
3. 复验：`output/design-qa/composer-reference-vs-after.png`。没有剩余 P0、P1、P2 问题。

## 剩余 P3

- 输入设计稿使用上箭头发送图标，KeenCode 保留现有纸飞机图标；该差异不阻塞本次比例和层次调整。
- 无模型状态下右侧控件按既有禁用语义呈现较低对比度；配置模型后的正常态需由用户在原生桌面环境复核。

historical result: passed; current release: not reverified

---

# 思考程度与 Ultra 面板 Design QA

## 对比目标

- 输入设计稿：原始输入未纳入仓库。
- 实现截图：`output/design-qa/reasoning-ultra-1287x550.png`
- 像素差异：`output/design-qa/reasoning-ultra-reference-diff.png`
- 参考图与实现均为 1287 × 550 px；参考图用于确认“标题/当前值/滑轨”的内部结构，不作为 KeenCode 工作台整体皮肤。

## 可见结果

- 模型选择器之后出现独立思考程度触发器，面板标题、当前值和单值离散 Slider 符合确认的信息结构。
- Ultra 位于同一面板下半区，使用现有 shadcn/ui Switch；开启后面板保持打开，状态为 `checked`，不会改变 Slider 的值。
- 面板宽度为 320 px，Ultra 说明在当前简体中文下不再孤立换行；菜单未裁切、未遮挡发送按钮。
- Web 验证环境没有 Tauri 供应商目录，因此 Slider 正确显示“不支持”禁用态；支持模型的档位映射由组件测试覆盖。
- 控制台 error/warn：0。

## 差异说明

- 输入设计稿展示脱离产品上下文的双值 Temperature 示例；当前需求是模型元数据给出的单个离散思考档位，因此实现使用单 Thumb，不引入 Temperature、双值范围或白底示例页面。
- 实现直接复用 KeenCode 当前菜单表面、文字、Slider 和 Switch 令牌；像素差异图主要反映产品工作台上下文与设计示例页面的有意差异。

historical result: passed; current release: not reverified

---

# Ultra Switch 可见性 Design QA

## 证据

- 缺陷截图：558 × 264 px，浅色 Dream 皮肤，Ultra 关闭；原始输入未纳入仓库。
- 修复后关闭态：`output/design-qa/ultra-switch-visible-off.png`。
- 修复后开启态：`output/design-qa/ultra-switch-visible-on.png`。
- 同屏对比：`output/design-qa/ultra-switch-visibility-comparison.png`。
- 浏览器视口：1287 × 550 CSS px；实现聚焦截图原始输出为 312 × 177 px，对比图移除浏览器裁切留白后统一归一化到 264 px 高。

## 发现与修复

- [P1 已修复] `app.css` 的全局透明 `button` 重置覆盖了 shadcn Switch 的状态背景，导致关闭态轨道和 Thumb 都接近白色；开启态也只有极淡轮廓。
- 公共 Switch 现在使用皮肤语义令牌：关闭态为 `--bg-active` 轨道、`--text-tertiary` 轨道与 Thumb 边界；开启态为实色 `--accent` 轨道和 `--accent-fg` Thumb。
- 修复后的实际渲染值：关闭态轨道 `rgba(196, 61, 85, 0.1)`、边界 `rgba(90, 50, 60, 0.4)`；开启态轨道与边界均为 `rgb(196, 61, 85)`。颜色、位置与形状共同区分状态。

## 必要表面检查

- 字体和文案：未改变；Ultra 标题和说明保持原层级与换行。
- 间距和布局：Switch 尺寸、位置、Thumb 位移及面板尺寸未改变。
- 颜色和令牌：只复用当前皮肤的状态令牌，没有引入原始颜色或面板局部覆盖。
- 图像与图标：本次没有图像或图标资产。
- 交互：关闭和开启均已实际点击；`data-state` 分别为 `unchecked`、`checked`，面板保持打开。
- 控制台 error/warn：0。

## 对比结论

- 缺陷截图中的白底白轨道已消失；关闭态存在可辨识的有色轨道和双边界，开启态为明显实色轨道。
- 没有剩余 P0、P1、P2 问题。

historical result: passed; current release: not reverified

---

# 新建对话单卡片结构 Design QA

## 对比目标

- 用户指出的错误实现：原始输入未纳入仓库。
- 输入设计稿：原始输入未纳入仓库。
- 修正后实现：`output/design-qa/new-chat-single-card-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-single-card-comparison.jpg`。

## 视口与归一化

- 设计真值：1526 × 347 px。
- 实现：1280 × 720 CSS px，浏览器报告 `devicePixelRatio = 2`，截图输出为 1280 × 720 px。
- 同屏对比将设计真值缩放到 1280 px 宽；实现裁切 `(190, 285)–(1090, 485)` 后缩放到 1280 px 宽，两侧均留白到 1280 × 360 px，再左右并排。
- 状态：浅色界面；新建对话；未选择项目；无可用模型；输入为空；发送按钮禁用。

## Findings

- [P1 已修复] 原实现同时给组合容器和 Composer 设置边框、白色表面与阴影，形成“卡片套卡片”。修正后组合容器只保留无边框、无阴影的中性顶部背景，唯一悬浮卡片是白色 Composer。
- 字体与文案：保持 KeenCode 当前字号、字重、品牌、交互和模型语义。
- 间距与布局：顶部项目栏 48 px，Composer 110 px；两者同宽、无外层留白和间隙，形成连续的“顶部上下文 + 下方输入区”结构。
- 颜色与令牌：浅色顶部背景使用 `--surface-tint-light` 与 `--bg-main` 混合；暗色回落为 `--bg-main` 与 `--text-primary` 的语义混合。没有引入主题专属硬编码色。
- 图像与图标：本状态没有位图或插画；继续使用项目现有文件夹、模型、思考程度和发送图标。
- 内容与交互：项目菜单可以正常打开，“添加项目”入口可见；已有对话 Composer 不使用该新建对话组合样式。

## 对比历史

1. 用户截图确认首轮实现存在双层白色卡片和重复阴影。
2. 移除组合容器的边框、阴影和内边距，把悬浮层级还给 Composer；顶部改为中性背景区。
3. 修正后同屏对比中，顶部项目栏与单一输入卡片的层级关系已与设计真值一致；没有剩余 P0、P1、P2 问题。

## 运行检查

- 项目菜单交互正常；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

historical result: passed; current release: not reverified

---

# 新建对话卡片比例协调 Design QA

## 对比目标

- 用户对比截图：原始输入未纳入仓库。
- 独立输入设计稿：原始输入未纳入仓库。
- 修正后实现：`output/design-qa/new-chat-balanced-after.jpg`。
- 聚焦同屏对比：`output/design-qa/new-chat-balanced-comparison.jpg`。

## 视口与归一化

- 设计真值：1526 × 347 px。
- 实现：1280 × 720 CSS px，浏览器报告 `devicePixelRatio = 2`，截图输出为 1280 × 720 px。
- 同屏对比将设计真值缩放至 1280 px 宽；实现裁切 `(280, 285)–(1000, 475)` 后等比缩放至 1280 px 宽，两侧留白到 1280 × 360 px，再左右并排。
- 状态：浅色界面；新建对话；未选择项目；无可用模型；输入为空；发送按钮禁用。

## Findings

- [P2 已修复] 当前卡片 864 px 的宽度明显大于设计稿约 672 px 的主体比例，顶部 48 px 高度和双层浮层阴影进一步放大了松散感。
- 修正后新建对话卡片宽度为 672 px，顶部栏为 40 px，Composer 保持用户已确认的 110 px 高度；整体宽高节奏与参考一致。
- 阴影由通用浮层的两段 28/48 px 阴影收敛为单段 10/28 px 阴影，只表达 Composer 高于顶部背景的层级。
- 字体与文案：项目栏调整到 14 px 主文字色；Composer 和产品文案保持 KeenCode 现状。
- 颜色与令牌：继续使用现有主题令牌和语义颜色混合，没有新增硬编码主题色。
- 图像与图标：没有新增图像资产；所有可见图标继续使用项目现有图标组件。
- 响应式：672 px 只作为新建对话的最大宽度；窄面板仍按可用宽度收缩，不改变既有对话内容与底部 Composer 的宽度规则。

## 对比历史

1. 用户确认单卡片结构正确，但同屏对比显示卡片过宽、顶部偏高、阴影偏重。
2. 仅收紧新建对话组合容器和欢迎态阴影，未改动 DOM、菜单或已有对话样式。
3. 修正后同屏对比没有剩余 P0、P1、P2 问题。

## 运行检查

- 项目菜单可正常打开，“添加项目”入口可见；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

historical result: passed; current release: not reverified

---

# 项目选择文案与创建项目输入框 Design QA

## 对比目标

- 项目选择文案输入：原始输入未纳入仓库。
- 输入框缺陷输入：原始输入未纳入仓库。
- 修复后项目选择：`output/design-qa/2026-08-26-select-project.png`。
- 修复后输入框：`output/design-qa/2026-08-26-add-project-focus.png`。
- 输入框同屏对比：`output/design-qa/2026-08-26-add-project-comparison.png`。

## Findings

- [P2 已修复] 无项目时的默认文案由“项目目录”改为动作导向的“选择项目”；英文和繁体中文同步更新。
- [P2 已修复] 项目名称输入框的外扩焦点轮廓会被面板内容边界裁切，形成底部蓝线和零散角标。焦点反馈改为输入框自身边框色，不再改变盒模型或侵入相邻区域。
- 输入框实测高度 42 px，聚焦时 `outline: none`、`box-shadow: none`；与“源文件夹”标签之间保留 16 px 间距。
- 创建项目面板的尺寸、字段顺序、文件夹选择和提交逻辑均未改动。

## 运行检查

- 新建对话显示“选择项目”；项目菜单和“添加项目”入口可正常打开。
- 创建项目面板自动聚焦项目名称输入框，标签与输入框无重叠；控制台 error/warn：0。
- 29 项聚焦测试、TypeScript 类型检查、CSS 检查和 `git diff --check` 均通过。

historical result: passed; current release: not reverified

## 2026-09-07：恢复内置网络服务的设置文案

- 基线：`841f3d2`，通过 `git archive HEAD src public index.html vite.config.ts tsconfig.json package.json components.json` 在 `/tmp/keencode-restore-baseline` 重建；复用当前 node_modules，仅将 Vite/HMR 端口改为 1431/1432。
- 对比环境：macOS，Playwright Chromium，同一浏览器会话，简体中文、浅色、`#/settings/general` 顶部，viewport 1280×820，deviceScaleFactor 1。基线地址 1431，修改后地址 1421。
- 截图及像素结果：`output/playwright/restore-builtins/{before,after,diff}.png`、`pixels.json`。9,427 个变化像素，边界 `(288,415)-(1100,464)`，全部位于服务说明与输入框占位文案。设置行尺寸保持 912×119，未修改 DOM、CSS 或组件变体。
- 有意变化：说明空地址使用内置服务及数据发送去向；占位符显示实际服务地址。
- 复现：分别启动上述两份 Vite 源码，使用相同 Chromium page、1280×820 视口访问 `#/settings/general`，待页面完整加载后截图；以 Pillow `ImageChops.difference(before.convert('RGB'), after.convert('RGB'))` 比较并统计非零像素。
- 原生桌面未验收：本次工具不提供原生桌面 UI 控制；上述证据仅覆盖浏览器渲染，不替代 macOS/Windows 原生发布验收。

### 2026-09-07 插件市场错误文案纠正

- 基线：`69f5b52`，本次工作区；macOS 开发实例。保留原有错误区域 DOM、组件与样式，仅有意替换错误文案。
- 修改：市场加载、添加、移除和安装失败不再进入聊天模型错误分类器；补齐简中、繁中、英文提示。
- 验证：`pnpm run typecheck`、`pnpm exec vitest run src/components/ExtensionsBuildExtras.test.ts`（6 项通过）。
- 未验证：当前工具无法控制原生 WebView，未取得相同状态、视口、DPR 的本次原生基线/结果截图，因此未完成原生像素比较；不沿用历史截图作为本次通过证据。
- 网络实测：系统 HTTP/HTTPS 代理关闭；直连 ghfast.top 的 Git 市场地址返回 connection reset（HTTP 000），尚未确认当前原生实例成功渲染市场。

### 2026-09-07 市场保留已安装插件

- 基线：`git show HEAD:src/components/ExtensionsBuildExtras.tsx` 保存于 `output/playwright/installed-cards/Before.tsx`；当前 HEAD 为 `69f5b52`。浏览器 Chromium，1280×820，DPR 1，同一开发服务及 CSS。
- 场景：独立挂载市场组件，模拟 Tauri 返回一个 `official/review` 插件；比较安装前布局，点击安装并确认，断言卡片仍为 1 个、“已安装”出现、“管理”触发管理回调。
- 产物：`output/playwright/installed-cards/{before,after-before-install,diff,installed}.png`。安装前像素差异为 0；安装后有意新增名称旁的状态徽标，并将安装按钮替换为管理按钮。检查徽标宽度不超过 150px，避免被内容列拉伸。
- 验证：前端类型检查、37 项相关测试；Rust 扩展 109 项测试。浏览器场景使用模拟 IPC，不代表本次真实插件安装或原生 WebView 像素验收。
- 原生范围：开发实例通过 Tauri 自动重新编译；当前工具不支持控制原生 WebView，未补齐原生截图比较。

### 2026-09-07 插件模型适配设置

- 基线：`5a40abf`，源码重建于 `/tmp/keencode-plugin-alias-baseline`；同一 harness 分别挂载基线与当前市场组件，复用现有 CSS。1280×820、DPR 1、Chromium；模拟 Tauri IPC，无真实插件安装或模型调用。
- 产物：`output/playwright/plugin-aliases/{before,after,diff,mapped,deleted}.png`、`baseline.json`、`current-source.tar.gz`。页面比较最终使用 `settings-page__main` 容器；像素差异 492，范围 `(868,42)-(937,70)`，仅新增“适配设置”入口。弹窗行为截图捕获于早期独立容器，不能作为完整设置页布局验收。
- 复现：`pnpm exec vite --host 127.0.0.1`；打开 `/output/playwright/plugin-aliases/index.html?before` 与不带 query 的同一路径。harness 和基线组件位于同目录。
- 浏览器断言：默认继承；sonnet 选择 demo::fast 并保存；重新打开显示 fast；模拟供应商删除后重新打开显示“跟随当前会话”。错误日志只有 harness 缺 favicon 的 404，未见应用运行错误。
- 后端：508 项全量测试通过，另加市场名称不同的安装计划专项通过；覆盖本地配置保存/读取、供应商/模型缺失、插件 Agent 解析、color、安装清单原文保持。前端类型检查及 66 项相关测试通过。
- 限制：原生桌面控制不可用，未验证真实 WebView；未做 Windows 实机验证。设置读取仅在打开弹窗时触发，后台不增加轮询；不新增依赖或 CSS，未作性能基准测量。

### 2026-09-08 Hook 内部上下文气泡过滤

- 问题基线：用户提供原生桌面截图 `/var/folders/hc/nf14zb5555v06r8l386frvl00000gn/T/codex-clipboard-46479d2f-53fe-4b13-adf9-d5d669d3ba42.png`；源码基线 HEAD `5a40abf` 加本地插件适配设置改动。截图没有会话复现输入、视口与 deviceScaleFactor 记录，不能充当可重建的像素基线。
- 有意差异：新产生的 Hook 内部上下文不再生成用户气泡；没有修改前端组件、DOM、样式或布局。真正用户消息的显示保持原投影行为。
- 验证：`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib hook_meta_context` 覆盖实时、历史回放及相同正文的用户输入；`cargo test -p keencode-runtime hook_meta_context` 覆盖日志冷恢复和模型正文；Agent Hook 测试验证生成内部标记；Provider 测试验证三协议请求不受标记影响。
- 原生限制：当前没有原生 WebView 操控工具，无法重建相同会话状态的原生像素比较；协议投影测试不能替代原生视觉验收。此项未验证。开发进程由现有 Tauri watcher 重编译。
- 不新增依赖、后台任务或轮询；每条消息新增一个布尔值，投影为常数时间判断。未测性能基准。
- 执行结果：桌面后端 510 通过、3 忽略；核心四包测试 656 通过、1 失败、1 忽略；补跑剩余 Runtime 协作恢复 2 通过，三协议内部标记专项 1 通过。唯一失败在 `compaction_cold_recovery.rs:361`：断言仍要求中文摘要提示词，而 HEAD 的 `context.rs:26` 已为英文，本次未改这两个文件。Hook 生成、冷恢复与实时／历史过滤专项均通过。日志位于 `/tmp/hook-meta-{tests,desktop,provider,recovery}.log`。

### 2026-09-08 模型最大输出预算与导入参数

- 基线：HEAD `5a40abf9fa8ab900b381a38dcf44900c7f3d0675` 加本地未提交源码；修改前 ProvidersPanel 保存于 `output/playwright/model-limits/Before.tsx`，可重建源码与同一 CSS/公共资源保存于 `source.tar.gz`。当前组件与基线组件通过同一模拟 IPC harness 加载，不读取真实供应商凭据。
- 环境：Chromium，1280×900，DPR 1；`pnpm exec vite --host 127.0.0.1 --port 1431`，打开 `/output/playwright/model-limits/index.html?before` 与不带 query 的地址。分别截图详情页、空白添加弹窗、未勾选模型的导入弹窗，均等待弹窗动画稳定。
- 有意差异：新增最大输出 Token 输入框；导入每行补齐上下文与视觉支持；弹窗随字段增长。沿用原 Input 样式，通过 ui/input 的 settings/compact 变体复用，无新增 CSS。
- 产物：`before*.png`、`after*.png`、`diff*.png`、`pixels.json`。详情页变化 37591 像素，边界 `(480,380)-(1096,494)`；添加弹窗 94977，导入弹窗 169861。弹窗对比包含尺寸变化及模糊背景中参数行的有意变化。
- 行为：查询到 64000 自动填入；无记录使用 128000；手工修改保存；导入修改上下文 256000、输出 96000、视觉 true 后保存正确；查询返回不能覆盖用户已编辑输出；切换为未知模型会清除上一模型的上下文/视觉初值。
- 验证：TypeScript 检查、7 项前端相关测试、34 项供应商后端测试通过（1 项真实外网压缩测试忽略）；根/子代理实际本地 HTTP 请求测试通过，均发送配置的 `max_output_tokens=96000`。
- 边界：开发服务中途停止引起 Vite HMR WebSocket 错误，最终改用临时 1431 服务完成截图；未进行原生 WebView、Windows 实机或真实供应商请求验收。
- 资源影响：无新增依赖或轮询；添加模型查询有 400ms 防抖并丢弃过时响应，批量导入遵守每批 256 个的元数据接口上限。每模型增加一个持久数字和相应表单控件；未做性能基准测试。

### 2026-09-08 模型列表展示与编辑分离

- 基线：HEAD `5a40abf9fa8ab900b381a38dcf44900c7f3d0675` 加当前未提交改动；修改前组件存于 `output/playwright/model-editor/Before.tsx`，同一 CSS、公共资源与完整前端源码保存于 `source.tar.gz`。用户原生截图用于确认输入框重叠问题，无法从该裁剪图还原视口与 DPR，未将其冒充像素基线。
- 方案：列表仅展示名称、视觉/上下文摘要、编辑和删除操作；沿用原模型表单作为添加/编辑弹窗。导入列表保留选择与编辑入口，编辑后回到选择列表。修改先进入供应商草稿，取消/Escape 不写入，最终仍由供应商保存按钮持久化。
- 环境：Chromium 1280×900、DPR 1；`pnpm exec vite --host 127.0.0.1 --port 1431`，页面 `/output/playwright/model-editor/index.html?before` 和不带 query 的当前版本。六个模型，相同数据和滚动位置。
- 产物：`before.png`、`after.png`、`edit.png`、`import.png`、`diff.png`、`pixels.json`。变化 13502 像素，边界 `(710,374)-(1033,562)`，仅模型行参数展示和编辑入口发生有意变化。
- 验证：类型检查和 7 项相关测试通过；浏览器验证无列表内输入框、编辑回填、取消不写入、修改上下文/输出/视觉并保存、导入编辑返回后保持勾选且保存正确、Escape 返回列表、第六个模型滚动后可编辑。
- 边界：临时预览服务与桌面开发服务的 HMR 端口冲突产生 WebSocket 400，截图和行为通过显式完整导航加载当前源码完成；未验证原生 WebView 和 Windows。无新增依赖、CSS 或后台轮询，复用既有表单与 shadcn 按钮，未做性能测量。

### 2026-09-08 提问覆盖输入框与单选自动翻页

- 基线：当前工作树修改前的 AskUserModal 与 app-conversation.css 保存于 `output/playwright/ask-overlay/{Before.tsx,before.css}`；源码和公共资源快照为同目录 `source.tar.gz`。用户裁剪截图只作为问题证据，未冒充可重建像素基线。
- 有意变化：提问面板底部与输入框底部重合，宽度复用 composer-stack 上限，最小高度覆盖输入区；消息底部留白由相加改为取较大值。被覆盖的 composer 设置 inert。单选选择后自动前进，多选和最后一题保留用户控制，翻页焦点转至题目。
- 复现：`pnpm exec vite --host 127.0.0.1 --port 1431`，`/output/playwright/ask-overlay/index.html?before` 与不带 query 的页面；同一真实 AskUserModal 与现有 CSS，输入区为采用真实 class 的简化布局宿主。Chromium 1280×900、DPR 1，同一三题夹具与初始状态。
- 产物：before.png、after.png、diff.png、pixels.json；416713 个像素变化，边界 `(147,405)-(1133,900)`，对应下移、加宽及覆盖输入框。
- 验证：测量卡片与输入框 left/right/bottom 一致且 top 覆盖；760/1280 宽度、摘要栏开启状态同样通过；单选自动翻至第二题，多选连续选择两项仍停留当前题，最后一题不自动提交，最终提交三题答案正确。类型检查、8 项相关测试和 CSS 检查通过。
- 限制：原生 WebView/Windows 未验收，简化宿主无法替代完整原生桌面验证；临时预览 HMR 与桌面端口冲突，仅通过完整导航加载源码。未新增依赖、后台轮询或窗口级遮罩；未做性能基准测试。


# 2026-09-08 Markdown 自动链接中文标点

- 修改前源码快照：`output/design-qa/markdown-url-20260908/before-source.zip`，包含工作区当时的 `src/`、`public/`；不使用 HEAD 代替已有未提交修改。
- 复现正文：`**文件结构**（4 个文件，启动后访问 http://localhost:3000）：`。GFM 自动链接把 `）：` 纳入地址，最终编码为 `%EF%BC%89%EF%BC%9A`。
- 有意差异：裸 HTTP(S) 自动链接在中文句读、右括号和右引号处结束，后续文字保留在链接外；显式 Markdown 链接、尖括号链接、百分号编码地址和代码不改写。聊天、流式正文及资源 Markdown 共用同一 remark 转换，不修改组件样式。
- 验证命令：`pnpm exec vitest run src/components/lobe-chat/MarkdownChat.test.tsx src/lib/incrementalMarkdown.test.ts`、`pnpm run typecheck`。
- 原生视觉验收未完成：本会话原生 UI 控制 API 禁用，无法获取同状态原生截图及像素差。已保存可重建源码；在隔离副本解压基线、复用依赖并运行 `pnpm dev:desktop`，与修改版使用相同 macOS、视口、deviceScaleFactor、主题及上述正文比较，检查链接文本、悬停地址及点击目标。本次服务端渲染回归测试不能替代原生验收。


# 2026-09-08 行内链接文字基线对齐

- 修改前源码：`output/playwright/link-alignment/before-source.zip`（包含已有工作区修改）。复现页面 `output/playwright/link-alignment/index.html` 直接导入当前 `MarkdownChat`、Button、图标及完整原有 CSS，不重写组件。
- 根因：按钮的 `inline-flex` 使用 `align-items: center`，第一个 flex 子项图标参与容器基线，导致文字高于周围正文。改为 `align-items: baseline`，图标独立 `align-self: center`，由文字提供基线；未改字体、颜色、间距和点击逻辑。
- 浏览器对比：macOS、Playwright Chromium、浅色、1268×300、deviceScaleFactor=1，同一正文与视口；`before.png`、`after.png`、`diff.png` 均位于 `output/playwright/link-alignment/`。差异范围 `(330,40)–(481,57)`，1611 个变化像素，仅位于链接及图标区域，周围文字位置不变。
- 复现：`pnpm exec vite --host 127.0.0.1 --port 14321`，访问 `/output/playwright/link-alignment/index.html`；前后源码分别加载后截图。既有开发服务占用 HMR 1422，验证使用整页刷新。
- `pnpm run lint:css`、`pnpm run typecheck` 通过。原生 UI 控制 API 禁用，本次浏览器组件验证不等同于原生 WebView 验收。
