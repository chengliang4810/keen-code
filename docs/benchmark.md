# 开发评测入口

`keencode-bench` 复用桌面的 Agent Runtime，不启动窗口、不加载个人插件、Skills、MCP 或全局设置。仅通过 `benchmark` 功能开关编译，不进入默认桌面构建。

## 使用边界

- `cwd` 必须指向可丢弃的项目副本，不要使用正在开发的工作区。评测会读取其中的项目指令。
- `storage` 必须是尚不存在的绝对路径，且父目录已存在；每次运行使用新路径，避免复用个人会话或污染评测历史。它只隔离会话数据和日志，不是文件系统或进程沙箱。
- Agent 仍具有宿主进程的文件和命令权限，可以访问项目外路径。工具白名单也不是操作系统权限边界；测试不可信任务时应使用外部隔离环境。
- 凭据只从 `KEENCODE_BENCH_API_KEY` 环境变量读取。项目文件、工具结果可能发送到请求中指定的模型地址；日志可能包含这些内容，不要提交日志、凭据或私有项目数据。

## 调用

先通过自己的秘密管理方式设置 `KEENCODE_BENCH_API_KEY`，再执行：

```sh
cargo run --manifest-path src-tauri/Cargo.toml -p keencode-desktop \
  --features benchmark --example keencode-bench < /absolute/path/request.json
```

`request.json` 示例（路径和模型地址需自行替换）：

```json
{
  "cwd": "/absolute/path/disposable-project",
  "storage": "/absolute/path/results/new-run",
  "prompts": ["阅读项目并说明入口位置。"],
  "model": "configured-model",
  "baseUrl": "http://127.0.0.1:8000/v1",
  "apiBackend": "responses",
  "toolAllowlist": ["Read", "Grep", "Glob"],
  "timeoutMs": 60000,
  "maxOutputTokens": 8192
}
```

`apiBackend` 支持 `messages`、`chat_completions`、`responses`。可选的 `contextWindowTokens` 用于明确模型上下文窗口。

`toolAllowlist` 省略时使用该评测 Runtime 的全部可用工具；空数组表示不提供工具。未知或重复名称会使运行失败。根 Agent 严格校验并冻结白名单；子 Agent 从父快照继承，再移除仅供根 Agent 使用的工具，不会因为父白名单中存在 `spawn_agent` 而启动失败。白名单按 Runtime 实例隔离，不影响其他实例。

## 超时与结果

- `timeoutMs` 从输入 JSON 解析完开始计时，包含准备耗时及所有提示词的启动、执行，不是每轮重新分配的预算。预算耗尽后不启动下一轮。
- 成功、失败和超时都会尝试关闭 Runtime，取消根/子 Agent 并回收后台 Shell。清理另有最多 5 秒的异步等待时间；清理失败或超时不会输出成功结果。
- 异步定时器无法强行中断同步操作系统调用。自动化调用者仍应设置外层进程期限，并在强制终止时回收进程树；不要把 `timeoutMs` 当作操作系统级硬时限。
- 退出码：`0` 表示执行与清理成功，`124` 表示工作或清理超时，其他错误为 `1`。清理错误不会掩盖已经发生的工作超时。
- 成功后 stdout 输出包含 `model`、`logPath` 的 JSON；错误说明进入 stderr。ACP 投递记录保存在 `storage/acp.jsonl`，会话事件路径由 `logPath` 返回。

## 验证

```sh
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop \
  --features benchmark agent_runtime::benchmark --lib
cargo clippy --manifest-path src-tauri/Cargo.toml -p keencode-desktop \
  --features benchmark --example keencode-bench -- -D warnings
```

回归检查不调用远程模型，覆盖根/子 Agent 工具装配、非法白名单、实例隔离、启动等待超时、过期任务不启动，以及清理失败和超时。
