# Error、Retry 与 Workspace Hook 验收边界

状态：`pending`。本文件只定义原生 WebView2 验收计划和证据要求，尚未启动
native runner、浏览器或真实 provider 请求，也没有把任何未运行步骤记为通过。

计划入口：
`tooling/native-live/error-retry-hook-plan.json`。

计划使用隔离 provider 配置中的 `native-live-deepseek` 与
`deepseek-v4.1-flash`，provider key、base URL 和凭据只从显式隔离配置读取；
不读取、修改或覆盖用户现有配置。运行时应使用：

```text
node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/error-retry-hook-plan.json --provider-config <isolated-provider-config.json>
```

`<isolated-provider-config.json>` 只表示运行者提供的隔离配置路径，不应写入
报告、日志或提交内容。

## 已核对的 UI 投影

| 事实 | 实际来源与 DOM | 验收含义 |
| --- | --- | --- |
| 会话错误 | `packages/ui/src/v4/SessionPane.tsx:3374-3382` 将 `snapshot.control.lastError` 转成 Composer error；`packages/ui/src/ChatErrorBanner.tsx:167-168` 渲染 `[data-testid=chat-error-banner]` 和 `data-error-code` | 失败横幅必须来自真实错误投影，不能用页面文字或假响应代替 |
| API retry | `packages/ui/src/v4/SessionPane.tsx:3746` 把 `timelineSnapshot.control.apiRetry` 传给 Timeline；`packages/ui/src/v4/ConversationTurnGroup.tsx:122,147-157` 只在 attempt `>= 3` 渲染 `[data-zcode-chat-loading-slot="true"]` 中的 retry 文案 | 只有真实请求重试达到第 3 次且计数可见，才算 Retry 可见 |
| Model failure | `core/agent/src/event.rs:677-680` 定义 `ModelFailure`；新 Runtime bridge 需等待 RpcAgent source freeze 后提供实际投影证据；资源 Journal 最终事件是 `turn_stopped` 且 `reason=failed` | 不能通过业务 RPC 直接写入失败状态 |
| Hook 待审核 banner | `packages/ui/src/v4/SessionPane.tsx:3543-3549` 使用 `workspaceHookAdmission`；`packages/ui/src/v4/WorkspaceHookPendingBanner.tsx:142-163` 渲染 `[data-testid=v4-workspace-hook-pending-banner]`、review 和 dismiss 控件 | banner 必须在 Composer/会话投影中可见，并与实际 admission 绑定 |
| Hook review | `packages/ui/src/settings/HooksList.tsx` 的 `configured-hook-row` 和 `workspace-hook-trust-notice` | Trust、摘要变更和再次审核必须由设置 UI 完成 |

## Retry 当前边界

当前产品 Model Provider 设置仍不新增 request/read timeout 控件。Rust
层的 `core/provider/src/config.rs:216-232,321-322` 有
`request_timeout`/`read_timeout` 配置字段；本次只使用根授权的 native runner
测试入口，在隔离进程中通过 `KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS` 覆盖
`RuntimeProviderConfig.request_timeout`。这不是产品设置，也不写入用户真实配置。

计划中的重启步骤使用 `requestTimeoutMs: 1` 触发真实 HTTP timeout，并使用
`requestTimeoutMs: null` 恢复默认值。runner 实现和 native binary 尚未完成/运行
前，retry 保持 `pending`，不能把计划文本当作测试证据。

Retry 真实证据顺序：

1. Hook 正常模型回合完成后，通过真实 task row 捕获历史任务和 Journal session。
2. 使用 `restart` 的 `requestTimeoutMs: 1` 打开同一 captured task，保持
   `deepseek-v4.1-flash`，发送真实请求，不拦截网络、不注入成功响应。
3. 观察同一 UI connection 的 `model_retry_scheduled` 临时 ACP 事实；第 3 次
   重试起，确认 `[data-zcode-chat-loading-slot="true"]` 中的真实计数文案。
4. 通过同一 session、baseline 之后的 `turn_stopped` 且 `reason=failed` Journal 事件确认最终失败，
   并确认 `[data-testid=chat-error-banner]` 的 `data-error-code` 和消息可见。
5. 使用 `restart` 的 `requestTimeoutMs: null` 恢复默认配置，通过 UI 再打开同一
   captured task，发送 exact 模型 fresh Turn；以 baseline 之后的 `turn_completed`、
   真实回复和错误横幅消失确认恢复。

`model_retry_scheduled` 是 ACP 临时投递事件，没有 Journal sequence；
`ModelFailure` 是 AgentStreamEvent 中的失败类型，但其经 Runtime 新 bridge 的
实际交付仍待 RpcAgent source freeze；资源 Journal 没有 `turn_failed` 事件，最终
权威事件是 `turn_stopped` 携带 `reason=failed`。计划不能把 retry/failure 事件伪造
写入 Journal，也不能以按钮状态替代它们。

以下方式禁止作为 retry 证据：Hook timeout、MCP timeout、未授权的 benchmark
`request.timeout_ms`、非隔离环境变量、直接写入 RPC/Journal 的 `apiRetry` 或
`lastError`、网络拦截和伪造成功响应、切换模型、修改用户真实配置。

## Hook 计划范围

计划中的 `files` 只创建隔离项目内的 `hook-trust-marker.txt` 和
`.agents/settings.json`。实际证据顺序如下：

1. 进入隔离项目并选择 `native-live-deepseek/deepseek-v4.1-flash`。
2. 在 Composer 中确认 `[data-testid=v4-workspace-hook-pending-banner]`、review
   和 dismiss 控件可见，再发送未审核状态的真实模型请求；marker 应保持
   `HOOK_TRUST_UNTOUCHED_20261003`。
3. 进入 Hooks 设置的 project 作用域，确认 `configured-hook-row` 和
   `workspace-hook-trust-notice`，再通过真实 Trust 操作批准 Hook。
4. 冷恢复同一隔离项目，确认 pending banner 消失；发送真实请求后 marker 应变为
   `HOOK_TRUST_APPROVED_20261003`。
5. 通过 Hooks 编辑表单修改 command，使 bundle digest 变化；保存后必须看到
   `workspace-hook-trust-notice`，不能只依据旧 row 文本判断。
6. 冷恢复并确认 pending banner 重新出现；发送真实请求后 marker 保持批准阶段的
   值，证明摘要变化后的 Hook 没有被自动重放。

Hook 回合完成后才进入 retry 流程：计划捕获真实 task row 和 session，使用同一
captured task 做两次原生重启和 fresh Turn。短 timeout、retry、`turn_stopped(reason=failed)`、
错误横幅、默认恢复和 `turn_completed` 全部通过后，才能把 Error/Retry 范围记为
pass；Hook 局部通过不能替代这些事实。

Hook banner、Trust notice 和 marker 都属于本次隔离计划的证据。若 runner、
native binary、真实 provider 或任一 Journal/UI 断言失败，Hook 已完成的步骤只能
作为局部证据，不得把整份计划标为通过。

## 当前结论

本计划尚未运行。当前可确认的是来源代码中的投影入口、隔离 runner timeout
契约和 Journal 事件边界；真实错误、重试计数、失败横幅、timeout 恢复后的 fresh
Turn，以及 Hook 在原生窗口中的完整链路均保持 `pending`，等待 runner 支持、
固定 native binary、显式隔离 provider 配置和原生验收报告。
