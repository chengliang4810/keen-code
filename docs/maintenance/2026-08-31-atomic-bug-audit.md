# 2026-08-31 原子级功能 Bug 审计报告

## 结论

本轮在当前 `main` HEAD 上确认了 **53 个相互独立的产品缺陷**：

- P0（阻断、数据损失或安全边界）：10 个；
- P1（核心路径错误、持久化/并发一致性或明显性能风险）：33 个；
- P2（错误反馈、刷新、可访问性或次要交互）：10 个。

另确认 6 组测试基础设施缺陷、4 组已接受风险或契约偏差、9 个需要真实桌面或故障注入后才能定性的验证缺口。报告严格区分“产品 Bug”“测试缺陷”“已接受风险”“技术债”和“尚未验证”，不会把源码猜测或明确非目标伪装成已复现故障。

阶段 1.1 只记录问题，不提前修改产品实现。后续阶段必须按本报告的原子 ID 建立回归测试并修复根因。

## 审计基线与边界

| 项目 | 结果 |
| --- | --- |
| 审计基线 | `dce58c7`（阶段 0.2 提交后） |
| 前端测试 | `pnpm.cmd test`：Node 8/8；Vitest 108 个文件、894 个测试全部通过 |
| Tauri Rust 测试 | `cargo test --manifest-path src-tauri\Cargo.toml --lib -- --nocapture`：245 通过、5 失败 |
| Vendored Peri 全工作区 | 仅 3 个 target 失败；归因见 TEST-002～TEST-004 |
| 当前桌面验证 | 本阶段没有宣称完成全功能 Windows 桌面 E2E；源码可达性和可控异步路径不等同于真实用户路径验收 |
| 排除项 | PDF、Agent Teams、递归 Agent、Cron/Workflow、外部消息渠道、Bedrock 等项目明确非目标 |

等级定义：

- **P0**：可直接造成错误写入、未保存数据丢失、核心功能不可用、越过项目访问边界，或关键运行状态永久丢失。
- **P1**：核心功能在常见异常/并发条件下错误，或持久化、生命周期和资源预算存在确定性缺陷。
- **P2**：错误反馈、刷新、一致性或无障碍缺陷；不会立即破坏底层数据，但会误导或阻断部分用户。

## 已确认产品 Bug

### BUG-001 P0：Windows 工作区原子覆盖无法替换既有文件

- **功能点**：保存文件、项目重命名、项目排序等复用工作区原子写入的功能。
- **复现条件**：Windows 上目标文件已经存在，再次调用写入。
- **实际/预期**：`fs::rename(temp, target)` 对既有目标可能返回失败；预期可靠替换旧文件且保留原子性。
- **根因与影响**：`src-tauri/src/workspace.rs:464-492` 自行实现 rename 覆盖，没有使用 Windows 可替换目标的统一原子写入入口；重复保存代码文件或项目清单可失败。
- **建议回归**：在同一路径连续写入两次，断言第二次成功、内容为新值、临时文件清理，且故障注入时旧文件仍完整。
- **建议阶段**：1.4（统一原子写入）和 5.1（持久化）。

### BUG-002 P0：Windows 供应商私有配置无法可靠覆盖

- **功能点**：第二次保存或更新自定义供应商配置。
- **复现条件**：Windows 上供应商私有文件已经存在。
- **实际/预期**：`src-tauri/src/providers.rs:647-670` 同样用 `fs::rename` 覆盖既有目标，更新可能失败；预期安全替换并保持私有权限。
- **根因与影响**：重复实现了比 `src-tauri/src/storage.rs:39-45` 更弱的写入事务，供应商编辑/密钥更新不可用。
- **建议回归**：连续保存两版供应商目录，验证第二版可读、权限正确、失败时旧版不丢。
- **建议阶段**：1.4、5.1。

### BUG-003 P0：损坏的 projects.json 会被当作空清单并被覆盖

- **功能点**：项目清单加载以及后续创建、重命名、排序。
- **复现条件**：`projects.json` 存在截断、非法 JSON 或当前结构校验失败。
- **实际/预期**：`src-tauri/src/workspace.rs:351-370` 返回空列表；下一次项目变更会把原文件覆盖为空基线。预期隔离损坏文件、保留可恢复副本并阻止破坏性覆盖。
- **根因与影响**：读取失败和“确实没有项目”共用 `Ok(Vec::new())`，造成项目清单数据损失。
- **建议回归**：提供截断 JSON，断言读取明确报错或进入恢复状态，任何写命令不得覆盖原文件。
- **建议阶段**：5.1。

### BUG-004 P0：Windows MCPB/DXT 首次解包稳定失败

- **功能点**：安装或加载 MCPB/DXT 插件包。
- **复现条件**：Windows 上首次解包并写完成标记。
- **实际/预期**：3 个当前测试稳定返回 `PermissionDenied (code 5)`；预期完整解包并提交内容哈希缓存。
- **根因与影响**：`src-tauri/src/claude_plugins.rs:4069-4071` 写完标记后用只读 `File::open` 句柄执行 `sync_all`；Windows 的 `FlushFileBuffers` 要求可写句柄。真实插件路径不可用。
- **建议回归**：保留写句柄同步后，在 Windows 连续验证首次解包、缓存复用、并发同哈希和配置合并；不能仅删除断言。
- **建议阶段**：2.1/2.3（插件功能闭环）。

### BUG-005 P0：critical EventBus 饱和时永久丢关键事件

- **功能点**：文本/思考渲染、工具开始/结束、`TurnCompleted` 和状态快照。
- **复现条件**：Render 或 State 有界通道短时写满。
- **实际/预期**：`vendor/peri/peri-acp-types/src/event_v2.rs:453-465` 对 `try_send` 失败直接丢弃；`_drop_timeout` 在 `:400-441` 仅保存而未使用。预期 critical 事件有有界背压、超时重试或可恢复快照。
- **根因与影响**：实现与注释中的 critical/超时承诺不一致，UI 可永久停留“运行中”、工具状态不闭合或响应缺块。
- **建议回归**：容量为 1 的总线故意饱和，逐类验证 `TurnCompleted`、工具终态和最新状态不会无恢复丢失，同时测取消和慢消费者。
- **建议阶段**：5.1。

### BUG-006 P0：终端 cwd 没有已加入项目的访问边界

- **功能点**：创建终端 Session。
- **复现条件**：IPC 调用传入项目外任意绝对目录。
- **实际/预期**：`src-tauri/src/terminal.rs:179-213` 只验证目录存在；预期 cwd 必须位于已加入项目或明确授权的数据目录。
- **根因与影响**：系统能力层没有复用项目授权校验，WebView/IPC 调用可在授权范围外启动 Shell，违反“加入项目即授予该目录范围”的边界。
- **建议回归**：项目内目录允许，项目外目录、路径穿越、符号链接越界和大小写/UNC 变体拒绝。
- **建议阶段**：2.2、4.4。

### BUG-007 P0：保存冲突弹窗会重载或覆盖错误标签

- **功能点**：资源编辑器的并发写冲突处理。
- **复现条件**：保存 A，在请求完成前切到 B，A 返回冲突后点击“重新加载”或“覆盖”。
- **实际/预期**：弹窗记录 `conflictTabId=A`，但操作仍按当前 `activeId=B` 调用 `reloadActiveFile`/`saveActiveFile`；预期只处理 A。
- **根因与影响**：`src/components/ResourceViewer.tsx:335,918-1038,2229-2239,2662-2686` 的冲突身份未传给动作，可能强制写错 B 或丢弃 B 草稿。
- **建议回归**：用 deferred write 制造冲突并切标签，断言重载/覆盖参数始终是 A，B 草稿不变。
- **建议阶段**：2.2、5.2。

### BUG-008 P0：批量关闭标签会静默丢弃未保存内容

- **功能点**：关闭其他、关闭左侧、关闭右侧、关闭全部资源标签。
- **复现条件**：目标集合含至少一个 dirty 文件。
- **实际/预期**：`src/components/ResourceViewer.tsx:1352-1408,2620-2648` 直接过滤或清空；预期逐项确认或一次性明确列出未保存文件。
- **根因与影响**：只有单标签关闭走脏数据确认，批量入口绕过同一规则，造成直接数据损失。
- **建议回归**：覆盖四个批量命令、多个 dirty 标签、取消/确认和当前标签变化。
- **建议阶段**：3.2（统一关闭规则）、5.2。

### BUG-009 P0：运行时提醒与用户文本缺少可验证来源边界

- **功能点**：Goal steering、Hook 反馈、工具失败、后台结果等运行时提醒。
- **复现条件**：用户输入与运行时消息都包含同形 `<system-reminder>`。
- **实际/预期**：`QueuedMessage` 保存 `MessageSource`，但 `vendor/peri/peri-agent/src/agent/stages/mod.rs:486-516` 丢弃来源并统一转成 Human 文本；提示词 `14_system_reminder.md:3-12` 却要求模型把运行时同形文本视为权威。预期模型可见协议保留不可伪造来源。
- **根因与影响**：信任结论依赖纯文本标签，而该标签可由用户完全复制，形成提示注入和错误工作流控制路径。
- **建议回归**：同一文本分别以 UserInput 与 SystemInjected/GoalSteering 注入，断言模型请求中的角色或结构可区分且用户标签不获权威。
- **建议阶段**：4.4。

### BUG-010 P1：无鉴权本地供应商无法切换已有会话模型

- **功能点**：本机 OpenAI 兼容端点的会话模型切换。
- **复现条件**：供应商允许空 API Key，已有会话切到其模型。
- **实际/预期**：供应商配置允许空 Key（`src-tauri/src/providers.rs:35-36,120-121,627-632`），会话切换却在 `src-tauri/src/session_commands.rs:962-964` 无条件要求 Key；预期与供应商认证策略一致。
- **根因与影响**：创建/拉取模型与会话切换使用两套认证判定，本地模型配置成功但不能实际使用。
- **建议回归**：分别覆盖无需 Key、需要 Key 但缺失、需要 Key 且存在三种供应商。
- **建议阶段**：2.1、2.2。

### BUG-011 P1：显式选择的项目外附件不能打开或定位

- **功能点**：附件卡片“打开”和“在文件夹中显示”。
- **复现条件**：用户通过系统选择器添加项目外文件。
- **实际/预期**：选择器允许任意文件（`src-tauri/src/workspace.rs:805-812`），打开/定位只允许项目或应用数据目录（`:895-920`）；预期显式选择产生可追踪的最小授权，或 UI 不展示不可用动作。
- **根因与影响**：选择与后续动作的授权模型不一致，`AttachmentCard.tsx:119-127`、`ImageUi.tsx:274-276` 的可见按钮稳定失败。
- **建议回归**：项目内、显式选择的项目外、未选择的项目外三种路径分别验证。
- **建议阶段**：2.1、2.3、5.2。

### BUG-012 P1：带标题分叉失败会遗留孤儿 Session

- **功能点**：分叉会话并设置标题。
- **复现条件**：fork 持久化成功、随后重命名失败。
- **实际/预期**：`src-tauri/src/session_commands.rs:761-799` 直接返回错误但不删除 fork；预期事务回滚或把已创建 Session 返回给前端。
- **根因与影响**：两步操作没有补偿，与 `:899-905` 已有的编辑前分支回滚行为不一致，产生前端不知道的孤儿数据。
- **建议回归**：注入 rename 失败，断言 fork 被清理；再覆盖清理失败的可观测错误。
- **建议阶段**：1.4、5.1。

### BUG-013 P1：取消全部后台任务在首个失败处提前中止

- **功能点**：应用退出或 Session 关闭时取消全部后台任务。
- **复现条件**：列表前部存在一个授权/RPC 已失效的 Session，后部仍有正常任务。
- **实际/预期**：`src-tauri/src/session_commands.rs:403-413` 在循环内使用 `?`；预期尝试所有任务并汇总失败。
- **根因与影响**：批处理使用 fail-fast，后续任务残留运行。
- **建议回归**：三个任务中首个/中间失败，断言其余均收到取消且错误包含失败集合。
- **建议阶段**：3.3。

### BUG-014 P1：项目级 Agent 错误绑定进程 cwd

- **功能点**：项目 Agent 列表与详情。
- **复现条件**：安装版进程 cwd 不是当前用户项目。
- **实际/预期**：`src-tauri/src/extensions.rs:3053-3080,3160-3167` 扫描 `std::env::current_dir()`，前端 `src/lib/api.ts:898,937` 也无法传项目；预期使用当前已加入项目路径。
- **根因与影响**：UI 会漏掉当前项目 Agent，或把启动目录中的 Agent 误归为当前项目。
- **建议回归**：进程 cwd 与项目目录刻意不同，验证列表/详情只读取项目和规定的全局目录。
- **建议阶段**：2.1、2.3。

### BUG-015 P1：Agent 模型覆盖可保存不存在的供应商或模型

- **功能点**：Agent 定义中的 `provider::model` 覆盖。
- **复现条件**：保存格式合法但目录中不存在的供应商/模型，或随后删除被引用供应商。
- **实际/预期**：`src-tauri/src/extensions.rs:3398-3402,3435-3454` 只校验字符串格式；预期保存前校验当前供应商目录，并在删除时处理引用。
- **根因与影响**：格式校验代替实体校验，留下 UI 看似有效但永远不能运行的 Agent。
- **建议回归**：存在/不存在/删除后引用三种路径，错误需定位到具体 Agent。
- **建议阶段**：2.1、2.2。

### BUG-016 P1：供应商配置落盘与现有 Session 热加载状态分裂

- **功能点**：创建、更新、删除供应商后热更新当前会话。
- **复现条件**：磁盘保存成功，`reload_provider` 通知任一现有 Session 失败。
- **实际/预期**：`src-tauri/src/lib.rs:136-166,183-198,216-231` 返回错误，但磁盘和新会话已使用新配置，旧会话仍持有旧快照；预期事务补偿或明确的逐 Session 收敛状态。
- **根因与影响**：持久层与运行时更新不是原子事务，也没有可重试状态。
- **建议回归**：两会话中一个 reload 失败，验证命令结果、磁盘、两会话和重试后的最终一致性。
- **建议阶段**：5.1。

### BUG-017 P1：插件配置落盘与运行时刷新状态分裂

- **功能点**：插件安装、更新、启停与卸载。
- **复现条件**：文件操作成功，运行时插件刷新失败。
- **实际/预期**：`src-tauri/src/extensions.rs:3665-3673` 及安装/更新/卸载同型路径先写磁盘后刷新；预期 UI、磁盘和全部活动 Session 对同一版本达成一致，或返回可恢复的部分成功状态。
- **根因与影响**：跨层事务没有补偿/重试协议，当前与新 Session 可见插件集合不同。
- **建议回归**：对四种变更注入刷新失败并验证恢复动作。
- **建议阶段**：5.1。

### BUG-018 P1：退出清理失败会留下不可工作的窗口

- **功能点**：正常退出。
- **复现条件**：运行时已 shutdown 后 analytics flush 失败。
- **实际/预期**：`src-tauri/src/app_exit.rs:39-43,74-95` 保留窗口并报告失败，但 `src-tauri/src/peri_runtime.rs:1475-1479` 已拒绝新任务；预期失败后应用仍可工作，或完成退出并保存可恢复错误。
- **根因与影响**：不可逆 shutdown 早于可能失败的持久化步骤，用户只能强制重启。
- **建议回归**：注入 flush 失败，验证窗口状态和后续新任务；再验证重试退出幂等。
- **建议阶段**：5.1。

### BUG-019 P1：重置本地记忆与在途流水线竞态

- **功能点**：删除/重置本地记忆。
- **复现条件**：记忆抽取或整合任务运行中调用 `memories_reset`。
- **实际/预期**：`src-tauri/src/memories.rs:169-205,536-550,602-605` 不取消或等待流水线；删除成功后后台任务可重新生成文件。预期 reset 完成后旧代任务不能再写。
- **根因与影响**：清理与调度没有共享 generation/cancel 屏障，用户看到的数据会“死而复生”。
- **建议回归**：挂起 pipeline 写入点，reset 后恢复旧任务，断言旧 generation 被拒绝。
- **建议阶段**：5.1。

### BUG-020 P1：记忆状态并发读改写会丢更新

- **功能点**：多会话同时更新记忆 usage、job 和 output 状态。
- **复现条件**：两个会话同时执行 `load_state → 修改 → save_state`。
- **实际/预期**：`src-tauri/src/memories.rs:134-165,220-376,494-534` 无统一 I/O 锁；预期合并两个更新。
- **根因与影响**：经典 lost-update；`:739-755` 的临时文件名还只含 PID，同进程并发写会争用同一路径。
- **建议回归**：用 barrier 同时保存两个不同字段，循环运行并断言两者都保留。
- **建议阶段**：5.1。

### BUG-021 P1：Windows 记忆替换存在删除旧文件后的数据丢失窗口

- **功能点**：写 `MEMORY.md`、摘要和 `state.json`。
- **复现条件**：Windows 目标已存在，删除后 rename 失败或进程崩溃。
- **实际/预期**：`src-tauri/src/memories.rs:739-755` 先删旧文件再 rename；预期任何失败都保留旧版或完整新版。
- **根因与影响**：没有复用统一原子写入，断电/失败可直接丢记忆，且与 BUG-020 的临时名冲突叠加。
- **建议回归**：在删除和提交之间故障注入，断言旧文件仍存在；并发两次写不共用临时路径。
- **建议阶段**：1.4、5.1。

### BUG-022 P1：损坏的记忆状态会被静默覆盖

- **功能点**：加载记忆状态并继续抽取。
- **复现条件**：`state.json` 截断或字段非法。
- **实际/预期**：`src-tauri/src/memories.rs:494-506` 仅警告后返回空状态，下一次保存覆盖原文件；预期隔离、备份并阻止无提示覆盖。
- **根因与影响**：损坏与“首次运行”共用空状态，丢失所有既有运行进度。
- **建议回归**：损坏输入后调用 prompt/pipeline，断言原文件被保留且 UI 可见恢复错误。
- **建议阶段**：5.1。

### BUG-023 P1：rollout 汇总先全删再逐个写

- **功能点**：重建记忆 rollout summaries。
- **复现条件**：重建中任一文件写失败或进程退出。
- **实际/预期**：`src-tauri/src/memories.rs:446-480` 先删除全部旧集合；预期在独立临时目录生成完整集合后一次提交。
- **根因与影响**：集合级更新没有事务，失败后只留下残缺摘要。
- **建议回归**：第 N 个摘要写入故障，断言旧集合仍完整；成功时不存在旧/新混合集。
- **建议阶段**：5.1。

### BUG-024 P1：一个无效设置字段会重置全部有效设置

- **功能点**：启动加载 `settings.json`。
- **复现条件**：仅一个字段类型或约束非法，其余字段有效。
- **实际/预期**：`src-tauri/src/app_settings.rs:488-553` 整体回退 `AppSettings::initial()`；预期只修复无效字段并保留有效值。
- **根因与影响**：全对象反序列化/校验失败采用全量默认，用户的项目目录、语言、外观等无关设置被重置。
- **建议回归**：逐字段损坏参数化测试，断言其余字段不变、损坏原文件有备份。
- **建议阶段**：5.1。

### BUG-025 P1：请求历史尾行损坏会让全部历史不可访问

- **功能点**：读取 analytics 请求 JSONL。
- **复现条件**：崩溃留下最后一行半写。
- **实际/预期**：`src-tauri/src/analytics.rs:456-479` 任一行解析失败就整体报错；预期保留此前有效行，并仅隔离/修复不完整尾行。
- **根因与影响**：流式追加格式却采用全有或全无读取，历史页面完全空白。
- **建议回归**：有效行 + 截断尾行、非尾部损坏两种场景分别验证恢复和明确隔离。
- **建议阶段**：5.1。

### BUG-026 P1：图片“压缩”管线为空且在 async 路径多次复制

- **功能点**：向模型发送图片附件。
- **复现条件**：接近默认 20 MiB 上限的图片。
- **实际/预期**：`vendor/peri/peri-middlewares/src/middleware/image/mod.rs:20-34,144-151` 默认 compressor 为空；`compressor.rs:35-53` 仍 `to_vec()`，随后 `mod.rs:230` Base64 扩容。预期按尺寸/像素压缩并把 CPU 工作移出 async 主循环。
- **根因与影响**：空管线至少同时持有原始、复制和约 1.33 倍编码结果，违反低内存预算并扩大请求体。
- **建议回归**：记录 1/10/20 MiB 输入的峰值内存、输出大小和执行线程，验证失败降级有明确上限。
- **建议阶段**：4.2、5.3。

### BUG-027 P1：运行时提醒被重复包裹

- **功能点**：Goal steering、Stop Hook、工具失败和 Speculation Guard 提醒。
- **复现条件**：这些生产者已经生成 `<system-reminder>` 后作为 Info/Defer 入队。
- **实际/预期**：`goal_middleware.rs:107-115`、`hooks/middleware.rs:371-380`、`tool_dispatch.rs:709-713`、`speculation_guard.rs:161-163` 已包裹一次，`stages/mod.rs:505-510` 再包一次；预期模型只收到一层规范结构。
- **根因与影响**：生产者与队列消费方都拥有格式化职责，测试只做 `contains`，嵌套标签会破坏解析与信任判断。
- **建议回归**：每种 `MessageSource` 精确断言起止标签各一次，并把格式化收敛到唯一入口。
- **建议阶段**：1.4、3.2。

### BUG-028 P1：异步子 Agent 完成后没有父回合 join/idle 屏障

- **功能点**：父 Agent 委派异步子 Agent 并汇总结果。
- **复现条件**：父 ReAct 队列先变空，子 Agent 后完成。
- **实际/预期**：`vendor/peri/peri-agent/src/agent/stages/mod.rs:599-613` 直接 `Completed`；`SessionInbox::await_wake` 只有测试调用，已装配的 `idle_inbox`/`idle_suspended_flag` 不被循环消费。预期普通委派在父任务完成前进入 Join 状态；显式后台任务可另行定义。
- **根因与影响**：完成结果虽入 Defer 队列，但不会自动启动已结束父回合，用户得不到默认汇总。
- **建议回归**：子任务在父队列清空前/后完成、用户取消、用户新输入、多个子任务和显式后台五类生命周期。
- **建议阶段**：2.2、3.4、4.4。

### BUG-029 P1：Plan Mode 同时只能记录一个会话

- **功能点**：多会话并行使用 Plan Mode。
- **复现条件**：会话 A 开启 Plan Mode，再在会话 B 开启。
- **实际/预期**：`src/hooks/composer/useComposerModes.ts:16-17,51-65` 只有单个 `planModeSessionKey: string | null`；预期按 ADR-0006 的 `sessionId ?? "__draft__"` 独立键控。
- **根因与影响**：开启 B 会隐式关闭 A，切回 A 时 UI 和发送契约错误。
- **建议回归**：A/B/草稿三键并存、草稿首发迁移和关闭单键不影响其他键。
- **建议阶段**：2.2、5.1。

### BUG-030 P1：多个已识别 Hook 事件没有生产触发点

- **功能点**：Hook 配置与生命周期触发。
- **复现条件**：配置 `Setup`、`TaskCreated`、`TaskCompleted`、`ConfigChange`、`WorktreeCreate`、`WorktreeRemove`、`Elicitation`、`ElicitationResult`、`CwdChanged` 或 `FileChanged`。
- **实际/预期**：这些值可被 `HookEvent::parse` 接受并进入 UI/配置，但生产代码没有对应生命周期调用；预期已接受事件可触发，或 UI 明确标为不支持并拒绝保存。
- **根因与影响**：事件枚举/输入构造器先于实际 wiring 暴露，UI 只提示“未知事件”，无法提示“已知但不可达”。
- **建议回归**：建立事件支持矩阵；每个可配置事件至少一条真实生命周期集成测试。`TeammateIdle` 属非目标，不纳入。
- **建议阶段**：2.1、2.3。

### BUG-031 P1：切换项目会清空按 Session 保存的资源标签和脏草稿

- **功能点**：Session 资源编辑状态。
- **复现条件**：当前 Session 打开并编辑文件，切换项目后返回。
- **实际/预期**：`src/components/ResourceViewer.tsx:170-183,309-320,796-810` 虽按 `sessionKey` 保存状态，却在 `projectPath` 变化时无条件清空；预期 dirty 数据受保护并按 Session/项目身份恢复。
- **根因与影响**：持久化键和清理触发条件不一致，未保存内容丢失。
- **建议回归**：两个 Session/两个项目交叉切换，验证 tab、activeId、draft、baseline 和冲突状态隔离。
- **建议阶段**：5.1、5.2。

### BUG-032 P1：旧项目的迟到目录响应会污染新项目文件树

- **功能点**：文件树懒加载。
- **复现条件**：展开 A/src 后立即切 B，B 根先返回、A/src 最后返回。
- **实际/预期**：`ResourceViewer.tsx:717-732,796-810,826-843` await 后无项目快照/请求序号校验，直接 patch 当前树；预期丢弃旧项目响应。
- **根因与影响**：异步目录请求没有身份，B 树可显示并操作 A 文件。
- **建议回归**：deferred `fsListDir` 控制乱序，覆盖同名/不同名目录。
- **建议阶段**：5.1。

### BUG-033 P1：旧项目 Git Diff 会显示在新项目

- **功能点**：Workspace 变更 Diff。
- **复现条件**：A 的 diff 请求在途时切 B，A 最后返回。
- **实际/预期**：`ResourceViewer.tsx:354-363,524-629,796-810` 切项目未使 `diffLoadSeq` 失效，也不校验 project snapshot；预期 B 页面不采用 A 结果。
- **根因与影响**：不同项目共享请求序号生命周期，造成跨项目信息泄漏和误审。
- **建议回归**：deferred `gitFileDiff` + rerender B，断言旧响应被忽略且 B loading 正确收口。
- **建议阶段**：5.1。

### BUG-034 P1：侧栏折叠偏好启动即被错误回写

- **功能点**：项目展开/折叠持久化。
- **复现条件**：只持久化 B 为折叠，重启加载 A/B。
- **实际/预期**：`src/hooks/sidebar/useSidebarLists.ts:70-104` 未读设置而先把全部项目设为 false，`:123-133` 再把全部 ID 写回；预期 `src/lib/sidebarExpand.ts:4-20` 的“缺失 ID 默认展开”规则。
- **根因与影响**：正确纯函数未被 Hook 使用，启动会破坏用户偏好。
- **建议回归**：mock `settingsGet=[B]`，断言 A=true/B=false 且初始加载不产生改写。
- **建议阶段**：5.1。

### BUG-035 P1：当前格式 localStorage 损坏会导致启动白屏

- **功能点**：布局、Session 完成状态、侧栏顺序等本地投影恢复。
- **复现条件**：任一当前键包含截断/非法 JSON。
- **实际/预期**：`src/App.tsx:165-177`、`src/lib/layout.ts:101-109`、`src/lib/sessionCompletion.ts:14-27`、`src/hooks/sidebar/useSidebarLists.ts:67`、`src/lib/sidebarOrder.ts:3-10` 的渲染期 initializer 可直接抛出；预期仅丢弃损坏的可再生 UI 投影并继续启动。
- **根因与影响**：没有数据损坏边界；这不是为旧结构做兼容迁移。
- **建议回归**：逐键注入非法值，断言应用启动、损坏键被隔离、Rust 权威数据仍显示。
- **建议阶段**：5.1。

### BUG-036 P1：文件卡片 props 更新后仍操作旧路径

- **功能点**：聊天中的文件路径卡片打开/定位。
- **复现条件**：同一组件从 `a.ts` rerender 为 `b.ts` 后点击。
- **实际/预期**：`src/components/FilePathCard.tsx:109-123,164-177,188-245` 的 `resolvedAbs` 没有随 path 失效，动作仍优先旧绝对路径；预期使用 b。
- **根因与影响**：派生状态缺少输入身份，可能打开/定位错误文件。
- **建议回归**：先解析 a，再 rerender b，分别验证打开和 reveal 参数。
- **建议阶段**：5.2。

### BUG-037 P1：通用设置已落盘后 reload 失败导致前后端分叉

- **功能点**：界面语言等 `settingsSet` 设置。
- **复现条件**：`app_settings::set` 成功，后续 provider/runtime reload 失败。
- **实际/预期**：`src-tauri/src/lib.rs:59-99` 先落盘再返回错误；`src/hooks/useAppSettings.ts:84-99,147-156` 对任何错误回滚 UI。预期磁盘、运行时和 UI 同步回滚或明确部分成功。
- **根因与影响**：后端错误语义把“未保存”和“已保存但热更新失败”混为一类，重启后界面会突然变成磁盘新值。
- **建议回归**：故障注入 reload，断言三层最终一致；错误结构需包含持久化状态。
- **建议阶段**：5.1。

### BUG-038 P2：附件分类失败仍提示添加成功

- **功能点**：Composer 选择文件或粘贴路径。
- **复现条件**：`classifyPaths` 失败。
- **实际/预期**：`src/hooks/composer/useComposerAttachments.ts:88-192` 的公共添加函数吞异常，调用方无条件清错并显示成功；预期显示失败且不声称已添加。
- **根因与影响**：异常未向调用者传播，用户误以为附件会发送。
- **建议回归**：选择、粘贴、拖放三个入口注入失败，统一断言附件列表和 toast。
- **建议阶段**：3.3、5.2。

### BUG-039 P2：设置并发保存会由旧失败回滚最后选择

- **功能点**：快速连续切换语言、通知等设置。
- **复现条件**：请求 1 较晚失败，请求 2 较早成功。
- **实际/预期**：`src/hooks/useAppSettings.ts:84-99,147-157,192-214` 的旧闭包用 `previous` 回滚当前状态；预期仅最新 revision 能提交/回滚。
- **根因与影响**：乐观更新没有请求身份，UI 最终值与最后用户选择、磁盘值都可能不一致。
- **建议回归**：可控 Promise 交换完成顺序，覆盖成功/失败四种组合。
- **建议阶段**：5.1。

### BUG-040 P2：快速主题切换会被旧异步调用覆盖

- **功能点**：浅色/深色/跟随系统切换。
- **复现条件**：快速 `system → dark`，system 的 native/RAF 分支最后完成。
- **实际/预期**：`src/hooks/useThemeAppearance.ts:59-66` 与 `src/lib/theme.ts:111-136` 无 revision/cancel；预期最终 DOM/native/localStorage 都是最后选择 dark。
- **根因与影响**：旧异步操作仍有写权限，`SettingsPage.tsx:895-910` 也未串行化。
- **建议回归**：控制 native 和 RAF promise 完成顺序，断言最后选择获胜。
- **建议阶段**：5.1、5.3。

### BUG-041 P2：设置读取失败时自动归档 UI 与运行状态相反

- **功能点**：自动归档开关。
- **复现条件**：启动时 `settingsGet` 失败，状态保持 null。
- **实际/预期**：`src/hooks/useAppSettings.ts:102-123` 静默吞错；`src/App.tsx:668` 将 null 当关闭，`src/features/app/SettingsRoute.tsx:153` 将同一 null 当开启。预期单一明确的 loading/error/default 语义。
- **根因与影响**：同一三态值在两个消费者中使用相反默认，UI 会误报实际行为。
- **建议回归**：loading、成功 true/false、失败四态集成测试。
- **建议阶段**：3.2、5.2。

### BUG-042 P2：Provider 刷新失败仍提示热重载成功

- **功能点**：Provider 保存/删除后的目录刷新提示。
- **复现条件**：后续 `providersList` reject。
- **实际/预期**：`src/hooks/useProviderModels.ts:112-152` 吞异常并 resolve，`:208-216` 必进成功分支、增加 revision；预期保留旧状态并显示错误。
- **根因与影响**：内部刷新函数把失败转换为成功，用户被告知不存在的热重载结果。
- **建议回归**：mock list reject，断言 revision 不变、success toast 不出现、error 可重试。
- **建议阶段**：5.1、5.2。

### BUG-043 P2：插件变更后 Composer Slash Skills 不刷新

- **功能点**：插件启停、卸载、更新后的 `/skill` 菜单。
- **复现条件**：在扩展面板改变插件后返回 Composer。
- **实际/预期**：`src/hooks/composer/useComposerSlashMenu.ts:109-135` 只依赖 projectPath，`ExtensionsPanel.tsx:65-73,645-685` 无变化通知；预期菜单自动反映当前插件技能。
- **根因与影响**：扩展目录和 Composer 投影没有失效事件，重启前显示过期功能。
- **建议回归**：四种插件变更分别断言技能新增/移除，无轮询。
- **建议阶段**：2.3、5.1。

### BUG-044 P2：ExtensionsPanel 会被旧项目响应覆盖

- **功能点**：项目 Skills/Agents/扩展列表。
- **复现条件**：A 刷新在途时切 B，A 最后完成。
- **实际/预期**：`src/components/ExtensionsPanel.tsx:328-381` 无 active/project/sequence guard；预期忽略 A 响应。
- **根因与影响**：异步结果缺少项目身份，B 页面展示 A 配置。
- **建议回归**：deferred list + rerender B，断言数据与 loading 均由 B 请求收口。
- **建议阶段**：5.1。

### BUG-045 P2：ACP 监听器部分注册失败会泄漏并跳过恢复

- **功能点**：启动注册 ACP 事件监听与 Session 状态 bootstrap。
- **复现条件**：多个 `listen` 中一项 reject、其他项已成功。
- **实际/预期**：`src/hooks/acp-runtime/events.ts:190-193,663-779` 使用 `Promise.all`，失败时已成功 disposer 未保存，后续 `sessionGetState` 也跳过；预期清理部分成功监听并显式重试/报错。
- **根因与影响**：批量资源获取没有 rollback，重新挂载可重复监听和重复处理事件。
- **建议回归**：逐个索引注入注册失败，断言已注册者全部 unlisten 且 bootstrap 状态明确。
- **建议阶段**：5.1。

### BUG-046 P2：AppDialog 声明模态但未限制/恢复焦点

- **功能点**：应用级确认对话框的键盘操作。
- **复现条件**：打开对话框后 Tab/Shift+Tab 或关闭。
- **实际/预期**：`src/features/app/overlays/AppDialogPortal.tsx:49-134`、`src/hooks/useAppDialog.ts:15-34,80-103` 缺少焦点陷阱和关闭后恢复；预期符合 modal dialog 键盘语义。
- **根因与影响**：ARIA 声明与实际焦点管理不一致，键盘/读屏用户可进入背景界面。
- **建议回归**：初始焦点、循环 Tab、Escape、确认关闭和触发器恢复。
- **建议阶段**：5.2。

### BUG-047 P2：ContextMenu 缺少标准键盘导航

- **功能点**：项目、Session、附件和资源标签上下文菜单。
- **复现条件**：Shift+F10/键盘打开并尝试方向键操作。
- **实际/预期**：`src/components/ContextMenu.tsx:105-168` 只有外部点击和 Escape；预期初始聚焦、上下箭头、Home/End、Enter/Space 和关闭后恢复。
- **根因与影响**：自建菜单没有实现 menu roving focus 语义，键盘用户无法可靠使用。
- **建议回归**：禁用项、分隔符、首尾循环、嵌套在多种触发器中的焦点恢复。
- **建议阶段**：3.3、5.2。

### BUG-048 P1：打开 Session 失败后视图引用没有回滚

- **功能点**：从 Session A 切换并打开 Session B。
- **复现条件**：B 的项目路径丢失、连接或状态恢复失败。
- **实际/预期**：`src/hooks/useSessionNavigation.ts:261-267` 在连接前把 `viewingSessionIdRef` 改为 B，`:344-348` 的 catch 只清 opening slot；可见 UI 仍是 A，但内部 ref 已是 B。预期切换失败原子回滚到 A。
- **根因与影响**：导航事务提前提交身份且没有补偿，后续 `snapshotOutgoingSession`/队列逻辑可能把 A 的消息投影写入 B。
- **建议回归**：A 可见、B connect reject，断言 session、ref、消息 cache 和 active project 全部仍指 A。
- **建议阶段**：5.1。

### BUG-049 P1：直接发送失败会丢失尚未发送的正文与附件

- **功能点**：Composer 直接发送。
- **复现条件**：`session_connect`、Provider 或 send IPC 在 Host 接受消息前失败。
- **实际/预期**：`src/hooks/session-turn/useSessionDraftSend.ts:71-91,121-129` 先调用 `clearComposerAfterSubmit()`，再 await `executeSend`，false/reject 后不恢复；预期失败时保留原草稿、附件和模式。
- **根因与影响**：UI 在发送事务确认前销毁唯一草稿副本，用户输入直接丢失。
- **建议回归**：`executeSend=false` 和 reject 两条路径，断言正文、附件、Goal/Plan 相关草稿状态均可重试。
- **建议阶段**：5.1、5.2。

### BUG-050 P1：一个 Session 的队列失败会暂停全部 Session

- **功能点**：按 Session 隔离的 follow-up 发送队列。
- **复现条件**：A flush 失败后切换到已有 ready 队列的 B。
- **实际/预期**：`src/hooks/useSendQueue.ts:76-80` 只有全局 `queueFlushHoldRef/flushHold`；A 在 `:250-263` 置 hold 后，B 在 `:286` 也被阻断。预期暂停状态按 queue key 隔离。
- **根因与影响**：数据按 Session 分桶，错误控制却是全局单例，互不相关的会话被永久阻塞。
- **建议回归**：A 失败后切 B，断言 B 自动调用 `executeSend`，恢复 A 不影响 B。
- **建议阶段**：3.3、5.1。

### BUG-051 P0：Worktree GC 预览与执行范围可不一致

- **功能点**：Worktree 垃圾回收预览及确认执行。
- **复现条件**：快速切换“强制清理”，让新请求先回、旧请求后回。
- **实际/预期**：`src/hooks/useWorktrees.ts:200-217` 的预览请求无 request id，旧结果可覆盖；`:225-233` 执行却读取当前 `worktreeGcForce`。实际可能显示非强制预览却按强制范围删除，预期预览与执行参数强绑定。
- **根因与影响**：潜在破坏性动作没有把 `{projectPath, force}` 身份随预览快照保存并在提交时校验，实际删除范围可能大于用户所见。
- **建议回归**：deferred false/true 预览逆序，旧响应不得覆盖；提交参数必须等于当前展示预览的参数与 revision。
- **建议阶段**：5.1、5.2。

### BUG-052 P1：终端跨 Session 关闭会更新错误的活动标签

- **功能点**：多 Session 终端标签关闭与邻近标签选择。
- **复现条件**：A 创建终端，切 B 创建并关闭终端。
- **实际/预期**：`src/components/TerminalPanel.tsx:56-61` 的 setter 随 sessionKey 变化，但 `closeTerminal` 在 `:200-216` 以空依赖捕获首次 setter，并从全局 tabs 选替补；预期只更新被关闭终端所属 Session，并从同 Session 选择替补。
- **根因与影响**：闭包身份与 tab 过滤都未按 Session 隔离，A/B 的 activeId 可互相污染并出现空白面板。
- **建议回归**：rerender A→B 后关闭 B，分别断言 A/B activeId，且替补只来自 B。
- **建议阶段**：3.3、5.1。

### BUG-053 P1：终端创建/关闭竞态会遗留无 UI 的后台 Shell

- **功能点**：创建中立即关闭终端。
- **复现条件**：`terminalCreate` 尚未返回时用户关闭新标签。
- **实际/预期**：前端 `TerminalPanel.tsx:151-189` await 创建，`:200-215` 关闭则 fire-and-forget 并立即删 UI；后端 `src-tauri/src/terminal.rs:195-224` 在慢 spawn 完成后才 insert，而 `terminal_close` 在 `:327-334` 对尚不存在 ID 直接成功。预期关闭意图在创建完成后补偿执行。
- **根因与影响**：同一 ID 的 create/close 没有串行或 tombstone，后台 Shell 最后仍运行且 UI 已无法关闭。
- **建议回归**：挂起 create，先 close 再完成 create，断言后端 Session map 不含该 ID、进程/PTY 已结束。
- **建议阶段**：5.1。

### BUG-054 P1：NPM Marketplace 可逃逸临时目录并泄漏取得现场

- **功能点**：通过 NPM 包添加或刷新插件 Marketplace。
- **复现条件**：配置文件直接提供含 `..`、反斜杠、控制字符或命令选项前缀的 package，或让 `npm pack` 超时、失败、返回损坏归档。
- **实际/预期**：`vendor/peri/peri-middlewares/src/plugin/marketplace/fetch.rs` 原实现把未经统一校验的 package 拼入系统临时目录，并在所有退出路径留下目录；预期 package 与缓存名称经过唯一安全入口，临时目录名称不含远端输入，且成功、失败和取消都自动清理。
- **根因与影响**：配置反序列化可绕过交互输入解析；`create_dir_all` 接受攻击者控制的路径段，超时 future 默认也不终止子进程，可能写出预期临时根之外并长期遗留归档。
- **建议回归**：表驱动覆盖普通包、scoped 包及所有非法路径形态；验证 NPM 命令禁用安装脚本、超时终止，以及成功/失败后无 `peri-npm-pack-*` 残留。
- **建议阶段**：1.4（统一路径、进程和 RAII 入口）。

#### 阶段 1.4 处理结果（2026-09-05）

- NPM package 现统一经过 Marketplace 解析契约校验；父目录逃逸、反斜杠路径、Unix/Windows 绝对路径、控制字符和命令选项前缀都会在创建缓存根或临时目录前被拒绝。
- `npm pack` 使用随机 RAII 临时目录和共享短命令 runner；stdin 关闭、脚本禁用、输出并发排空、总体超时、进程树终止及退出清理由统一入口负责，失败现场不会被提升为有效缓存。
- 直接 `fetch_npm` 表驱动回归已通过，并验证每个非法输入都不会创建缓存根或遗留 `peri-npm-pack-*` 目录。真实 npm 执行失败/超时尚缺少可注入、无网络竞态的 hermetic fixture；该验证缺口保留，但不影响路径逃逸根因已经关闭的判断。

## 测试基础设施与错误契约缺陷

### TEST-001：HTTP 限额测试夹具在 Windows 主动制造 RST

- `src-tauri/src/extensions.rs:6301-6320` 接受连接后不读取请求头，直接写响应并关闭。
- Windows 会因未读入站请求数据发送 RST；两项失败的真实错误是“发送请求失败”，生产大小限制逻辑尚未被该夹具验证。
- 修复应先读取到 `\r\n\r\n` 再发送响应，然后继续断言 Content-Length 与 chunked 两条生产限额路径；不能放宽为任意错误。

### TEST-002：18 项 LSP 测试硬依赖 Windows 未提供的 Perl

- `vendor/peri/peri-lsp/src/client_test.rs`、`pool_test.rs`、`jsonrpc/transport_test.rs` 和 `peri-middlewares/src/lsp/tool_test.rs` 以 `perl` 启动伪 LSP。
- 当前导致 `peri-lsp` 15 项、`peri-middlewares` 3 项失败；应改为当前测试二进制子进程模式或仓库内跨平台 fixture。

### TEST-003：Sandbox 测试把 /tmp 路径当作 Windows 绝对路径

- `vendor/peri/peri-middlewares/src/tools/filesystem/write_sandbox_test.rs:621-629` 使用 `/tmp/evil.txt`。
- Windows 路径语义下它不是预期的盘符/UNC 绝对路径，实际进入另一错误分支；应使用平台原生绝对路径构造。

### TEST-004：peri-resources 测试读取真实用户默认数据库

- `vendor/peri/peri-resources/src/context_test.rs:62` 的 `test_open_with_none_default_ok` 打开 `~/.peri/threads/threads.db`。
- 当前命中旧用户库并报 `no such column: t.agent_nickname`；测试必须使用临时目录，不能读取或修改用户数据。

### TEST-005：侧栏测试固化了错误默认并缺少 Hook 集成覆盖

- `src/App.contract.test.ts:507-516` 固化“项目默认全折叠”，没有区分首次默认与已持久化偏好。
- `src/lib/sidebarExpand.test.ts:9-15` 只测纯函数，未覆盖 `useSidebarLists.ts:70-133` 的读取和错误回写。

### TEST-006：前端异步与脏数据关键路径缺少回归

- `FilePathCard` 没有 props rerender 测试。
- `ResourceViewer` 没有批量关闭 dirty、项目切换恢复、目录/Diff 乱序、冲突时切标签测试。
- 设置、主题、Provider、扩展目录和 ACP listener 均缺少“旧请求最后完成”或“部分注册失败”的故障注入。

## 已接受风险、契约偏差与技术债

### RISK-001：Plan Mode 只靠提示词实现只读

- `src-tauri/src/session_commands.rs:539-547,614-643` 只向 `developerContext` 注入契约，没有运行时过滤 Write/Edit/Bash。
- ADR-0006 明确选择“契约注入”而非严格工具层隔离，因此本报告不伪装成新发现的无争议实现 Bug。
- 但 UI 文案承诺“只读调研”，模型误判或提示注入仍可写项目；阶段 4.4 必须用真实模型/恶意提示验证，并决定修正文案还是增加运行时只读能力。

### RISK-002：最终回复自包含与不得提及 reminder 内容冲突

- `06_tone_style.md:15-19` 要求最终回复包含用户理解结果所需的全部信息。
- `14_system_reminder.md:7` 又禁止提及 reminder 内容；当后台结果只经 reminder 到达时，两个契约不能同时满足。
- 阶段 3.2 应明确“不得暴露标签/内部指令”与“可以转述用户需要的事实结果”的边界。

### DEBT-001：Smart Compact 与 v1→v2 no-op 兼容桥仍可达

- 这是与“全新项目不保留旧兼容层”冲突的结构债，当前没有证据证明默认路径已产生用户可见错误。
- 阶段 1.4 按真实引用移除，而不是在 Bug 报告中虚构故障。

#### 阶段 1.4 处理结果（2026-09-05）

- 当前 HEAD `adcffafd3b14025f9366b86d044985b1244aef7a` 已删除 Smart Compact 的模块、配置、策略、结果和运行时分支，并删除 `MiddlewareState` 中 4 个 v1→v2 no-op 弃用方法；源码可达路径仅保留 Micro/Full。
- 相关 `peri-acp-types`、`peri-agent` 和 `peri-acp` 测试及构建检查已通过；统一 `vendor/peri` 补丁已同步到当前供应商树，并在固定上游基线 `ef45872c0a725ef8acda5afffb6e45cabeeff9e3` 上完成应用检查与树哈希验证。

### PERF-001：前端动态/静态导入冲突和约 1.6 MiB 主包

- `pnpm.cmd build` 通过，但继续输出 2 组动态/静态导入冲突、6 个超过 500 kB 的产物，最大主包约 1,609 KiB。
- 这是已测得的分包/资源预算缺陷，阶段 1.4、5.3 应以启动、内存和包体数据闭环；不能只抬高 warning 阈值。

## 尚需验证，不能当作已确认 Bug

| ID | 候选问题 | 所需验证 |
| --- | --- | --- |
| GAP-001 | `TerminalPanel.tsx:55-61,200-216` 关闭终端闭包可能捕获首次 Session setter | 两 Session 真实切换并关闭终端 |
| GAP-002 | `EmbeddedBrowser.tsx:82-220` 旧 cleanup 可能关闭新 URL 创建的同 label Webview | 控制 create/cleanup 完成顺序的 Tauri Webview 测试 |
| GAP-003 | `VirtualList.tsx:171-205` 同数量重排可能不重新定位活动项 | 固定高度和动态高度两类键盘/滚动测试 |
| GAP-004 | `useWorktrees.ts:200-217` 取消并重开 GC preview 时旧响应可能覆盖新预览 | deferred preview 请求乱序测试 |
| GAP-005 | `useProjectDialog.ts:154-185` 已存在项目分支可能绕过 busy/error 并产生 unhandled rejection | 注入 `finalizeAddedProject`/`projectsList` 失败 |
| GAP-006 | `read_local_image` 可读任意绝对图片且无尺寸上限 | 明确“选择附件是否构成持续授权”后做越界与大文件测试 |
| GAP-007 | 终端自然退出后 manager 可能未及时移除 Session | 真实 PTY 退出、句柄释放和前端标签闭环 |
| GAP-008 | `respond_rpc` 发送前消费 pending，传输失败时可能无法重试 | 发送故障注入与幂等语义测试 |
| GAP-009 | Goal steering 的高压提醒可能诱导错误 complete/blocked | 真实聊天文本、多轮失败次数和边界 Goal 行为评测 |

## 后续修复顺序

1. 阶段 1.2/1.3/1.4 先完成结构拆分、warning 根治和统一复用入口，不夹带产品功能修复。
2. 阶段 2 优先闭环 BUG-004、006、010、011、014、015、029、030 以及所有前后端入口缺失。
3. 阶段 3 统一原子写入、批量操作、提醒格式化、错误传播与可访问组件。
4. 阶段 4 处理 BUG-009、026、028 和 RISK-001/002，以真实聊天内容与运行时来源为依据。
5. 阶段 5 优先解决全部数据损失、持久化、并发乱序和自动刷新问题；每个 UI 修改按固定视口做像素差异与 Windows 桌面交互验证。

每个 Bug 修复必须附带对应回归测试；测试基础设施缺陷应恢复测试对真实生产行为的证明力，不能通过删除测试、放宽断言、整体 `allow` 或提高告警阈值收口。
