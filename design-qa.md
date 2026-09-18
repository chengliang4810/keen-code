# 2026-09-17 常驻系统托盘图标与「关闭窗口后保留在系统托盘」设置

- 需求：新增常驻系统托盘图标（macOS 为菜单栏图标）。关闭主窗口后应用不退出，隐藏窗口并移除 macOS Dock 图标；托盘菜单提供「新建对话 / 显示窗口 / 最近 5 个会话 / 退出」；新增设置项控制关闭窗口是隐藏到托盘还是直接退出。
- 现状：应用没有托盘图标，关闭主窗口即走统一退出入口（`useAppDialog` 的 `onCloseRequested` → `app_request_exit`）；`tauri` 依赖未启用 `tray-icon` feature。
- 修改（后端）：`src-tauri/Cargo.toml`（tauri 启用 `tray-icon`）；`src-tauri/src/tray.rs`（新增：托盘与菜单构建、菜单项标识→动作映射、`tray_set_menu` 与 `app_close_window` 命令、`show_main_window`/`hide_main_window`、macOS Dock 可见性切换、Windows 左键单击显示窗口）；`src-tauri/src/lib.rs`（注册 `mod tray`、启动时 `tray::install`、注册两个命令、`handle_run_event` 处理 `ExitRequested`（有运行中任务时先恢复窗口再弹确认）与 macOS `Reopen`）；`src-tauri/src/app_settings.rs`（新增 `close_to_tray` 字段、补丁与默认值 true）；`src-tauri/src/app_exit.rs`、`src/lib/api.ts`（删除被 `app_close_window` 取代的 `app_request_exit` 命令与前端封装）；`src-tauri/icons/tray-macos.png`（模板图标：单色 + alpha）、`src-tauri/icons/tray-windows.png`（品牌蓝托盘图标）。
- 修改（前端）：`src/hooks/useTrayMenu.ts`（新增：按当前界面语言与会话投影推送托盘菜单，启动页结束后才推送以免默认语言覆盖后端按持久化语言生成的兜底菜单；监听 `app://tray-new-chat`、`app://tray-open-session` 回投到既有导航）；`src/App.tsx`（装配 `useTrayMenu`）；`src/hooks/useAppDialog.ts`（窗口关闭改调 `appCloseWindow`，保留 `preventDefault`）；`src/hooks/useAppSettings.ts`、`src/features/app/SettingsRoute.tsx`、`src/components/SettingsPage.tsx`（新增「关闭窗口后保留在系统托盘」开关，位于「保持电脑运行」之后）；`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（`tray.show`、`tray.quit`、`settings.closeToTray`、`settings.closeToTrayDesc` 三语文案）；`src/components/SettingsPage.test.ts`（新增托盘设置行契约）、`src/lib/appSettingPersistence.test.ts`（设置字面量补字段）。
- 交互约定：macOS 菜单栏左键单击直接弹菜单（与参考截图一致，菜单首项为「显示窗口」）；Windows 托盘左键单击显示窗口、右键弹菜单。
- 门禁：`pnpm run typecheck` 通过；`pnpm exec vitest run` 145 文件 / 1429 项通过；`cargo check --manifest-path src-tauri/Cargo.toml -p keencode-desktop` 通过；`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib` 641 项通过（含 `tray` 3 项）；`cargo clippy --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib --all-targets` 对本次改动无告警。
- 基线：`d697356559dc1b8a24a08625719c23ab3ee53f97`。工作区并发 WIP 较多，基线取「当前工作区 `src/` 副本 + 仅移除本次新增的设置行（`closeToTray` props 声明、解构与 `settings-anchor-close-to-tray` 行）」，只隔离本次可见差异。基线 `SettingsPage.tsx` SHA-256 `08bad97540439dd848da46247745fb4c4e17ea6ac1718c681ac8bb225efc2efa`，当前 `552512cb817b7cdb0687ddeaa56f875f081910d8a9f771a8f158d3e33f951786`；`tray.rs` `c3a0c1739de5003ea8ae645b5d80906e6be0756635e461ac48253ca6c9956afd`；`useTrayMenu.ts` `d4798f3492f25331fbde01bb9e8a4ca45b8a8844c73ddde14b4cf56548e67343`；`tray-macos.png` `ecd0d69abd94b2275cfa24e50f9ee446b9d51959ffae05daca45f42589ff38d5`；`tray-windows.png` `421cd58709e02801a5881d20599d19b5a539236a4559f44b235dd2a5cfc0c648`。
- 夹具：`output/playwright/tray-20260917/`。`baseline/` 为当前工作树 `src/` 副本并仅还原新增设置行，`current/src` 符号链接指向工作树；两变体共用 `node_modules`、使用独立 `cacheDir`。`QA_VARIANT=baseline ../../../node_modules/.bin/vite --config vite.config.mts` 与 `QA_VARIANT=current ...` 分别起 `http://127.0.0.1:14421/`、`http://127.0.0.1:14422/`；`node shoot.mjs > geometry.json` 采集几何，`python3 compare.py` 比对。夹具渲染真实 `SettingsPage` 的「通用」分区与合成设置值，不调用模型、不写用户数据。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1280×900，deviceScaleFactor=1。
- 几何（baseline→current）：新增行 `#settings-anchor-close-to-tray` 位于 `y=369.19`、`912×79`、开关 `checked`（基线不存在，`trayRowCount` 0→1）；其上方 `#settings-anchor-keep-awake`（278,290.19,912×79）与基线完全相同；其下方 `#settings-anchor-background-agent-limit` 由 `y=369.19` 下移到 `y=448.19`，后续各行整体下移 79px，行高与宽度不变。
- 像素：RGB 任一通道差值 >16 计入，未掩码。差异 40755/1152000（3.53776%），范围 `[278,389,1190,900)`，即新增行控件与该行下方整体下移的区域；新增行以上区域逐像素一致。两页 Console 均 0 error、0 warning。
- 未验收：夹具为浏览器组件渲染，不替代原生 Tauri WebView 实机验收；未在 macOS 实机确认「关闭红灯 → 窗口隐藏且 Dock 图标消失 → 点菜单栏图标恢复窗口」，未在 Windows 实机确认「左键单击显示窗口 / 右键弹菜单」；Rust 侧非 macOS 分支（Windows 托盘点击事件、`menu_text` 的 `&` 转义）本机无法编译验证，需由 Windows CI 或实机覆盖。

---

# 2026-09-17 管理模型默认选中该对话供应商 / 移除响应等待超时设置

- 需求：1) 对话框模型菜单点击「管理模型」时，模型设置页默认选中该对话当前使用的供应商（会话内切换的模型可能不属于全局活跃供应商）；2) 移除供应商表单的「响应等待超时」设置，后端固定默认 300 秒。
- 现状：会话级模型切换走 `session/set_config_option`，前端只在会话模型缓存里存裸模型 ID，供应商信息丢失；模型设置面板始终选中列表第一项。响应等待超时是每供应商的 `read_timeout_seconds` 持久化字段，前后端贯通。
- 修改（默认选中）：`src/lib/modelCatalog.ts`（新增 `providerIdFromSessionReference`）；`src/hooks/acp-runtime/history.ts`、`src/hooks/acp-runtime/events.ts`（会话模型缓存改存 `providerId::modelId` 完整引用，恢复、导航与终态打点处解析回模型 ID）；`src/hooks/useSessionNavigation.ts`（导航恢复解析引用）；`src/features/app/main/ComposerToolbar.tsx`（管理模型入口携带会话供应商，缺省回退全局活跃供应商）；`src/hooks/useAppRoute.ts`（新增 `settingsProviderId` 路由状态）、`src/App.tsx`、`src/features/app/SettingsRoute.tsx`、`src/components/SettingsPage.tsx`、`src/components/ProvidersPanel.tsx`（`initialProviderId` 优先选中，缺失或不存在时回退第一项）。
- 修改（超时移除）：`src-tauri/src/providers.rs`（删除 `ProviderRecord`/`CustomProvider`/`ProviderUpsert` 的 `read_timeout_seconds` 字段、校验函数与已知字段列表项，Runtime 映射不再覆盖 `read_timeout`）、`src-tauri/src/lib.rs`（`providers_upsert` 删除该参数）、`src-tauri/src/agent_runtime/benchmark.rs`、`src-tauri/src/agent_runtime/live_messages_runtime_tests.rs`、`src-tauri/src/agent_runtime/live_prompt_tests.rs`、`src-tauri/src/agent_runtime/live_messages_agent_tests.rs`、`src-tauri/src/providers/live_context_tests.rs`（构造点删除该字段）；`src/lib/api.ts`、`src/components/ProvidersPanel.tsx`（表单字段与校验删除）、`src/components/ProvidersPanel.test.tsx`（断言同步）、`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（`prov.readTimeout*` 三语文案删除）。Runtime `ProviderConfig.read_timeout` 保持默认 300 秒；磁盘旧配置中的 `readTimeoutSeconds` 走既有未知字段忽略路径并记诊断日志（`provider_config_with_removed_context1m_maps_to_runtime_registry` 夹具覆盖）。
- 门禁：`pnpm exec vitest run` 145 文件 / 1424 项全部通过（含 `history.test.tsx` 断言更新为完整引用）；`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib providers` 47 项通过；`cargo check --manifest-path src-tauri/Cargo.toml -p keencode-desktop --features benchmark` 通过；`pnpm run typecheck` 仅余 2 个既有错误（`closeToTray`，另一会话未完成改动，非本次范围）。
- 基线（超时移除）：工作区并发 WIP 较多，基线取「当前工作区 `src/` 副本 + 仅回填超时字段（`ProvidersPanel.tsx`、`messages.ts`、`zh-tw.ts`）」，只隔离本次可见差异；追溯提交 `cbf7bb6297a0641b1e6030d6b8db2e304359715a`。
- 夹具：`output/playwright/providers-timeout-remove-20260917/`（baseline 14421 / current 14422）；`output/playwright/providers-preselect-20260917/`（default 14431 不传预选 / preselect 14432 预选 `fix-local`，两变体共用工作树源码，仅 `initialProviderId` 传参不同）。均为 `QA_VARIANT=… vite --config …/vite.config.mts` 起服务后 `node shoot.mjs` 截图、`python3 compare.py` 比对；`providers_list` 用内存桩，不读真实配置、不写用户数据。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1280×800，deviceScaleFactor=1。
- DOM 与几何（超时移除）：表单标签 6 → 5，「响应等待超时」标签消失；API Key 标签 y=325 → y=233（后续字段整体上移），x/宽（338/443）不变。像素差异 22161/1024000（2.16416%），范围 [338,235,1238,603)，即被移除字段及其后上移内容；两页 Console 均 0 error、0 warning。
- DOM 与几何（默认选中）：default 选中列表第一项 Workbuddy；preselect 选中 Fix Local（会话实际供应商，非全局活跃项），右侧标题均为「编辑提供商」。像素差异 58044/1024000（5.668359%），范围 [24,106,1238,566)，即左栏选中项与右侧表单内容差异；两页 Console 均 0 error、0 warning。
- 未验收：浏览器夹具不替代原生 Tauri WKWebView 实机点击验收；端到端「管理模型 → 设置页选中」未在真实桌面会话内复跑（夹具直接验证 `initialProviderId` 生效）。

---

# 2026-09-17 模型选择器（供应商归属 / 当前项可见 / 视觉能力标签）

- 需求：底部模型触发器显示「供应商/模型」；级联列表能看出当前使用的供应商与模型；支持图片输入的模型在名称后带「视觉」标签。
- 现状：`ComposerModelMenu` 触发器只显示模型名；供应商行仅靠 `cmm__dropdown-active` 底色区分，无勾选；模型行无能力标签。数据侧 `CustomProvider.supportsVision` 已由供应商配置保存并驱动运行时 `image_input`，但 `useProviderModels` 未把它投影进 composer 模型目录，标签无处取数。
- 修改：`src/components/ComposerModelMenu.tsx`（触发器改为 `${providerLabel}/${modelLabel}`，供应商标签缺失时回退纯模型名；新增 `activeProviderId = activeModel?.providerId ?? providerId`，供应商行按它渲染勾选，模型行按它判定选中；模型行在 `model.supportsVision` 时渲染 `.cmm__badge`）；`src/hooks/useProviderModels.ts`（供应商目录投影补 `supportsVision: provider.supportsVision[model]`，`availableModels` 合并改为 `contextWindow`/`supportsVision` 均为「手工配置优先、元数据兜底」）；`src/styles/app-resource.css`（新增 `.cmm__badge` 胶囊标签，`--radius-full`/`--border-subtle`/`--bg-hover` 令牌；`.cmm__trigger` 上限 200→260px 以容纳“供应商/模型”）；`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（新增 `composer.modelVision` 三语文案）；`src/features/app/main/ComposerToolbar.tsx`（传入 `vision` 标签）；`src/lib/modelCatalog.ts`（`supportsVision` 注释更正为供应商配置优先）。未新增依赖、后台活动或网络请求。
- 门禁：`pnpm run lint:css` 通过；`pnpm exec vitest run` 145 文件 / 1424 项全部通过（`ComposerModelMenu.test.tsx` 由 4 项增至 6 项：触发器拼接与回退、视觉标签与当前项可见性契约）；`pnpm run typecheck` 仅余 2 个既有错误（`src/hooks/useAppSettings.ts:419`、`src/lib/appSettingPersistence.test.ts:31` 的 `closeToTray`，属另一会话未完成改动，非本次范围）。本次未改 Rust。
- 来源门禁：`node scripts/clean-room-source-gate.mjs` 命中 54 项，与改动前基线一致（`result.json`、`src-tauri/prompts/README.md`、`src-tauri/src/providers.rs` 等既有命中，本次新增文本 0 命中）。
- 基线：`cbf7bb6297a0641b1e6030d6b8db2e304359715a`。基线 `ComposerModelMenu.tsx` SHA-256 `8b3bfa06bddf29e6d38ed74b4581802f9fafc67b9108b2c18c8b0e4903ee0027`，当前 `4f248f321f6db8a26c10024945a7190e53fce714f23bf7b7c15ae037578e89e8`；基线 `app-resource.css` `2ddea2ef22e693d80a75712c4c2a49725a742599ac8ada79cc5157909f890784`，当前 `02399925407bb9896b112aea805018fc52cbb1439fdf819d65dd5ad4a5532ffc`；`useProviderModels.ts` 当前 `6c0c6877d8f341494a33ebece512080cfe450d97ed3810f758247625911bb7b2`。
- 夹具：`output/playwright/model-menu-provider-20260917/`。`baseline/ComposerModelMenu.tsx` 与 `baseline/app-resource.baseline.css` 取自 `git show HEAD:`，`baseline-app.css` 与 `src/styles/app.css` 同序仅替换 `app-resource.css`，两变体共用 `node_modules` 与同一 vite 配置（`pnpm exec vite --config output/playwright/model-menu-provider-20260917/vite.config.mts`，`http://127.0.0.1:14381/`）；`node shoot.mjs` 截图与几何采集（悬停 Workbuddy 展开子菜单），`python3 compare.py` 比对。夹具用合成供应商/模型目录与空回调，不调用模型、不写用户数据。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，900×640，deviceScaleFactor=1。
- DOM 与几何：触发器文案 `cn:glm-5.3-flash` → `Workbuddy/cn:glm-5.3-flash`。供应商行 `cmm__dropdown-active` 两版均仅 Workbuddy；勾选字形由 0 项变为仅 Workbuddy（`svg.tabler-icon-check`，其余行只有右侧 chevron）。模型项：`deepseek-v3.2`/`glm-5.3-flash`/`cn:glm-5.3-flash` 由无标签变为 `.cmm__badge` 文本「视觉」，未配置视觉的 `hy3` 两版均无标签；`cn:glm-5.3-flash` 两版均为当前项。子菜单框 `x=216,y=123,w=196` 不变，高 `125.27→127.73`（标签行高所致）。
- 像素：RGB 任一通道差值 >16 计入，未掩码。差异 3876/576000（0.672917%），范围 `[32,32,413,252)`，即触发器与两级菜单区域；两页 Console 均 0 error、0 warning。
- 未验收：夹具为浏览器组件渲染，不替代原生 Tauri WebView 与 Windows 实机验收；未在桌面应用内实机确认「供应商设置里切换视觉开关后菜单标签随之更新」的端到端往返（该链路依赖 `providers_list` 回读，本次只覆盖其投影函数）。

---

# 2026-09-17 界面字号设置（侧栏项目名 / 对话名与对话正文跟随）

- 需求：新增「界面字号」设置（12–20px，默认 14px），整个界面文字随其缩放；侧栏项目名称、对话名称与对话正文使用同一套字号视觉层级。
- 现状：KeenCode 原先没有界面字号设置，字号散落在 5 档 `--text-*` 令牌与 289 处硬编码 px 中；`src/styles/harness/gradient-shadow-text.css` 的 `--dsh-content-font-size` 已被 19 处 Markdown/回合统计令牌引用，但全仓没有任何写入方，长期停在 14px 回退。
- 修改：`src/lib/uiFontSize.ts`（新增：范围校验、持久化、写入根元素 `--ui-font-size`）、`src/lib/uiFontSize.test.ts`（新增）；`src/styles/tokens.css`（`--ui-font-delta` = 字号 − 14px，`--text-xs/sm/md/lg/xl` 全部改为 `calc(基线 + var(--ui-font-delta))`，并把 `--dsh-content-font-size` 接到 `--ui-font-size`，激活原有 Harness 排版令牌）；`src/styles/app-conversation.css`、`app-features.css`、`app-foundation.css`、`app-resource.css`、`setup-wizard.css`、`effort-slider.css`、`src/components/lobe-chat/lobe-chat.css`（280 处 `font-size` 与同规则块内 26 处配对 `line-height` 换算为 `calc(Npx + var(--ui-font-delta))`；对话变量 `--chat-fs`/`--chat-prose-fs`/`--chat-fs-sm`/`--chat-fs-xs` 改为引用 `--text-md`/`--text-sm`/`--text-xs`）；`src/hooks/useThemeAppearance.ts`（新增 `uiFontSize` 状态与 `applyUiFontSizeChoice`）、`src/main.tsx`（首次绘制前应用，避免启动跳变）；`src/components/SettingsPage.tsx`、`src/features/app/SettingsRoute.tsx`、`src/App.tsx`（外观分区新增 12–20 数字输入，失焦保存，复用 `.settings-input--compact` 行形态）；`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（`settings.uiFontSize`、`settings.uiFontSizeDesc` 三语文案）；`src/components/SettingsPage.test.ts`、`src/components/lobe-chat/ConversationThread.test.tsx`（契约断言同步）。未新增依赖、样式类、后台活动或网络请求。
- 保持固定（不随界面字号）：图标字形尺寸（`.nav-item__icon` 18px、`.skill-chip__icon/glyph` 11px、`.cmm__chev` 10px、`.rp-kind*` 8–10px、`.rp-tab__x` 14px 共 8 处跳过），代码预览（`code-preview.css`）、终端（`TerminalPanel` xterm `fontSize: 12`）与布局几何（`height`/`padding`/`gap`）同样不变——与 ZCode 语义一致：字号只缩放文字，图标与布局尺寸不受影响。`src/styles/harness/gradient-shadow-text.css` 的 `--dsw-font-*` 尺寸令牌（20 处，含 small/code 密集次级文本）按该文件原有注释保持固定。转换后全仓 CSS 硬编码 `font-size` 由 311 处降至 31 处，剩余项均为上述图标字形、代码预览、Harness 尺寸令牌与 `tokens.css` 的基线定义。
- 门禁：`pnpm run typecheck` 通过（仅余 `src/lib/sidebarOrder.test.ts` 报错，属另一会话未完成改动，非本次范围）；`pnpm exec vitest run` 145 文件 / 1425 项全部通过（含本次新增 `uiFontSize.test.ts` 11 项、`SettingsPage.test.ts` 新增 2 项、`ConversationThread.test.tsx` 断言更新）；`pnpm run lint:css` 通过。
- 门禁例外：`pnpm test` 在 `scripts/clean-room-source-gate.mjs` 阶段失败，报错文件为 `result.json`、`src-tauri/prompts/README.md`、`src-tauri/src/providers.rs`（外部产品名与供应商示例标识），均为工作区既有未跟踪/并发产物，与本次改动无关；本次未改 `src-tauri/`，故未运行 Rust 测试。
- 基线：`f12ae5296b9ce8fa2dd1222d425c6245b0629953`。基线 `src/styles/tokens.css` SHA-256 为 `b1abc43750f123ae1899e712ecb1caaabdb2ddc41c94180f55ecc0a0bb1d5c82`，当前为 `6e49b348c809bfe67171846ba64e0be6dd5a844452b1b964d13123885a938220`；`app-foundation.css` 当前 `4acb9c5213199bf8f535ae0b6ccc29524d8ecffce4a9845c82d25c8bb91f4423`；`app-conversation.css` 当前 `ee272f1299a2c8551327835e6fb9bd4f14c571fdac77920b0bdea5beb269bb8a`；`lobe-chat.css` 当前 `f0f02d565ba427af6192e6b5fdde78f5570e16ed16b9f0c592f910c31e756e11`；`uiFontSize.ts` `dba221107a77c6efcd1e78e9ca94fa97182e300880cfdf68bc0a1027d3f8d924`；`SettingsPage.tsx` `a217843fcb383078f5e9bc4953697a33371070e395b994b43db981d69204005d`；`turn-metrics.css` 当前 `6cae694e41171c929158dcaa3510db1a4495831e9c1717faede3eab22b8e2612`。
- 夹具：`output/playwright/ui-font-size-20260917/`。`baseline/` 为 `HEAD` 源码副本并覆盖改动前的 7 个 CSS 与 `tokens.css`，`current/src` 符号链接指向工作树；两变体共用 `node_modules`、使用独立 `cacheDir`。`QA_VARIANT=baseline <repo>/node_modules/.bin/vite --config output/playwright/ui-font-size-20260917/vite.config.mts` 与 `QA_VARIANT=current ...` 分别起 `http://127.0.0.1:14401/`、`http://127.0.0.1:14402/`（注意：夹具目录无 `package.json`，必须直接调用 vite 二进制，`pnpm exec` 会因依赖检查失败）。`node shoot.mjs` 截图并采集几何，`python3 compare.py` 比对。夹具渲染真实 `ProjectTree`（项目名 + 两条会话）与真实 `ConversationThread`（含标题、段落、行内代码），合成数据，不调用模型、不写用户会话。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1280×800，deviceScaleFactor=1，`?size=14` 与 `?size=18`。
- 几何（baseline→current）：`14px` 下项目名 `14px/32px`、对话名 `14px/30px`、正文段 `14px/24px`、`h2 19px/28px`、行内代码 `12.6px`，两版完全一致；`18px` 下项目名与对话名 14→18px（行盒高度保持 32/30px 不变），正文段 14→18px、行高 24→28px，`h2` 19→23px/行高 32px，行内代码 12.6→16.2px。层级差值与比例保持。
- 像素：RGB 任一通道差值 >16 计入，未掩码。默认 `14px` 差异 **0/1024000（0.0%）**，即默认态逐像素一致；`18px` 差异 36904/1024000（3.603906%），范围 `[19,26,1216,454)`，落在侧栏项目/会话行与对话正文区域，其余区域不变。用量统计浮层单独开合后截图：`14px` 差异 **0/1024000（0.0%）**，浮层字号 12px/行高 18px 两版一致；`18px` 差异 34909/1024000（3.409082%），浮层字号 12→16px、行高 18→22px，面板宽 300→303.44、高 187→211（内容撑开，无固定高度裁剪）。两页 Console 均 0 error、0 warning。
- 未验收：夹具为浏览器组件渲染，不替代原生 Tauri WebView 与 Windows 实机验收；未在桌面应用内实机确认「设置改字号 → 重启应用后仍生效」的持久化往返，以及长文本、代码块、工具卡片在 20px 上限时无裁剪（已用规则扫描确认无 `height` 小于字号上限的固定高度块，但未逐屏目视核对）。

---



- 需求：左侧项目栏目标题右侧新增排序方式选择，默认按「最后用户消息时间」，另提供「更新时间」；无用户消息时间的会话按更新时间兜底。
- 背景：原排序完全依赖后端 `updated_at`（最近一条权威事件时间），后台运行中的会话每个模型输出/工具调用都会顶高该时间，导致切换对话时顺序自行变化。
- 修改：`crates/keencode-resources/src/catalog.rs`（`StoredSessionMetadata` 新增 `last_user_message_at_unix_ms`，由根用户 Turn 起点计算，`last_user_message_at()` 只取 `source_agent_id == root` 且 `root_turn_id == turn_id` 且无父 Turn 的 Turn）；`src-tauri/src/acp_host.rs`（`session/list` 每项在 `_meta["keencode/lastUserMessageAt"]` 透出 RFC 3339 时间，为 0 时不写入）；`src/lib/acp/api.ts`（`SessionListItem.lastUserMessageAt` 解析与形状校验）、`src/lib/sessionProjection.ts`、`src/features/app/models.ts`（`SessionRow.lastUserMessageAt`）；`src/lib/sidebarOrder.ts`（新增 `SidebarSortMode`、`loadSessionSortMode`/`saveSessionSortMode`、`sortSessionRows`：拖拽过的会话固定在前面并保持相对顺序，其余按所选时间倒序、同值按标识稳定排序）；`src/features/app/sidebar/SidebarSortMenu.tsx`（新增，复用 `DropdownMenu` + `DropdownMenuRadioGroup`）、`ProjectTree.tsx`（头部 `.tree-l1__actions` 首位插入）、`src/hooks/sidebar/useSidebarLists.ts`、`useSidebarDrag.ts`、`src/hooks/useSidebarController.ts`、`src/App.tsx`（状态与传参接线，拖拽只把涉及的两个会话记为固定项）；`src/components/icons.tsx`（新增 `IconArrowsSort`、`IconMessageCircle`）；`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（`sidebar.sort`、`sidebar.sortByLastUserMessage`、`sidebar.sortByUpdatedAt` 三语言）。未改 CSS，未新增依赖或后台活动。
- 门禁：`pnpm run typecheck` 通过；`pnpm exec vitest run` 145 文件 / 1425 项通过（`sidebarOrder.test.ts` 新增 5 项：默认按用户消息时间、后台更新不上浮、空时间兜底、拖拽固定优先、同值稳定排序与持久化校验；`useSessionSend.test.tsx` 新增 1 项断言：发送成功即以本地时间推进排序键）；`cargo test --manifest-path crates/keencode-resources/Cargo.toml -p keencode-resources` 全部通过。
- 后续修复（同日）：首版只在切换会话触发 `refreshSessions()` 时才更新排序键，因此旧对话发送消息后停在原位，直到导航才跳动。改为在发送成功点即时推进：`src/hooks/sidebar/useSidebarLists.ts`（新增 `markSessionUserMessage`，把该行 `lastUserMessageAt` 与 `updatedAt` 同时置为发送时间）、`src/hooks/session-turn/useSessionSend.ts`（`useSessionSend` 在 `applyMessagePrefixTitle` 之后调用）、`types.ts`、`useSidebarController.ts`、`App.tsx` 逐层透传。只改一条共享发送路径，草稿首条发送与编辑重发（同走 `executeSend`）一并覆盖；排序仍只用 `lastUserMessageAt`，`updatedAt` 仅在无用户消息时兜底。
- 门禁例外：`pnpm test` 在 `scripts/clean-room-source-gate.mjs` 阶段失败，报错文件为 `result.json`、`.zcode/plans/*.md`、`design-qa.md`、`src-tauri/prompts/README.md`、`src-tauri/src/providers.rs`，均为工作区既有未跟踪/并发产物，与本次改动无关。`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib` 有 3 项失败（`claude_hook_tests` 两项报 `hook_wait_failed`、`root_completed_artifact_final_message_cold_recovery_preserves_outcome` 报未收敛），单独重跑全部通过，属并行负载下的既有抖动。
- 基线：`f12ae5296b9ce8fa2dd1222d425c6245b0629953`。夹具 `baseline/` 由当前工作树复制后仅还原 `ProjectTree.tsx` 的排序入口（移除 `SidebarSortMenu` 导入、渲染与两个 props），用于隔离本次改动；基线 `ProjectTree.tsx` SHA-256 为 `7867c9f0898bf03a5dc82b7aa60fd27480f1fe7587399e85ad7898df2cd22875`，当前为 `299e159275afd33645085fac2905527d41c1d0bc69b620a79402fb2557b3db0c`；新增 `SidebarSortMenu.tsx` SHA-256 为 `cd50aa08ce09c35db2f2565089468d6b6c2f8cd8a79bec0833fa96e7a4bda864`，`sidebarOrder.ts` 为 `fcf750ec21ca5bbe8b925a77ea27a7dcbcde761ff64b1e06f3aabd64124829fd`。
- 夹具：`output/playwright/sidebar-sort-menu-20260917/`。`baseline/` 为改动前源码副本，`current/src` 通过符号链接指向工作树 `src/`，两变体共用 `node_modules` 但使用独立 Vite 依赖缓存。`QA_VARIANT=baseline node_modules/.bin/vite --config output/playwright/sidebar-sort-menu-20260917/vite.config.mts` 与 `QA_VARIANT=current ...` 分别起 `http://127.0.0.1:14411/`、`http://127.0.0.1:14412/`（14401/14402 已被另一夹具占用）；`node shoot.mjs` 截图与几何采集，`python3 compare.py` 比对。夹具用真实 `ProjectTree` + 合成项目/会话数据，不调用模型、不写用户会话。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，340×320，deviceScaleFactor=1。
- DOM 与几何：新增排序按钮位于 `.tree-l1__actions` 首位，`aria-label="对话排序方式"`、28×28、x=185、y=16；「收起全部项目」x=213、28×28 与「创建项目」x=241、28×28 几何不变（基线分别为 x=213、x=241）。`.tree-l1__head` 宽度由 203 收缩为 175，`.tree-l1`（8,16,263×28）、`.tree-l1__label`（15,20,26×20）、`.tree-l2`（8,46,263×32）与两条会话行（18,79/109,253×30）几何完全不变。展开菜单 `.cmm__dropdown-content` 为 31,50,182×96.19，label 为「对话排序方式」，两个选项文本为「最后用户消息时间」「更新时间」，默认选中「最后用户消息时间」（`data-state=checked`）。
- 像素：RGB 任一通道差值 >16 计入，未掩码。菜单关闭态差异 56/108800（0.051471%），范围 `[193,25,206,36)`，即新增排序图标字形所在区域，其余区域逐像素一致。两页 Console 均为 0 error、0 warning。
- 未验收：浏览器组件夹具不替代原生 Tauri WebView / Windows 实机验收；「发送消息 → 侧栏会话上浮」的可见动效未在桌面应用内实机复核。已覆盖的是组件级排序规则（`sidebarOrder.test.ts`）、发送点是否推进排序键（`useSessionSend.test.tsx` 断言）与 Rust 侧元数据计算（`keencode-resources` 测试）；后端 `last_user_message_at` 经 ACP `_meta` 到前端 `SessionListItem.lastUserMessageAt` 的解析由 `src/lib/acp/api.test.ts` 的 `_meta` 用例覆盖，但该时间在真实 Turn 落盘后回读的链路未端到端重跑。

---

# 2026-09-17 模型设置移除“导出全部”并把“导入”移到新增之后

- 需求：模型设置左栏不再提供“导出全部”，并把“导入”入口放到“添加提供商”之后。
- 修改：`src/components/ProvidersPanel.tsx`（删除左栏“导出全部”按钮与 `exportAllProviders`，导入按钮紧随“添加提供商”，复用既有 `.prov-transfer-row` 单按钮行容器保持 ghost 尺寸与左对齐）；`src/i18n/messages.ts`、`src/i18n/zh-tw.ts`（删除 `prov.exportAll` 三语文案）；`src/lib/api.ts`、`src/lib/providerTransfer.ts`（`providersExport` 收窄为必填 `providerId`，导出文件名不再接受空名称）；`src-tauri/src/lib.rs`、`src-tauri/src/providers.rs`（`providers_export` 与 `providers::export` 只接受具体供应商标识，删除全部导出分支）；`src/components/ProvidersPanel.test.ts`、`src/components/ProvidersPanel.test.tsx`、`src/lib/providerTransfer.test.ts` 同步断言。未改 CSS，未新增依赖或后台活动。
- 门禁：`pnpm run typecheck` 通过；`pnpm exec vitest run` 144 文件 / 1406 项通过；`cargo check --manifest-path src-tauri/Cargo.toml -p keencode-desktop` 通过；`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib providers` 47 项通过。
- 基线：`37218407a6ee8fa494371bc80f31b9463b490c6b`，`git show HEAD:src/...` 取出改动前的源码写入夹具 `baseline/src/`。基线 `ProvidersPanel.tsx` SHA-256 为 `ad7681f40b48b817b8a81a73bb10b803a9c5e60e6d2fb7082b4492c50ab8443a`，当前为 `9ef06f893aa0f8b8b06d208537c64b89d63ee8ceea71ecab0547964d3bfc6d4a`。
- 夹具：`output/playwright/providers-import-order-20260917/`。`baseline/` 为改动前源码副本，`current/src` 通过符号链接指向工作树 `src/`，两变体共用 `node_modules` 但使用独立 Vite 依赖缓存。`QA_VARIANT=baseline pnpm exec vite --config output/playwright/providers-import-order-20260917/vite.config.mts` 与 `QA_VARIANT=current ...` 分别起 `http://127.0.0.1:14411/`、`http://127.0.0.1:14412/`；`node shoot.mjs` 截图与几何采集，`python3 compare.py` 比对。夹具用合成供应商列表与内存 Tauri 命令桩，不读真实配置、不写用户数据。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1280×800，deviceScaleFactor=1。
- DOM 与几何：“添加提供商”两版均为 x=24、y=24、280×34；左栏按钮序列由 `[添加提供商, 导出全部, 导入]` 变为 `[添加提供商, 导入]`，导入按钮由 x=120 移到 x=24、y=68、46×28（与基线“导出全部”同一点位）；`.prov-transfer-row`（24,68,280×28）、`.prov-rail`（24,106,280×513）与首个供应商条目（24,106,278×63.27）几何完全不变。
- 像素：RGB 任一通道差值 >16 计入，未掩码。差异 819/1024000（0.07998%），范围 `[35,68,166,96)`，即被移除的“导出全部”按钮及其后方左移的“导入”按钮所在区域，其余区域逐像素一致。两页 Console 均为 0 error、0 warning。
- 未验收：浏览器组件夹具不替代原生 Tauri WebView / Windows 实机验收；未在桌面应用内实机点击导入并选择文件复核原生文件对话框链路。

---

# 2026-09-17 失败回合可见性（侧边栏终态标记 + 对话流失败标记）

- 问题：会话以 `turn_stopped{reason:"failed"}` 结束时界面没有任何可见痕迹，用户报告“没看到错误，但是自己停止了”。实测失败时若该会话不在前台，侧边栏只是转圈消失。
- 根因（已用真实 journal 事件跑前端归约 + 投影证实）：`src/hooks/acp-runtime/events.ts` 的终态副作用只对 `turn_completed` 打未读标记，`turn_failed` 只把状态改回 `ready`；且 `turnStatus`/`turnIncomplete`/`turnErrorKind` 全仓无 UI 消费方。对话流内的错误气泡（`turn_failed` → `view.last_error` → `mergeAcpTurnError`）本身是正常的，气泡带完整原文，因此本次不改该链路。
- 修改：`src/lib/sessionCompletion.ts`（未读状态由 `Set<string>` 改为 `Map<string, "completed"|"failed">`，存储键 `UNREAD_TERMINAL_RESULTS_KEY`，含旧值形状校验）、`src/hooks/acp-runtime/events.ts`（`turn_completed`/`turn_failed` 都产生未读结果，`turn_cancelled` 保持静默）、`src/hooks/acp-runtime/types.ts`、`src/hooks/useAcpSessionRuntime.ts`、`src/hooks/useSessionNavigation.ts`、`src/App.tsx`、`src/features/app/sidebar/{types,PinnedSessionList,HistorySessionList,ProjectTree,SidebarSessionRow}.tsx`（按结果渲染完成点或失败标记）、`src/components/lobe-chat/ConversationThread.tsx`（失败回合在对话流内复用 `EndOfTurnChip` + 既有文案 `endOfTurn.error`）；`src/i18n/messages.ts` 与 `src/i18n/zh-tw.ts`（新增 `sidebar.sessionFailedUnread` 三语言文案）；`src/styles/app-foundation.css`（`.tree-l3--unread-terminal` 取代 `.tree-l3--completed-unread`，新增 `.tree-l3__status--failed`）。未新增依赖或后台活动。
- 门禁：`pnpm run typecheck` 通过；`pnpm run lint:css` 通过；`pnpm exec vitest run` 144 文件 / 1406 项通过（新增 3 项：未读终态持久化与非法值拒绝、失败/完成终态产生未读结果、前台终态与主动取消不产生未读结果）。
- 基线：`37218407a6ee8fa494371bc80f31b9463b490c6b`，用 `git archive HEAD src public` 解压到夹具的 `baseline/` 目录。基线 `SidebarSessionRow.tsx` SHA-256 为 `fefdae0c683e42f7f971e9951ab7da2854663526fcb18c15b4f18b01f6bfc5e5`，当前为 `ce09df0e0556b1b483566f3045f5584eb0b86a7b2c6d76c2cc60506eb096a2b5`。
- 夹具：`output/playwright/failure-visibility-20260917/`。`baseline/` 为基线源码副本，`current/src` 通过符号链接指向工作树 `src/`，两变体共用 `node_modules` 但使用独立 Vite 依赖缓存。`QA_VARIANT=baseline pnpm exec vite --config output/playwright/failure-visibility-20260917/vite.config.mts` 与 `QA_VARIANT=current ...` 分别起 `http://127.0.0.1:14391/`、`http://127.0.0.1:14392/`；`node shoot.mjs` / `node shoot-thread.mjs` 截图与几何采集，`python3 compare.py` 比对。侧边栏夹具用 `?state=plain|completed|failed` 固定状态，仅渲染合成会话行；对话流夹具用合成消息（含一个 `turnStatus:"failed"` 与一个正常回合），两者都不调用模型、不写用户会话。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，侧边栏夹具 420×800、对话流夹具 1024×800，deviceScaleFactor=1。
- DOM 与几何：侧边栏会话行几何完全不变（x=8、y=8、263×30；`.tree-l3__name` x=18、高 30）。`?state=failed` 下当前版出现 `.tree-l3__status--failed`（x=242、y=11、24×24，`aria-label="已失败，点击查看错误"`、`dotCount=0`），基线版无该元素（`status=null`、`dotCount=0`）；`?state=completed` 下两版均为 `.tree-l3__status--completed`、`dotCount=1`、同一 `aria-label`，行类名由 `tree-l3--completed-unread` 变为 `tree-l3--unread-terminal`。对话流在失败回合后出现 1 个 `[data-testid=end-of-turn]`（`data-reason="error"`、文本“本轮以错误结束”、y=203、高 28.8），基线为 0 个。
- 像素：RGB 任一通道差值 >16 计入，未掩码。侧边栏 `?state=plain`（对照）与 `?state=completed` 均 0/84000 像素差异，证明改动未影响既有完成标记与普通行；`?state=failed` 差异 444/84000（0.528571%），范围 `[172,16,261,30)`，即新增失败标记所在区域。对话流同状态比较差异 13857/819200（1.691528%），范围 `[68,211,956,442)`；逐行核对确认首个差异行 y=211、chip 顶边 y=203 之上差异像素为 0，差异全部来自新增标记行及其下方随之位移的内容。
- 两页 Console 均为 0 error、0 warning。
- 未验收：浏览器组件夹具不替代原生 Tauri WebView / Windows 实机验收；真实会话中经 ACP 投递 `turn_failed` 触发侧边栏标记的端到端流程未在桌面应用内重跑（夹具直接渲染行组件，`latencyRecovery.test.tsx` 覆盖事件到未读结果的归约）。

---

# 2026-09-17 移除编辑重发的文件恢复复选框

- 需求：删除最后一条用户消息内联编辑器中的“同时恢复本轮及其子 Agent 修改的文件”复选框，并一并移除其背后的文件恢复能力。
- 修改：前端 `src/components/lobe-chat/ConversationThread.tsx`（删除 `Checkbox`、`Label` 与 `revertFiles` 状态，`onSend` 收敛为单参数）、`src/hooks/session-turn/useSessionEditResend.ts`、`src/hooks/session-turn/types.ts`、`src/lib/acp/api.ts`（rewind 请求与响应不再携带文件恢复字段）、`src/i18n/messages.ts` 与 `src/i18n/zh-tw.ts`（三语言文案）；后端 `crates/keencode-acp/src/protocol.rs`、`src-tauri/src/acp_host/extensions.rs`、`crates/keencode-resources/src/session_mutation.rs`（恢复计划、预检、应用与崩溃分类逻辑及其事务字段）、`crates/keencode-resources/src/atomic.rs`（只读原子替换原语随唯一调用方一并删除）。未改 CSS，未新增依赖或后台活动。
- 门禁：`pnpm run typecheck` 通过；`pnpm run lint:css` 通过；`pnpm exec vitest run` 144 文件 / 1394 项通过；`cargo test -p keencode-resources --tests` 全部通过，`-p keencode-acp`、`-p keencode-runtime`、`-p keencode-desktop --lib` 全部通过；`cargo check --workspace --all-targets` 与 `src-tauri` 检查均无警告。
- 基线：`2e881fe7^`（`c3dcad32e0a3d16d3e37c4b19cbb6ade4899fb45`，复选框仍在），用 `git archive c3dcad32 src public` 解压到夹具的 `baseline/` 目录。基线 `ConversationThread.tsx` SHA-256 为 `5b04c0c08b84d53739fbaf320bc177fb81f7da9bea1ef74386443058aaa6eedf`，当前为 `25eed44c65c02575163e08812a013f772dee5d00a2663b03af560823a698348b`。
- 夹具：`output/playwright/edit-resend-remove-20260917/`。`fixture.tsx` 自动点击“编辑并重新发送”进入编辑态；`QA_VARIANT=baseline` 加载 `baseline/` 源码副本，`QA_VARIANT=current` 通过符号链接加载工作树 `src/`，两变体使用各自独立的 Vite 依赖缓存。运行 `QA_VARIANT=baseline pnpm exec vite --config output/playwright/edit-resend-remove-20260917/vite.config.mts` 与 `QA_VARIANT=current ...`，地址分别为 `http://127.0.0.1:14381/`、`http://127.0.0.1:14382/`；`node shoot.mjs` 截图与几何采集，`python3 compare.py` 比对。夹具只使用合成消息与提交回调，不调用模型、不写用户会话。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1280×800 与 760×800，deviceScaleFactor=1。
- DOM 与几何：基线编辑器内 `[role=checkbox]` 为 1 个、label 文本为“同时恢复本轮及其子 Agent 修改的文件”，当前两者均为 0。1280 宽下编辑器 x=580、y=24、宽 504，高度由 182.390625px 回到 146px（与 2026-09-13 记录中复选框加入前的 146px 一致）；760 宽下 x=240、宽 504，页面 `scrollWidth` 等于视口宽，无横向溢出。
- 像素：RGB 任一通道差值 >16 计入，未掩码。1280×800 差异 23236/1024000（2.269141%），范围 `[580,120,1084,233)`；760×800 差异 23236/608000（3.821711%），范围 `[240,120,744,233)`。差异集中于被删除的复选框行及其下方随之位移的区域，其余区域逐像素一致。两页 Console 均为 0 error、0 warning。
- 已知影响：事务记录格式版本由 4 提升到 5、编辑请求摘要由 `v3` 提升到 `v4`，由旧版本写入的事务记录会被拒绝。实测把本机真实的 3 条 v4 记录复制到隔离存储根后调用 `recover_session_mutations`，返回 `SessionMutationRecoveryRequired("事务记录 schema 或版本无效")`；`RuntimeManager` 的列表与打开路径都会对每个项目目录调用该恢复，因此这些项目残留记录时会阻断会话列表与打开，需要删除已完成的墓碑记录。本机 `~/.keencode` 下另有 2 条更早的 v2 根级记录，改动前即已无法通过同一校验，不属于本次影响。
- 未验收：浏览器组件夹具不替代原生 Tauri WebView / Windows 实机验收；真实会话中经 ACP 的 rewind 往返未在桌面应用内重跑。

---

# 2026-09-17 提问卡片头部导航组贴右上角

- 问题：长问题把卡片头部撑成多行后，头部右侧的翻页/关闭导航组（`1 / 2`、`‹`、`›`、`✕`）落到问题正文的垂直中间，而不是停在右上角。
- 根因：`src/styles/app-conversation.css` 的共享规则 `.ask-user__header, .ask-user__nav, .ask-user__custom, .ask-user__footer { display: flex; align-items: center; }` 让 `.ask-user__header` 也吃到 `align-items: center`；该头部自身的规则块只覆盖 `min-height` 与 `gap`。`.ask-user__prompt` 是 `flex: 1` 且 `white-space: pre-wrap`，长问题折成多行后把头部撑高，30px 高的 `.ask-user__nav` 于是被居中到整块文本的垂直中点。单行问题时文本块与导航同为 30px 高，`center` 与 `flex-start` 无差别，因此只在长问题下暴露。
- 修改：`src/styles/app-conversation.css` 的 `.ask-user__header` 增加 `align-items: flex-start` 并加一行说明注释；未新增控件、依赖或其它样式改动。`src/lib/composerOverlayLayout.test.ts` 增加一条读真实 CSS 的回归断言。
- 门禁：`pnpm run typecheck` 通过；`pnpm run lint:css` 通过；`pnpm exec vitest run src/lib/composerOverlayLayout.test.ts src/components/AskUserModal.test.ts` 9 项通过。
- 浏览器夹具：`output/playwright/ask-user-header-20260917/`（`index.html`/`fixture.tsx` 加载当前源码与真实样式，`index-baseline.html`/`fixture-baseline.tsx` + `baseline-app.css` 把 `app-conversation.css` 换成 `git show HEAD:src/styles/app-conversation.css` 的副本，其余导入链同序）。问题正文取自用户截图场景，折成 5 行。`pnpm exec vite --config output/playwright/ask-user-header-20260917/vite.config.mts` 起 `http://127.0.0.1:14381/`，用本机 Chrome for Testing（`chromium-1228`）截图，deviceScaleFactor=1、浅色、中文、1440×900。
- 几何：卡片与问题文本块几何完全不变（`.ask-user` y=459、952×427；`.ask-user__prompt` y=474、770×189）。仅 `.ask-user__nav` 的 y 由 553.5（= 474 + (189-30)/2，垂直居中）变为 474（贴头部顶边），x=1041、140×30 不变。
- 像素：`baseline.png` 与 `after.png` 同状态比较，RGB 任一通道差值 >16 的像素 360/1296000（0.0278%），差异范围 `(1053,483)–(1172,576)`，即仅导航组移动区域，其余逐像素一致。
- 未验收：浏览器组件夹具不替代原生 Tauri WebView / Windows 实机验收；真实会话中由 Runtime 触发的问答流程未重启桌面应用复核。

---

# 2026-09-17 模型菜单“管理模型”入口

- 需求：模型选择器级联菜单底部新增分隔线与“管理模型”入口，跳转设置 → 模型设置。
- 修改：`src/components/ComposerModelMenu.tsx`（底部 `DropdownMenuSeparator` + `DropdownMenuItem`，复用既有 `onAddModel` 回调，新增 `labels.manageModels`）、`src/features/app/main/ComposerToolbar.tsx`（透传 `composer.manageModels`）、`src/i18n/messages.ts` 与 `src/i18n/zh-tw.ts`（三语言文案）。未改 CSS，未新增控件或依赖。
- 门禁：`pnpm run typecheck` 通过；`pnpm test` 的前端部分 `vitest run` 144 文件 / 1397 项通过（含更新后的 `ComposerModelMenu.test.tsx`）。`pnpm test` 整体仍失败于本次未改动的 `src-tauri/prompts/README.md` 与未跟踪 `.zcode/` 触发的 clean-room 来源门禁，已单独复现确认与本次改动无关。
- 浏览器夹具：`output/playwright/model-menu-manage-20260917/`（`index.html`/`fixture.tsx` 加载当前组件，`index-baseline.html`/`fixture-baseline.tsx` 加载 `5f3d6e1e` 的组件副本，两者模型目录与标签一致），`npx vite --config output/playwright/model-menu-manage-20260917/vite.config.mts` 起 `http://127.0.0.1:14371/`，用本机 Chrome for Testing（`chromium-1228`）截图，deviceScaleFactor=1、浅色、中文、900×600。
- 交互与几何：菜单项文本由 `["tokenharbor","opencode"]` 变为 `["tokenharbor","opencode","管理模型"]`；下拉高度 96.1875 → 133.25px，x/y/宽（24/60/196）不变；点击“管理模型”使 `onAddModel` 计数 1。
- 像素：`baseline-menu.png` 与 `after-menu.png` 同状态比较，RGB 任一通道差值 >16 的像素 919/540000（0.1702%），差异范围 `[7,139,237,210)`，其余区域逐像素一致。
- 未验收：浏览器夹具不替代原生 Tauri WebView / Windows 实机验收；未重启桌面应用复核真实设置跳转与深链。

---

# 2026-09-15 思考强度滑块四态动效

- 需求：对照 Droppy Code（gitlab.com/droppyformac1/droppy-code，本地克隆 /tmp/droppy-inspect/droppy-code）`EffortSlider.swift` 移植思考强度滑块的胶囊轨道与四态 Canvas 动效（plain/supercharged/fast/fusion）。拍板：品牌色固定 Claude 陶土橙；fast 态由面板内既有 Ultra 开关驱动（不新增控件、不改 Rust）；新建专用胶囊滑块组件（B2）+ CSS 玻璃旋钮近似（C1）。
- 修改：`src/lib/effortTrack.ts`（四态判定 + 动效纯规则，`fract(sin())` 哈希无状态粒子、速度线、双槽闪电、扫光、fusion 渐变/等离子/火花/呼吸缝核）、`src/components/ui/effort-slider.tsx`（Radix Slider 行为层 + 自绘视觉层，填充宽度 `inset + p*(W-2*inset)` 与 Radix thumb 边界公式等价）、`src/styles/effort-slider.css`（令牌化样式，零裸色）、`src/styles/tokens.css`（`--effort-brand/-ink`、`--effort-fast/-ink`、`--effort-spark` 六个新令牌）、`src/components/ComposerReasoningMenu.tsx`（换用 EffortSlider，Ultra 同时驱动 fast，标题按四态着色）。
- 有意差异（相对上游）：粒子辉光用 `shadowBlur` 替代 `ctx.filter`（Safari 18 才支持后者）；fusion 扫光只画一遍；品牌色不做 provider 映射（KeenCode providerId 为用户自定义字符串）；plain 填充用品牌暗色而非蓝色业务 accent；旋钮玻璃用 `backdrop-filter blur+saturate` 近似，无真实折射。
- 门禁：`pnpm run typecheck` 通过；`pnpm run lint:css` 通过；完整 `pnpm test` 143 文件 / 1384 项通过（含新增 `src/lib/effortTrack.test.ts` 14 项与改写的 `ComposerReasoningMenu.test.tsx` 3 项）。
- 浏览器夹具验收：`output/playwright/effort-slider-20260915/`（index.html + fixture.tsx，四个滑块分别固定 plain/supercharged/fast/fusion），`pnpm exec vite --port 14361` 后用本机 Chrome（无头）访问，截图 `four-states.png`。四行类名分别为 `effort-slider--plain/supercharged/fast/fusion`；每行 4 档位 1 旋钮；canvas 仅挂载 3 份（plain 不挂载）。
- 像素证据：动效画布实际点亮像素 supercharged=518、fast=3182、fusion=44402（devicePixelRatio=1 下画布 298×149 / 127×64），plain 无画布。reduced-motion（`reducedMotion: 'reduce'` 上下文）下画布挂载但 0 像素点亮。
- 几何断言：thumb 中心 y=88.5 = 轨道中心 y=88.5（修复了 Radix 包裹层 `top:0` 导致旋钮偏上 21px 的问题，thumb `top` 取根高一半）；键盘 ArrowRight 后 `aria-valuenow` 0→2、填充宽 127.3→212.7px；指针拖拽 supercharged 旋钮到最左后 `aria-valuenow=0`。
- Console：仅夹具自身的 React key 展开警告与 favicon 404，产品代码 0 error。
- 未验收：浏览器夹具不替代原生 Tauri WebView / Windows 实机验收；真实 Composer 面板中的布局（330px 宽下拉内）未截图，夹具宽度同为 330px 等效验证；触觉反馈（上游 NSHapticFeedback）Web 无等价物，以 240ms 下沉动画近似。

---

# 2026-09-14 编辑重发验收证据重建

- 原因：下方 2026-09-13 记录引用的 `/tmp/keencode-edit-resend-qa.4L4PyU` 截图与像素报告已不存在。本次重新生成可复核证据，没有修改产品源码。
- 源码：基线 `63e8f9546f6d52125fcb1b5608c6e4736d1c13c4`，当前 `cc62ce0ac762b4eb9791229fb923c10588f12803`；分别使用 `git archive <commit> src public` 重建到隔离目录，直接加载真实 `ConversationThread`、依赖组件与样式。组件 SHA-256 分别为 `ec9ff4c830dc04f59a641ef7fd192d1890c85454c6dddd843a5305b0ffcb666d`、`9ecc7db7f735d3ae81c203f24e33b57af30913b807b898f87e49c23f86c9d742`。
- 产物根目录：`~/.codex/artifacts/jian-edit-resend.90WTog`，包含 `before/`、`after/` 两份源码与夹具、`vite.config.mts`、`compare.py`、`pixel-differences.json`，以及 `before-editor.png`、`after-editor-default.png`、`after-editor-selected.png`、`after-editor-narrow.png`、`diff-editor.png`、`comparison-editor.png`。产物保留在系统临时目录外；仍是本地验收证据，不是发布资产。
- 复现：从仓库运行 `QA_VARIANT=before pnpm exec vite --config ~/.codex/artifacts/jian-edit-resend.90WTog/vite.config.mts`，另一个终端将变量改为 `after`；地址分别为 `http://127.0.0.1:14351/`、`http://127.0.0.1:14352/`。夹具只使用合成用户消息与提交回调，不调用模型或写入用户会话。使用已安装的 Playwright CLI 打开页面、悬停用户消息、点击“编辑并重新发送”；同状态截图后在产物目录执行 `python3 compare.py`。
- 环境：macOS 14.8.7、Chromium 153、中文、浅色，1280×800 与 760×800，deviceScaleFactor=1。Browser 插件及其 skill 未提供，因此按前端测试技能使用普通 Playwright CLI；没有安装新浏览器依赖。
- 页面检查：标题和 URL 正确，真实消息与编辑器正常渲染，无 Vite 错误覆盖层。初次基线夹具出现 favicon 404，添加空 data favicon 后修复；当前版本交互后的 Console 为 0 error、0 warning。
- 交互：基线无复选框；当前复选框初始 `aria-checked=false`，直接发送记录 `revertFiles=false`，重新打开、勾选后发送记录 `revertFiles=true`，两次成功后编辑器均关闭。再次打开恢复未勾选。桌面编辑器为 x=580、y=24、504×182.390625；760px 视口下 x=240、宽504，页面 `scrollWidth=760`，未观察到横向溢出或控件遮挡。
- 勾选证据复核：独立审查发现首次 `after-editor-selected.png` 与默认截图相同，不能证明选中状态；随后重新执行独立 `check` 操作，保存包含 `[checked]` 的 `after-editor-selected-dom.md`，再截图。新选中图 SHA-256 为 `af7dcab075edceb6773a4d64742fd08634e03530ae7a9dceb957ddfe48dc8907`，默认图仍为 `abced7757b66ffbcd5449c4f05a56eb70f70882f1ab979adfcab316bb91a851b`。同次浏览器执行再次读取 `aria-checked=true`，发送回调记录 `revertFiles=true`，发送后编辑器数量为0。
- 像素：相同 1280×800 编辑状态下，RGB 任一通道差值 >16 的像素为 23236/1024000（2.269140625%），未掩码，差异范围为 `[580,120,1084,233)`；差异集中在新增复选框、下移的发送操作和复制控件，原有编辑器宽度保持504px。有意新增区域与原需求一致。
- 未验收：浏览器组件夹具不代替原生 Tauri WebView 或 Windows 实机验收；后端文件恢复、取消与冷恢复仍由各自 Rust 门禁验证。

---

# 2026-09-13 编辑重发文件恢复选项

- 修改：最后一条真实用户消息的内联编辑器新增“同时恢复本轮及其子 Agent 修改的文件”复选框，默认关闭；直接发送传递 `revertFiles=false`，勾选后传递 `revertFiles=true`。复用现有 shadcn/ui `Checkbox`、`Label`、`Textarea`、`Button` 和原有设计令牌，没有新增 CSS、依赖或后台活动。
- 基线：HEAD `63e8f9546f6d52125fcb1b5608c6e4736d1c13c4` 的 `src/` 与 `public/`，通过 `git archive HEAD src public` 解压到隔离目录；基线 `ConversationThread.tsx` SHA-256 为 `ec9ff4c830dc04f59a641ef7fd192d1890c85454c6dddd843a5305b0ffcb666d`。当前工作树夹具直接加载真实修改后组件和真实样式。
- 夹具：`/tmp/keencode-edit-resend-qa.4L4PyU/harness/{index.html,fixture.tsx,vite.config.ts}`；基线和当前版本分别运行在 `http://127.0.0.1:14351/`、`http://127.0.0.1:14352/`，仅用本地合成消息与提交回调，不访问模型、不写用户会话。
- 环境：macOS 14.8.7、普通 Playwright CLI、Chromium 153，中文、浅色；像素对比为 1280×800、deviceScaleFactor=1，另检查 760×800 窄视口。Browser 插件未提供，因此按前端测试技能使用普通 Playwright。
- 行为结果：修改前编辑器不存在 checkbox；修改后可访问性快照显示复选框默认 `aria-checked=false` / `data-state=unchecked`。不勾选发送得到 `{messageId:"user-last",revertFiles:false}`；重新打开、勾选并发送得到 `{messageId:"user-last",revertFiles:true}`，成功后编辑器关闭。1280 宽下编辑器宽度保持 504px，高度由 146px 增至 182.398px；760 宽下编辑器位于 x=223–727，页面 `scrollWidth=760`，无横向溢出。两页最终 Console 均为 0 error、0 warning。
- 像素结果：RGB 任一通道差值 >16 计入；修改前后同状态编辑器差异为 23167/1024000 像素（2.262402%），未掩码，边界 `(579,395)–(1083,508)`，集中于新增复选框、编辑器下边界和下移的操作区。
- 产物：`/tmp/keencode-edit-resend-qa.4L4PyU/{before-editor.png,after-editor-default.png,after-editor-selected.png,after-submitted-true.png,after-editor-narrow.png,diff-editor.png,comparison-editor.png,pixel-differences.json}`。临时目录会被系统清理，不作为发布产物。
- 未验收：没有启动、重建或控制原生 Tauri WebView，因此浏览器组件夹具不替代 macOS 原生桌面验收；没有 Windows 实机验证。后端真实工作区恢复、冲突关闭和冷恢复由 Rust 测试覆盖，本夹具只验证可见组件与布尔值交互边界。

---

# 2026-09-10 设置页中的更新弹窗可见性

- 问题：在设置页点击“查看进度／安装并重启”后看不到更新弹窗，必须切回对话页才出现。
- 根因：`App.tsx` 的视图三元分支把 `AppUpdateModal` 和 `AppDialogPortal` 放在工作台 Fragment 内，`appView === "settings"` 时两者被卸载。
- 修改：只把更新进度和安装确认浮层移到视图分支之外；`ShortcutsModal`、`SessionSearchPortal` 及其他工作台浮层仍留在工作台分支，全局按键监听在设置页和启动页直接返回。
- 基线：HEAD `82f91985cf643550ec085836a4eb0c94888a460d` 的 `src/App.tsx`，通过 `git show HEAD:src/App.tsx` 在同一夹具中切换对比。
- 夹具：`output/playwright/update-modal-view-20260910/{index.html,fixture.tsx}`，加载真实 `App`、真实样式与真实更新组件，仅用本地合成响应替换 Tauri IPC（`app_update_info` 返回 downloading、`app://update-status` 事件可注入），不访问网络、不写用户数据。运行 `pnpm exec vite --port 14341 --strictPort`，访问 `http://127.0.0.1:14341/output/playwright/update-modal-view-20260910/index.html`。
- 环境：macOS 14.8.7、Chromium 149（`Google Chrome for Testing`，无头、CDP 驱动）、1280×900、deviceScaleFactor=1、浅色、中文。
- 行为结果（修复前 → 修复后）：设置页 `#/settings/about` 点击“查看进度”后 `.app-update-progress` 节点数 0 → 1；下载完成点击“安装并重启”后 `.app-dialog` 节点数 0 → 1，标题“安装更新？”；设置页按 `Cmd+/` 和 `Cmd+K` 均不显示对话快捷键面板，也不拦截为 KeenCode 对话动作；工作台中的两个快捷键入口保持可用。
- 产物：`before-settings-page.png`、`after-settings-page.png`、`diff-settings.png`、`before-workbench-page.png`、`after-workbench-page.png`、`diff-workbench.png`。
- 像素结果：1280×900=1152000，RGB 任一通道差 >16 计入。对话页前后完全相同（SHA-256 前 16 位均为 `5b28b1bd5daf8cc3`，0 像素差），确认没有改动既有工作台视图；设置页 1060129 像素差（92.02509%），边界 `(0,0)–(1280,900)`，来自居中弹窗与其模糊遮罩。
- 自动验证：`pnpm run typecheck` 通过；完整 `pnpm test` 的 37 项 Node 门禁及 138 个文件 / 1272 项 Vitest 通过。新增契约测试 `src/App.contract.test.ts` 的“应用级浮层视图边界契约”，断言更新进度和安装确认跨视图挂载、快捷键帮助和会话搜索只在工作台挂载，并要求按键监听显式限制为工作台。
- 未验收：本机已安装的正式版 `KeenCode.app` 仍在运行且未重启替换，未做原生 WebView 截图；未做 Windows 实机验证。浏览器夹具使用合成 IPC 响应，不能替代真实更新下载与安装流程。
- 资源影响：只调整 JSX 位置和现有按键监听守卫，无新增依赖、CSS、后台任务或轮询。

---

# 2026-09-10 子 Agent 统计、事件驱动斜杠菜单与图片工具行

- 后续门禁验证：按用户要求排除根 `docs/` 的路径和正文扫描后，完整 `pnpm test` 已通过（37 项 Node、133 个文件 / 1255 项 Vitest）。下方门禁阻断记录保留为当时事实。
- 基线：`39c1117d` 加本轮开始前已有的压缩时序修改；本轮修改前 `src/`、`public/` 快照为 `/tmp/keencode-agent-ui-p8kDrw/before-source.tar.gz`，SHA-256 `ab713ec718626307206557128b05ff8eae97715e83b1e6f01db906c671c0e28b`。保留既有 Rust 修改，未提交、推送或重启正式版。
- 实现：子 Agent 每个 Turn 独立归约用量、配对 TPS 和计时；续跑分开消息，冷回放不伪造首 Token。删除斜杠菜单永久 rAF，复用编辑器 input、IME、MutationObserver 与光标事件，并补齐失焦时外部清空草稿的主动同步。压缩改为 26px 工具状态行；相邻图片结果显示可折叠缩略图组，复用 ImageUi、ImageViewer、Collapsible、Button 与现有令牌。
- 图片读取：时间线只保存工具实际返回的不可变 Artifact 引用；按需走二进制 IPC，校验 Session 归属、图片类型、25 MiB 上限、大小和摘要，不回退工作区原文件。查引用只复制小型引用，不为每张图克隆完整会话；折叠卸载缩略图并释放 Blob，没有新增依赖或后台服务。
- 浏览器夹具：`/tmp/keencode-agent-ui-p8kDrw/fixture.tsx`，直接加载真实组件、归约器、投影和样式；仅图片二进制 IPC 使用本地合成响应。运行 `KEENCODE_QA_BASELINE=1 pnpm exec vite --config /private/tmp/keencode-agent-ui-p8kDrw/vite.config.ts --port 14331` 和不带该变量的 `--port 14332`；分别访问两个回环地址的 `/`、`/?light`、`/?control`。未调用模型、未写用户数据。
- 环境：macOS、Chromium、中文，主对话和子 Agent 并列的同一完成状态。像素比较为 1100×900、deviceScaleFactor=1，另检查 760×900 窄视口无横向溢出；两组缩略图均 80×80、复用 16px 圆角。截图、差异图在同一临时目录：`before/after-dark.png`、`before/after-light.png`、`before/after-control.png`、`after-narrow.png`。
- 像素结果：RGB 任一通道有差异即计入，未掩码。无压缩/图片的控制场景为 0 像素差；深色 72656/990000（7.339%）、浅色 59360/990000（5.996%），差异边界均为 `(40,275)–(1060,483)`，限于有意替换的压缩/工具/图片区域及其后正文位移。
- 实际交互：`/`、中文查询、Escape 后同查询不重开、修改查询后重开、外部清空、无 input 的 DOM 修改、模拟 IME 预编辑和确认均通过。基线空闲 5 秒读取输入框 301 次，修改后 0 次；输入后再次空闲也为 0。该指标仅证明移除了这条扫描循环，不代表正式版整体 CPU 或内存达标。
- 图片组折叠后图片节点由 2 变 0、释放 2 个 Blob；Enter 展开与打开大图，方向键切图、Escape 关闭并恢复缩略图焦点通过，大图退出另释放 2 个 Blob。`image-viewer.png`、`child-usage.png`、`child-time.png` 记录交互；合成子 Agent 弹层显示总量 140、输入 100、输出 40、推理 10、缓存读取 80、TPS 40、首 Token 11 秒。最终页面 Console 为 0 error、0 warning。
- 正式记录离线核对：`node /tmp/keencode-agent-ui-p8kDrw/replay-metrics.mjs` 只读取所报 Session 的 snapshot，按当前后端字段映射输入真实前端归约器；62 次请求恢复总量 4830101、输入 4730891、输出 99210、推理 57220、缓存读取 4614656、总用时 1080032ms、TPS 249.8124078。历史首 Token 无持久证据，保持未知；这不等于正式 App 已重新加载修复。
- 验证：类型检查和全部 133 文件 / 1255 项 Vitest 通过；Runtime 全部 102 项、Resources 全部 44 项、桌面工具投影 3 项通过。样式检查覆盖 `src/styles/*.css` 与 `src/components/lobe-chat/lobe-chat.css`，差异检查通过。统一来源门禁仍被既有 `docs/current-system-prompt.zh-CN.md:114` 的来源说明阻断，未修改该文档或绕过规则。
- 未验收：原生 UI 控制接口禁用，本轮只完成浏览器组件级交互和后端测试，没有完整桌面壳层/原生 WebView、系统真实 IME 或 Windows 实机验收。临时夹具与截图位于 `/tmp`，会被系统清理，不能作为发布验收的持久产物。

---

# 2026-09-10 压缩卡片按实际时序显示

- 基线：`39c1117d` 的 `src/`、`public/`；归档 `output/diagnostics/compaction-performance-20260910/before-source.zip`，SHA-256 `9935b12d60cfc644df94ba06675525bcc73a243e055170cb2d560d4e1bd353b0`。原有未提交修改仅在 Rust，不影响前端基线。
- 修改：压缩完成事件加入当前 Agent 的有序片段，随本轮整体固化；复用原有卡片 DOM、样式和设计令牌。保留单一整轮计时、事件重放顺序、多次压缩与子 Agent 隔离，并同步轨迹台账和子 Agent 摘要的片段处理。
- 浏览器夹具：`output/playwright/compaction-20260910/index.html`、`fixture.tsx`，直接复用真实事件归约器、投影、ConversationThread 和项目样式。运行 `pnpm run dev --host 127.0.0.1`，访问 `http://127.0.0.1:1421/output/playwright/compaction-20260910/index.html`。仅使用合成协议数据，无 Tauri 写操作。
- 环境：macOS Chromium，1100×820，deviceScaleFactor=1，浅色、中文，同一完成回合。`before.png` 为卡片位于首句之前，`after.png` 为“第一阶段 → 工具 → 压缩卡片 → 第二阶段”。两者工作耗时均为 6 秒。
- 像素比对：RGB 任一通道差 >16 的像素 11830 个（1.31153%），差异边界 `(116,138)–(984,341)`；`diff.png` 记录有意的卡片位移与随之改变的上下间距。无压缩控制场景 `?no-compaction` 的 `control-before.png` 和 `control-after.png` 字节及像素完全相同，SHA-256 均为 `9249b2aa186946036a1ff15023cc2add57f98156a2ef514f976ab67299127aab`。
- 针对性验证：158 项 Vitest 与类型检查通过，覆盖实时/取消/完成/冷重放、多次压缩、延迟工具更新、工具阶段分隔、子 Agent 隔离、轨迹和摘要。浏览器仅出现夹具缺少 favicon 的 404，没有应用运行时错误。
- 完整验证：独立执行 `pnpm exec vitest run --reporter=dot`，131 个文件、1244 项测试通过。`pnpm test` 的 36 项 Node 测试通过，但随后被现有 `docs/current-system-prompt.zh-CN.md:114` 的来源说明挡在来源门禁；该行已确认存在于修改前的 HEAD，未修改或绕过门禁规则，不能称统一入口通过。
- 原生桌面验收未执行：没有重启或替换正在运行的正式版，没有把浏览器结果当作原生验收；正式版资源占用只读采样另见 `output/diagnostics/compaction-performance-20260910/资源占用排查.md`。

---

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


# 2026-09-11 历史分页前插与滚动位置

- 基线 Git：0a8654ae7765736b6e0d56a9289895dfe5e0ab95；对应虚拟列表 hook 保存为 output/playwright/history-pagination-20260911/before-virtualizer.ts。前后均复用当前真实 ConversationThread、CSS 与合成数据，仅基线替换 hook；本次无 DOM/CSS 样式修改。
- 环境：macOS、HeadlessChrome 153.0.0.0，1100×900、deviceScaleFactor 1。产物目录 output/playwright/history-pagination-20260911/ 包含 fixture.tsx、vite.config.ts、check.js、results.json 和截图。
- 复现基线：KEENCODE_QA_BASELINE=1 pnpm exec vite --config output/playwright/history-pagination-20260911/vite.config.ts --port 14351；修改版去掉环境变量并使用 14352。通过 Playwright CLI 的 run-code 执行 check.js。
- 场景：100 条长列表和 12 条短列表，非吸底阅读时前插 10 条。基线锚点分别从 y=38/44 移至 868/874，修改版分别保持 y=38/44，偏移均从 830 px 降至 0。
- 相同初始状态截图：before/after-{large,small}-initial.png，两组均 0/990000 像素不同（RGB 任一通道不同即计数，无 mask）；前插截图为 before/after-{large,small}-prepended.png，差异图为 diff-{large,small}-initial.png。
- 类型检查及 146 项相关前端测试通过。浏览器日志仅 favicon 404 与开发提示，无业务错误。未取得同状态原生 macOS WebView 前后截图，也未量化真实点击耗时；浏览器坐标与像素验证不能替代原生验收。
# 2026-09-15 压缩状态时间线验收缺口

- 基线：提交 `3d88b171` 的 `src/` 和 `public/` 可通过 `git archive 3d88b171 src public` 在隔离目录重建；当前工作区改动在 `src/lib/acp/store.ts`、`src/components/lobe-chat/ConversationThread.tsx` 和文案文件。用户提供的截图仅显示多次“已自动压缩”，没有相同会话状态下的压缩开始帧，不能作为前后像素基线。
- 已验证：合成事件 `context_compaction_started` → `context_compaction_completed` / `context_compaction_failed` / `turn_cancelled` 的时间线投影测试和组件渲染测试；未取得原生桌面窗口同状态、同视口及 deviceScaleFactor 的前后截图。当前原生计算机控制 API 禁用，不能完成本次原生像素差异检查；浏览器或历史截图不能替代。
- 待验收：分别用上述基线与当前版本在隔离开发桌面进程运行，保持 macOS、中文、浅色、同视口和 deviceScaleFactor，用同一合成压缩事件序列截图并比较像素；再在真实长会话确认开始提示及时出现、完成后原位更新，且不会每轮反复压缩。


# 2026-09-17 供应商限额提示可见性与标题租约释放

- 需求：1) 供应商额度耗尽（如 GLM HTTP 429 / 业务码 1308「已达到 5 小时的使用上限…重置」）时，重试原因必须直接可见，不能只放在悬停 `title` 与无障碍名称里；2) 后台自动标题请求在额度耗尽后长时间重试时，不得继续独占 Session Runtime 租约，否则随后的 `keencode/session/rewind`、`session/load` 会报「Session Runtime 正被另一个进程或句柄占用」，并在前端折叠成 `ACP 请求失败`（-32603）。
- 根因：`src-tauri/src/acp_host/extensions.rs` 的 `dispatch_generate_title` 把授权句柄 `_session` 持有到标题网络等待结束；`AgentRuntime::generate_title` 也在等待模型前调用 `runtime_manager.get()` 并在整个超时窗口内保持句柄。关闭会话只移除投递世代，不等待标题任务退出，租约因此被占用到标题请求超时。
- 修改（后端）：`src-tauri/src/acp_host/extensions.rs` 授权校验后立即 `drop` 句柄；`src-tauri/src/agent_runtime.rs` 把 `title_generation_gates` 的值由裸 `Mutex<()>` 改为 `TitleGeneration { gate, cancellation }`，句柄改为在取得 gate 之后获取，标题请求包在 `tokio::select!` 中与 `cancellation.cancelled()` 竞争；`close_session_delivery` 拆出持锁内部函数 `close_session_delivery_locked`，移除标题任务记录后取消并等待 gate 释放；`close_session`/`shutdown_session` 在既有 `delivery_reset_gate` 内完成投递关闭与租约释放，避免关闭期间新请求重新登记。
- 修改（前端）：`src/components/lobe-chat/ConversationThread.tsx` 的 `RetryStatus` 把限额原因渲染为可见正文（`<br/>` + 原因文本），倒计时单独标记 `aria-hidden`，避免实时区域每 100ms 变更被反复播报；`src/components/lobe-chat/lobe-chat.css` 的 `.lobe-chat-retry-status__label` 由单行省略号改为可换行（`white-space: normal` + `overflow-wrap: anywhere`），长限额文案不再被截断。
- 回归测试：`src-tauri/src/agent_runtime.rs` 新增 `title_generation_close_releases_lease_without_caching_stale_result`（模型永不返回时 `close_session` 必须在有界时间内成功、重开成功、旧标题不写入缓存）；`src/components/lobe-chat/ConversationThread.test.tsx` 新增「将供应商限额和恢复时间显示为正文」并断言 `__label` 换行规则。
- 验证：`cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop` 有 641 passed / 0 failed 的一次完整运行；另两次全量运行各命中 1 个既有 flaky（`agent_runtime::tests::shutdown_is_idempotent`、`extensions::runtime_contributor::claude_hook_tests::cancelled_session_start_late_success_cannot_complete_reloaded_candidate`）。前者隔离复跑 30 次失败 2 次，其 `send_command` 回执分支与 HEAD 逐字一致且用例只经过 `SessionDeliverySender`、不经过本次改动路径，判为既有竞态，未纳入本次范围。相关前端 137 项通过，`pnpm run lint:css` 通过；`pnpm run typecheck` 仅余 2 个既有 `closeToTray` 错误（另一会话未完成改动）。
- 视觉对比：基线取 `output/playwright/provider-limit/before-source.tar.gz`（`src/` 快照，追溯提交 `d60beaa1`）叠加本次两个前端文件，对照为「快照 `src/` + 本次两个文件」；夹具 `output/playwright/provider-limit/{baseline,patched}/fixture-retry.tsx` 只渲染真实 `ConversationThread` 与合成重试状态（`reason` 为上述限额文案），两变体共用 `node_modules`、各自 `cacheDir`，端口 14395/14396，`node shoot-retry.mjs` 截图并采集几何，`python3 compare-retry.py` 比对。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，1024×420，deviceScaleFactor=1。
- DOM 与几何：基线 `.lobe-chat-retry-status` 可见文本仅为「正在进行第 2/10 次请求尝试 · 3s」，原因只存在于 `title` 与 `aria-label`；对照可见文本包含完整限额文案，标签盒高 17.55 → 35.09px、容器高 24 → 35.09px，两变体 `scrollWidth - clientWidth` 均为 0（无溢出截断）。
- 像素：RGB 任一通道差值 >16 计入，未掩码。差异 3843/430080（0.893555%），范围 [68,197,472,228)，即重试提示行本身；两页 Console 均 0 error、0 warning。
- 未验收：浏览器夹具不替代原生 Tauri WKWebView 实机验收；未在真实桌面会话里用额度耗尽的供应商复跑「标题请求挂起 → 关闭/重开会话」端到端链路（Rust 回归测试覆盖同一路径）。提交只包含本次 hunk，工作区另有并发会话 WIP（水位提示移除、`--ui-font-delta` 字号跟随、`batch_failure_kind` 等）未纳入。


# 2026-09-18 已工作耗时支持小时与天单位

- 需求：「已工作/工作中 {duration}」以及「持续了 {duration}」的耗时在超过 1 小时后仍只显示分钟（如「120分钟」），需要增加小时与天两级单位换算。
- 修改：`src/components/lobe-chat/Thinking.tsx` 的共享格式化函数 `formatProcessingDuration` 由分/秒两级扩展为天/小时/分/秒四级紧凑展示，三语言（`en`/`zh`/`zh-TW`）分别输出 `1d 1h`、`1天 1小时`、`1天 1小時`；`ConversationThread.tsx` 与 `TimelinePhaseBlock.tsx` 的「工作中/已工作/持续了」标签均复用该函数，一处修改全链路生效。未改动 DOM 结构、CSS 或 i18n 文案。
- 测试：`src/components/lobe-chat/Thinking.test.tsx` 新增中文/英文天与小时档位断言（`3_660_000ms → 1小时 1分钟 / 1h 1m`，`30_000_000ms → 8小时 20分钟 / 8h 20m`，`90_000_000ms → 1天 1小时 / 1d 1h`，zh-TW 同步覆盖）；`pnpm exec vitest run` 聚焦 3 个相关文件 47 项通过，`pnpm run typecheck` 通过。
- 未验收：与既往限制一致，未取得原生 macOS WKWebView 同状态前后截图做像素比对；本次仅改文本内容与字符宽度（按钮宽度自适应），无布局/样式/交互变化，浏览器级 SSR 断言与类型检查不足以替代原生桌面验收。


# 2026-09-18 右侧面板内置浏览器（地址栏与多网页标签）

- 需求：右侧面板可直接查看对话里出现的 URL 与本地 HTML 全路径，不切到外部浏览器；需要地址栏，且能同时打开多个网页并切换（首选每个标签各自持有一个网页）。
- 修改（Rust）：新增 `src-tauri/src/browser.rs`，提供 `browser_open/bounds/show/hide/close/navigate/reload/history` 八个命令，并在 `src-tauri/src/lib.rs` 注册。每个网页标签对应一个独立原生子 WebView（label 前缀 `browser-`），切换标签只切显隐，因此滚动位置、表单内容与前端路由在切换后保留；`browser_open` 为 `async` 命令，避免 Windows 上同步命令内创建 WebView 死锁；导航与标题变化经 `browser://state` 事件回写前端。
- 修改（前端）：`src/components/EmbeddedBrowser.tsx` 重写为带地址栏的网页标签（后退/前进/刷新/地址栏/外部打开）；`src/lib/browserTabs.ts` 归一化地址栏输入（http(s) 直连、主机名补全协议、本地绝对路径交给文件预览链路、拒绝脚本协议），`src/lib/browserWebview.ts` 按标签串行化命令，`src/hooks/useCoveringOverlay.ts` 按几何遮挡判断原生子 WebView 是否需要让位；`ResourceViewer.tsx` 常驻挂载全部网页标签并新增「新建网页标签」入口；三语言文案与 `app-resource.css` 同步更新。
- 安全修正（本次引入并已修复）：Tauri 为每个 WebView 注册 `asset://` 协议，且把 `Access-Control-Allow-Origin` 设为该 WebView **创建时**的来源。若直接用目标地址创建子 WebView，远程页面即可读取 `assetProtocol.scope`（当前含 `$HOME/**`）。现改为用 `about:blank` 创建（来源为 `null`）再导航到目标地址，并在 `browser.rs` 注释中记录该约束与原因。
- 测试：`src/lib/browserTabs.test.ts` 新增 8 项（协议补全、回环地址用 http、本地路径与 `file://` 还原、脚本协议拒绝、标签名派生）；`src-tauri/src/browser.rs` 新增 3 项单元测试（label 派生与非法字符替换、http/https 与空白页放行、`file:`/`javascript:`/`data:`/`ftp:` 拒绝）。
- 验证：`pnpm run typecheck`、`pnpm run lint:css`、`pnpm exec vitest run`（147 文件 / 1445 用例全通过）、`cargo check`、`cargo test -p keencode-desktop --lib browser::`（3 通过）均通过。
- 未验收：未取得原生 macOS WKWebView 实机证据。核心风险（原生子 WebView 能否真正创建、与主界面的层叠与坐标是否准确）只能由实机确认，本会话无法驱动原生 UI，且用户已有 KeenCode 实例占用同一 `~/.keencode` 数据目录。实机复现：设置 `KEENCODE_BENCHMARK=1`、`KEENCODE_BENCHMARK_DATA_DIR=<隔离目录>` 后运行 `pnpm dev:desktop`，在右侧面板点「新建网页标签」，验证地址栏导航、多标签切换后页面状态保留、下拉菜单/弹窗出现时网页正确让位，以及 `file:///…/x.html` 由文件预览链路打开。本次未做像素对比，也未量化多网页标签的内存增量。


# 2026-09-18 模型选择器显示会话实际供应商

- 问题：首次使用某个模型后，后续切换新模型时新模型已生效，但模型选择器的触发器与下拉勾选仍显示旧供应商。触发场景为首个模型命中供应商限额（HTTP 429）后切换到另一供应商的同名模型。
- 根因（用本机真实日志与 journal 证实）：Composer 只保存模型 ID，供应商仅在 `session/set_config_option` 成功后写入 `modelBySessionRef`（`providerId::modelId`），随后恢复、`config_option_update` 回执与打开会话等路径都用 `modelIdFromSessionReference(...)` 把引用截断成模型 ID；显示层再按全局活跃供应商（`activeCustomProvider`）解析 `ModelOption`。当同一模型 ID 存在于多个供应商时，显示便回退到全局活跃供应商。本机 `~/.keencode` 复现条件：`workbuddy` 与 `workbuddy-ai` 都提供 `deepseek-v4.1-flash`；会话 `session-621c24e7…` 的 journal 在 10:40:48 → 10:40:53 记录 `provider_snapshot_updated` 由 `de08ae2a…`（Workbuddy）切到 `db970eaf…`（WorkBuddyAI），`model-request-records.jsonl` 同一时刻有 429「您的使用量已超出频率限制…您也可以切换其他模型继续使用」，与用户描述的触发条件一致。
- 修改（前端，全部落在共享汇聚点）：`src/lib/modelCatalog.ts` 新增 `findActiveModel`（按会话供应商精确匹配；供应商已知但目录不再列出该模型时不再借用同名条目）、`formatSessionModelReference`、`isBoundSessionModelReference`（模块内把 Host 未绑定 Provider 的 `unconfigured` 占位值按“未选择”处理，不当作模型名展示）；`src/hooks/useProviderModels.ts` 状态由 `modelId` 改为完整引用 `sessionModelReference`，对外提供 `sessionProviderId` 与 `setSessionModelReference`；`src/hooks/acp-runtime/{history,events,types}.ts`、`src/hooks/useAcpSessionRuntime.ts`、`src/hooks/useSessionNavigation.ts`、`src/App.tsx` 把端口与调用点统一改为传递完整引用；`src/features/app/main/ComposerToolbar.tsx` 与 `src/components/ComposerModelMenu.tsx` 按会话供应商解析与显示，切换时先落地含供应商的本地引用。未改动 CSS、DOM 结构与运行时协议；发送门槛保持按全局默认供应商判断，避免会话供应商被删除时静默禁用发送按钮。
- 回归测试：`src/lib/modelCatalog.test.ts` 新增引用往返、同名模型按供应商精确匹配、占位值断言（23 项）；`src/components/ComposerModelMenu.test.tsx` 新增「同名模型属于多个供应商时显示会话自身供应商」；`src/hooks/acp-runtime/history.test.tsx` 断言恢复把完整引用 `fix-local::hy3` 交给 Composer；`src/hooks/useSessionNavigation.test.tsx` 同步断言。
- 验证：`pnpm run typecheck` 通过；`pnpm exec vitest run` 147 文件 / 1450 用例全通过。
- 视觉对比：夹具 `output/playwright/model-menu-session-provider-20260918/`。`baseline/ComposerModelMenu.tsx` 由当前源码仅回退本次解析 hunk 生成（基线按其原接线传全局活跃供应商 `workbuddy`），`catalog.ts` 提供两家网关的同名模型目录，两页共用同一份 `src/styles` 与 vite 配置（`pnpm exec vite --config output/playwright/model-menu-session-provider-20260918/vite.config.mts`，`http://127.0.0.1:14391/`），`node shoot.mjs` 截图并采集几何，`python3 compare.py` 比对。
- 环境：macOS 14.8.7、Chrome for Testing（`chromium-1228`）、中文、浅色，900×640，deviceScaleFactor=1。
- DOM 与几何：基线触发器为 `Workbuddy/deepseek-v4.1-flash`，下拉中 `Workbuddy` 是当前供应商（`active` 与 `checked` 均为 true），`WorkBuddyAI` 两项均为 false；当前版触发器为 `WorkBuddyAI/deepseek-v4.1-flash`，`WorkBuddyAI` 是当前供应商，`Workbuddy` 两项均为 false。
- 像素：`baseline-menu.png` 与 `current-menu.png` 同状态比较，RGB 任一通道差值 >16 的像素 11237/576000（1.950868%），差异范围 `[29,32,247,152)`，即触发器与下拉供应商行本身；两页 Console 均 0 error、0 warning。
- 未验收：浏览器夹具不替代原生 Tauri WKWebView 实机验收；未在真实桌面会话中复跑「首个模型 429 → 切换到另一供应商同名模型」的端到端链路（本机日志已证实该切换序列，前端投影由上述单测覆盖）。夹具首次运行曾因基线页从另一夹具导入数据而重复挂载（下拉出现两组供应商），抽出 `catalog.ts` 后重采。


# 2026-09-19 模型设置导入按钮移至表单头部右上角

- 需求：模型设置页的供应商导入按钮原在左栏「添加提供商」按钮下方，位置错误；应移至右侧添加供应商面板（表单头部）的右上角。
- 修改：`src/components/ProvidersPanel.tsx` 移除左栏导入按钮及其单按钮行容器；`prov-form__head` 的 `prov-transfer-row` 改为常驻并固定包含导入入口（`IconPush` + `tr("prov.importAll")`），编辑态在其前保留当前供应商的复制与导出；`submitImport` 删除随之不可达的空态分支（导入入口仅存在于表单可见的 create/edit 模式，导入弹窗阻塞背景交互，提交时 `rightMode` 不可能为 `empty`）。未新增 CSS，复用 `prov-form__head` 既有 space-between 布局与 ghost 按钮变体。
- 测试：`src/components/ProvidersPanel.test.ts` 布局锁定断言由「左栏在新增入口之后提供导入入口」改为「导入入口固定在表单头部右上角」（head 含 `prov.importAll`，rail 不含导入与导出全部入口）。
- 验证：`pnpm run typecheck` 通过；`pnpm exec vitest run src/components/ProvidersPanel.test.ts` 2 文件 14 项通过。
- 未验收：未取得原生桌面同状态前后截图做像素比对（本会话无法驱动原生 Tauri UI；非 Tauri 模式下供应商列表为空，右侧面板仅渲染空态，无法在浏览器夹具复现添加供应商表单状态）。本次为既有控件的位置迁移，未改动颜色、字体、间距等设计令牌。


# 2026-09-19 供应商列表定高内部滚动

- 需求：模型设置页供应商数量不限增长，左栏列表随供应商数量无限撑高页面；需要固定高度、溢出时以滚动条滚动的方式查看供应商。
- 根因：`.prov-rail` 早已具备 `flex: 1; min-height: 0; overflow: auto` 的内部滚动机制，但宽屏布局从未给列容器限高，栅格行高由内容驱动，滚动从未生效；窄屏断点（≤860px）已有 `.prov-split__list { max-height: 280px }` 先例，宽屏缺少同样的约束。
- 修改：`src/styles/app-resource.css` 宽屏基础 `.prov-split__list` 增加 `max-height: 560px`（窄屏 280px 覆盖保持不变），列表区域高度不再随供应商数量增长；`.prov-rail` 加入设置页静默滚动条例外组（`scrollbar-width: thin`，悬停/聚焦时出现 `--scrollbar-thumb` 细滚动条），与 `.settings-page__nav-inner`、`.settings-page__content` 共用同一组规则。未改动 TSX 结构与设计令牌。
- 测试：`src/components/ProvidersPanel.test.ts` 新增「供应商列表定高内部滚动，滚动条沿用设置页静默样式」，锁定列容器定高、rail 溢出滚动与滚动条例外组归属。
- 验证：`pnpm run lint:css` 通过；`pnpm run typecheck` 通过；`pnpm exec vitest run src/components/ProvidersPanel.test.ts` 2 文件 15 项通过。
- 未验收：未取得原生桌面同状态前后截图做像素比对（本会话无法驱动原生 Tauri UI）。行为可由 CSS 推导：列表内容超出 560px 定高后 rail 内部滚动，页面高度不再随供应商数量增长；窄屏维持 280px 现状。
- 修正（同日真机复验）：真机窗口实际表现为「单项被压缩、无滚动条」而非滚动。根因：`.prov-rail` 在定高下是纵向 flex 容器，`.prov-item` 默认 `flex-shrink: 1`，flex 先把子项压缩到恰好塞满，`overflow: auto` 没有溢出可滚。修复：`.prov-item` 与 `.prov-rail-empty` 增加 `flex-shrink: 0`，子项保持自然高度、超高交给 rail 滚动；布局锁定测试补充 flex-shrink 断言。上一条「行为可由 CSS 推导」的推断有误，滚动行为以本次真机与子项不收缩为前提。
- 验证：`pnpm run lint:css`、`pnpm run typecheck` 通过；`pnpm exec vitest run src/components/ProvidersPanel.test.ts` 2 文件 15 项通过。修复已随 Vite HMR 热更新到运行中的开发桌面。
- 修正二（同日真机复验）：单项恢复自然高度后，列表被 560px 定高截断，与右侧表单卡片高度不一致。改为 `.prov-split__list { contain: size }`：列表内容不计入栅格行高，行高只由右侧表单决定，左列经 `align-items: stretch` 拉伸到与右栏等高，供应商溢出仍由 `.prov-rail` 内部滚动；窄屏堆叠断点恢复 `contain: none` 并保留 280px 上限，避免 containment 把列表塌成 0。
- 验证：以 headless Chromium（chromium_headless_shell-1228）对最小复现页实测四场景——30 项 + 900px 表单：行高 900、列表 900 等高、内部滚动（client 856 / scroll 2094）；11 项：等高 900、无需滚动；2 项 + 100px 表单：行高落到 420px 下限、等高；堆叠 280px 上限正常滚动。`pnpm run lint:css`、`pnpm run typecheck` 通过；`pnpm exec vitest run src/components/ProvidersPanel.test.ts` 2 文件 15 项通过。修复已随 Vite HMR 热更新到运行中的开发桌面。
- 修正三（用户反馈）：编辑态右上角不应出现导入按钮。改为随模式切换：新增态右上角为导入（`IconPush` + `prov.importAll`），编辑态右上角仅为当前供应商的复制与导出；布局锁定测试改为断言导入位于编辑态三元分支的 else 侧。编辑态需要导入时经「添加提供商」进入新增态。`pnpm run typecheck` 通过；`pnpm exec vitest run src/components/ProvidersPanel.test.ts` 2 文件 15 项通过。
