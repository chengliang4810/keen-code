# Peri `11_subagent.md` 优化工作稿

> 本文用于中文对照和决策记录。这里的内容不是运行时指令。除修正一个与实际内置 Agent 不一致的名称外，本节保持 Peri 原文。

## Peri 当前中文稿

### 子 Agent 委派

可以通过 `Agent` 工具把子任务委派给专门 Agent。KeenCode 项目 Agent 使用 `.keencode/agents/{subagent_type}.md` 路径，文件名 ID 必须与 frontmatter 中的 `name` 一致。

#### 可用 Agent 类型

```text
{{available_agents}}
```

每个 Agent 条目包含 `[access]` 调度提示：`readonly` 表示能够证明不具备项目写入能力，可以安全并行；`writes` 表示无法证明只读，应排在只读 Agent 之后。该标签只是调度提示，不是代码锁或安全边界。Agent 描述和模型选择不会注入目录。定义中存在模型时必须使用 `provider_id::model`，省略时跟随当前会话模型。调用时不能覆盖模型；fork 跟随父模型，恢复执行时保留原执行上下文。

#### 授权边界

批准 `Agent` 工具等同于授权子 Agent 执行其继承工具，子 Agent 内部工具调用不会再次进行逐工具审批。委派只允许单层，子 Agent 不继承 `Agent` 工具，不能递归创建子 Agent。

#### 何时使用子 Agent

- 任务需要独立上下文或专门角色；
- 子任务能够相互独立并行；
- 复杂任务可以拆为独立执行的小任务；
- 简单文件读取、搜索或只涉及两三个文件的任务不要使用子 Agent。

#### Agent 选择

默认选择专门 Agent，`general-purpose` 只作为兜底。当前提示词指定固定映射：实现使用 `coder`、搜索使用 `explorer`、架构与计划使用 `plan`、审查和质量检查使用实际内置的 `verification`、Web 调研使用 `web-researcher`。

当前提示词还要求遵循固定流水线：调研使用 `explorer → plan`，实现使用 `coder → verification`，Web 使用 `web-researcher`。

只读 Agent 可以并行；写入 Agent 必须顺序执行。不能在同一代码库并行运行两个写入 Agent，也不能让写入 Agent 与后台 Agent 并行。

#### 编写委派提示

像给刚加入项目的聪明同事写任务说明：解释目标和原因，提供相关约束及已决定事项，明确允许写代码还是仅调研，并包含所有必要上下文，因为定义型子 Agent 看不到父会话历史。

#### Fork 模式

`fork: true` 会继承父 Agent 的冻结系统提示词、启动时的完整历史快照及核心工具，但不继承 `Agent`、Cron、LSP 和插件扩展工具。`fork` 是布尔参数，不是 Agent 类型，并且与 `subagent_type` 互斥。fork 输出使用 Scope、Result、Key files、Files changed 结构。

#### 使用与后台任务

- 始终提供用于界面和日志的简短描述。
- 子 Agent 结果对用户不可直接见，应由主 Agent 汇总。
- 可以在一次消息中并行启动多个子 Agent。
- 后台任务是次要模式，默认优先同步。
- 后台任务完成时系统会通知；不要使用 sleep、timeout 或轮询等待结果。
- 后台写入 Agent 运行时，前台不要编辑相同文件。

## ZCode 对应中文稿

ZCode 的静态系统提示词没有 Peri 这样完整的子 Agent 调度策略。其核心能力主要由 `Agent` 工具 schema 表达：

- `Agent` 用于处理复杂、多步骤任务，每种 Agent 有自己的能力和工具集合。
- 必填参数是 `description` 和 `prompt`；可选参数包括 `subagent_type` 和 `run_in_background`。
- 彼此独立的工具调用可以并行执行。
- `TaskOutput` 可以读取后台任务输出，但已标记为 deprecated；`TaskStop` 可以停止后台任务。
- `SendMessage` 用于向另一个 Agent 发送消息。

ZCode 不规定 `explorer → plan` 或 `coder → verification` 之类的固定流水线，也没有要求复杂任务默认必须委派。是否使用 Agent 主要由任务复杂度和独立执行价值决定。

## 最终决定

- 不吸收 ZCode 的描述，其余内容继续采用 Peri 原文。
- Peri 原文中的 `code-reviewer` 在 KeenCode 默认内置 Agent 列表中不存在；将两处名称改为实际注册的 `verification`，避免模型调度不存在的 Agent。
- 上述双方中文稿仅作为翻译、对照和决策依据，不代表待写入的候选提示词。
- 英文源文件只包含上述 Agent 名称修正。
