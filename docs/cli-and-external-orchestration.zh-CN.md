# 非交互 CLI 与外部编排

本文说明如何用 KeenCode 的命令行入口驱动它跑任务，把调度交给外部系统（系统 cron、CI、Git hook、Makefile）。KeenCode 进程内不实现调度器：它只在被调用时运行，用完即退出。

## 为什么用外部编排

- **不引入常驻轮询**。KeenCode 的定位是本地优先的交互工具，进程内做定时任务必然需要一个常驻调度器与持续唤醒。
- **调度语义交给更擅长的一方**。系统 cron 有日志轮转与失败通知，GitHub Actions 有 secrets、矩阵与缓存，Git hook 有天然的触发时机。自己实现一套只会更差。
- **失败可见**。命令行入口有稳定的退出码，外部系统能直接据此判断成败并决定是否告警。

## 命令一览

```
keencode run [--json] [--detach] [--no-input] [--session ID] [--cwd PATH] [--] PROMPT
keencode session list|show|send|attach|stop ...
keencode web start|stop|status ...
keencode headless [--json]
```

全局参数：`--data-root PATH` 覆盖数据根目录，`--json` 启用 NDJSON 输出。

以连字符开头的 Prompt 必须放在 `--` 之后，否则会被当作参数解析：

```sh
keencode run --cwd /path/to/repo -- "--fix the failing test"
```

当本地没有运行中的 Host 时，普通命令会自动拉起同一可执行文件的 headless Host 子进程，因此单条命令即可在 CI 中直接使用。

## 退出码

退出码是脚本判断分支的唯一依据，已作为对外契约固定：

| 码 | 含义 | 脚本建议处理 |
| --- | --- | --- |
| `0` | 成功 | 继续 |
| `1` | Agent 回合失败，或服务端返回业务失败 | 视为任务失败，输出诊断 |
| `2` | 参数、配置或输入格式错误 | 修脚本，不要重试 |
| `3` | Host 不可发现、不可连接或协议握手失败 | 重试或检查环境 |
| `4` | 需要认证但本地 CLI 没有有效凭据 | 配置 Provider 凭据 |
| `5` | 用户主动取消（Ctrl+C） | 按中断处理 |
| `6` | 非交互请求遇到需要用户回答的交互式询问 | 见下节；用 `--no-input` 可消除 |

### 关于退出码 6

退出码 `6` 表示这个回合**既没有失败也没有被取消**，它停在等待输入的状态，可以由桌面端或 `session attach` 接管续答。这是设计意图而非缺陷：`core/runtime` 在连接断开时明确保留该状态，不因为发起连接消失就取消操作。

在无人值守场景里这通常是干扰项。`--no-input` 让 CLI 在握手时不声明表单问答能力，Host 因此不注册 `AskUser` 工具，模型只能依据已有上下文自行决策，请求不会因等待输入而中断。

```sh
# 无人值守：模型不会提问，自行决策
keencode run --json --no-input --cwd /path/to/repo -- "检查依赖并升级有安全问题的包"

# 可交接：保留提问能力，回合可停下来等人用桌面端接手
keencode run --json --cwd /path/to/repo -- "重构这个模块，方案不确定时问我"
```

`--no-input` 可作全局参数，也可放在 `run` 或 `session send` 的位置参数中。

## NDJSON 事件流

`--json` 下每行是一个独立 JSON 对象。除业务事件外，还有 CLI 自己产生的记录：

| `type` | 出现时机 | 关键字段 |
| --- | --- | --- |
| `session_created` | `run` 新建会话 | `sessionId` |
| `detached` | `--detach` 成功交接 | `operationId`、`sessionId`、`turnId`、`taskId` |
| `completed` | 回合正常收敛 | `sessionId`、`turnId`、`stopReason`、`result` |
| `needs_input` | 需要用户输入（未用 `--no-input` 时） | `request` |
| `cancel_requested` | 收到 Ctrl+C 并已发出取消 | `sessionId`、`operationId` 或 `turnId` |
| `session_list` / `session` | `session list` / `show` | `sessions` / `session` |
| `session_loaded` | `session attach` 载入历史 | `sessionId`、`result` |
| `web` | `web start|stop|status` | `method`、`result` |
| `event` | 会话实时事件 | `method`、`id`、`params` |
| `error` | 失败终态 | `code`、`message` |
| `help` | `--help` | `usage` |

`stopReason` 的取值决定 `run` 的成败：`cancelled` 映射为退出码 `5`，`refusal` 映射为 `1`，其余为 `0`。

## 三种典型用法

### 系统 cron：每晚跑一次依赖检查

```sh
# crontab -e
0 3 * * * cd /path/to/repo && \
  keencode run --json --no-input --cwd /path/to/repo \
    -- "检查依赖是否有已知安全更新；有则开一个分支升级并运行测试，不要提交" \
  >> ~/.keencode/nightly.log 2>&1
```

要点：用 `--no-input` 避免卡在提问上；日志重定向保留完整 NDJSON 供事后排查；Prompt 里明确"不要提交"，把人工确认留给第二天看日志。

### GitHub Actions：把 KeenCode 当作一个 CI 检查步骤

```yaml
name: agent-review
on: pull_request

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - name: Install KeenCode
        run: |
          # 按项目的发布方式安装 keencode 可执行文件
          install-keencode
      - name: Configure provider
        env:
          KEENCODE_PROVIDER_KEY: ${{ secrets.KEENCODE_PROVIDER_KEY }}
        run: |
          # 按 README「使用前配置」写入 providers.json
          configure-keencode-provider "$KEENCODE_PROVIDER_KEY"
      - name: Run review
        run: |
          keencode run --json --no-input --cwd "$GITHUB_WORKSPACE" \
            -- "审查本次变更引入的回归风险，把结论写入 review.md" \
            | tee agent-review.ndjson
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: agent-review
          path: |
            agent-review.ndjson
            review.md
```

要点：`if: always()` 保证失败时也能拿到事件流；退出码非零会让这一步失败，从而阻断 PR。

### Git hook：push 前做一次检查

```sh
# .git/hooks/pre-push
#!/bin/sh
keencode run --json --no-input --cwd "$(git rev-parse --show-toplevel)" \
  -- "检查即将推送的提交里是否有明显的调试残留或密钥，只报告不修改" \
  >/dev/null 2>&1 || {
    echo "KeenCode 检查未通过，运行 keencode run 查看详情" >&2
    exit 1
  }
```

要点：hook 里不要用 `--detach`，需要等结果；把输出重定向掉，只在失败时提示。

## 分离执行与后续接管

`--detach` 让 Host 在 CLI 断开后继续执行，适合"触发后不等待"的场景：

```sh
keencode run --detach --json --cwd /path/to/repo -- "跑完整测试套件并修复失败"
# 输出包含 operationId / sessionId / turnId / taskId，可据此稍后查询
keencode session attach <sessionId> --json
```

`session send` 向已有会话追加一条 Prompt，`session attach` 观察实时事件直到回合结束或 Ctrl+C，`session stop` 取消正在执行的回合。

## 边界与限制

- **非交互模式无法提问**。不加 `--no-input` 时，需要用户决策的回合会以退出码 `6` 停下，等待桌面端或 `session attach` 接管；加了 `--no-input` 则模型必须自行决策，不会停下来。
- **适合无人值守的任务类型**：目标明确、不需要中途决策的工作，例如依赖升级、补充测试、代码审查、格式整理、按既定规则重构。
- **不适合的任务**：需求本身需要反复确认、方案有多种取舍需要人来定、涉及不可逆操作且需要人类批准。这类任务应留给交互模式。
- **凭据不进入命令行**。Provider 凭据按 README「使用前配置」写入 `providers.json`，不要把密钥放进 Prompt 或环境变量传给 `keencode`。
- **`--detach` 的进程生命周期**：detached 操作由 Host 持有，CLI 退出不影响它；应用或 Host 退出时整个进程树会被回收。
- **`--no-input` 只影响声明**。它改变的是本次连接的 ACP 握手内容，不修改任何持久配置，因此同一次安装可以按调用场景分别使用两种模式。
