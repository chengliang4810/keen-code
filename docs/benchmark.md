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
cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml -p keencode-desktop \
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
- 成功后 stdout 输出包含模型、状态、耗时及证据路径的 JSON；错误说明进入 stderr。Runner 通过严格 ACP JSON-RPC 边界依次发送 `initialize`、`session/new`、`session/set_config_option` 和 `session/prompt`，不直接调用 Runtime 的 Session/Turn 控制方法。

每次运行固定保留四类证据：`runtime.log` 是经过统一脱敏与限长出口的完整 tracing 日志，`acp-requests.jsonl` 是进入无窗口 ACP Host 的请求/响应记录，`acp.jsonl` 是桌面投递事件，Session 目录内的 `events.jsonl` 是权威 Journal。成功结果分别通过 `runtimeLogPath`、`acpRequestPath`、`acpDeliveryPath` 和 `journalPath` 返回。

## 批量执行

先构建一次 Runner，随后由批量脚本为每题启动独立进程。最大并发被硬限制为 6；任何一题失败或超时都不会中止其他题，`summary.json` 会在每题结束后原子更新。

```sh
cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml -p keencode-desktop \
  --features benchmark --example keencode-bench
KEENCODE_BENCH_API_KEY=... pnpm benchmark:batch -- /absolute/path/manifest.json
```

```json
{
  "outputDirectory": "/absolute/path/new-results",
  "concurrency": 6,
  "defaults": {
    "model": "configured-model",
    "baseUrl": "https://example.invalid/v1",
    "apiBackend": "messages",
    "timeoutMs": 1800000,
    "maxOutputTokens": 8192
  },
  "tasks": [
    {
      "id": "suite-task-001",
      "cwd": "/absolute/path/disposable-task-001",
      "prompts": ["完成题目要求并运行测试。"]
    }
  ]
}
```

每题目录包含脱敏后的请求、进程 stdout/stderr、结构化结果以及完整 Runtime 存储。API Key 只从环境继承，不进入 manifest、请求快照或结果。

Terminal-Bench 的 Linux 环境不能运行 macOS Runner。通过 BuildKit 导出只用于评测的 Linux 二进制，不生成或修改桌面发行物：

```sh
docker build --file tooling/scripts/benchmark-linux.Dockerfile \
  --target export --output type=local,dest=/absolute/path/linux-runner .
```

Harbor 通过 `scripts.harbor_keencode_agent:KeenCodeAgent` 上传并运行该二进制。从仓库根目录执行时需设置 `PYTHONPATH=$PWD`，使 Harbor 的 Python 环境可以导入本地适配器。适配器要求 `KEENCODE_BENCH_RUNNER`、`KEENCODE_BENCH_API_KEY`、`KEENCODE_BENCH_BASE_URL` 和 `KEENCODE_BENCH_MODEL`；模型凭据仅作为单次进程环境变量传入，不写入容器内请求文件。

```sh
PYTHONPATH=$PWD harbor run -y \
  --path /absolute/path/to/task-or-dataset \
  --agent scripts.harbor_keencode_agent:KeenCodeAgent \
  --n-concurrent 6 \
  --jobs-dir /absolute/path/to/new-harbor-results
```

适配器会把 Harbor 环境已有的 CA bundle 与 Runner 一起上传，避免极简题目镜像因缺少系统证书而无法创建 HTTPS 客户端；它不会通过包管理器修改题目镜像。

## 验证

```sh
cargo test -p keencode-desktop \
  --features benchmark agent_runtime::benchmark --lib
cargo clippy -p keencode-desktop \
  --features benchmark --example keencode-bench -- -D warnings
```

回归检查不调用远程模型，覆盖根/子 Agent 工具装配、非法白名单、实例隔离、启动等待超时、过期任务不启动，以及清理失败和超时。
