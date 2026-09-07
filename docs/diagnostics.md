# 本地错误追踪

开发版：`~/.keencode-dev/logs/keencode-desktop.log`。正式版：`~/.keencode/logs/keencode-desktop.log`。实际路径以启动日志或 `diagnostics_log_path` 命令为准。

| 环节 | 文件中的记录 |
| --- | --- |
| 界面 | `frontend.window_error`、`frontend.unhandled_rejection`、React 异常、`frontend.console_error`，以及通过 `localizeUiError` 展示的已捕获错误 |
| 发送与连接 | `frontend.send`、`frontend.session_connect`，保留失败原因、堆栈及可用的请求/会话标识 |
| Tauri IPC | `frontend.ipc.<命令>`，调用失败时记录原异常，不记录调用参数 |
| ACP | `acp.request` 的开始、完成/失败、耗时、方法、请求 ID、Session/Turn/操作 ID；公开错误转换前保留内部原因和源码位置 |
| 模型 | 请求失败的模型、HTTP 状态、失败分类、请求 ID、尝试次数及 Session/Turn/Agent；详细请求观测仍在 `model-request-records.jsonl` |
| 执行 | 回合开始/终态、工具失败、后台 Shell 失败、模型回合/上下文压缩失败、事件投递失败或订阅滞后 |
| 后台服务 | 后台 `tracing` 的 WARN/ERROR、本地记忆流水线失败、系统通知失败和 Rust panic |

工具失败只在诊断文件中写入身份、状态和 `sequence`，避免复制命令输出或项目内容。完整工具结果通过同一 Session 下 `events.jsonl` 的序号查找。诊断日志不保存完整模型请求、响应、工具参数或认证头。

排查时先按 `level=error` / `level=warn` 找到异常，再用 `request_id`、`session_id`、`turn_id` 串联前后记录。只有开始而没有终态的请求，需要结合进程退出、panic 和会话事件检查，不能当作成功。

日志在本机保存，常见凭据字段脱敏，换行转义，单条文本上限约 4 KB。当前文件达到 8 MiB 后保留一份 `.log.1` 备份。没有新增定时器、轮询或日志上传；使用标准 `tracing-subscriber` 的最小 fmt/registry/std 功能。写日志按事件执行同步写入和刷新；本次未做新的安装包、CPU、内存或启动性能基准，不据此声称性能改善。

诊断入口自身失败不会递归上报。磁盘不可写、WebView 到宿主的通信彻底断开、进程被强制终止时，无法保证最后一条错误成功落盘；已存在的开始记录和会话持久日志仍可用于定位中断位置。

## 2026-09-07 验证

- 新建会话连接抛错或返回空值时，停止工作状态和计时，清除重试状态，保留用户消息；已增加实际发送 Hook 回归测试。
- 验证了窗口异常、Promise 拒绝、console.error、IPC 失败与 ACP 错误的落盘入口；日志失败不递归、错误正文不进入 ACP 客户端日志。
- Rust 文件测试验证了 tracing 上下文、真实文件写入、脱敏、换行处理和轮转。
- `pnpm run typecheck` 通过；`pnpm exec vitest run`：131 文件、1224 项通过；桌面 Rust `--lib`：495 项通过、2 项忽略。
- `pnpm test` 的 36 项脚本测试通过，随后被工作区原有 `.claude/skills/better-ui`、`vendor/peri/` 来源门禁拦截；未删除这些目录或修改门禁。Vitest 已单独完整运行。
- 实际开发版日志已经出现带请求 ID 的 ACP initialize/session/list 开始和完成记录。原截图对应的首次失败尚未重新触发，不宣称已经确定其底层原因。
- 界面未改样式或布局。修改前源码已在 `output/design-qa/send-failure-20260907/before-source.zip` 重建，配套 `baseline.json` 记录重建方式。原生计算机控制 API 禁用，未完成同状态原生截图与像素差验收；预期有意差异是发送失败后移除“工作中”与停止按钮。
