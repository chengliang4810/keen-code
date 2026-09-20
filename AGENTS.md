# KeenCode Agent 开发指南

本文指导在本仓库中开发 KeenCode 的 Agent。产品内 Agent 的权限、Plan 模式和子 Agent 机制是实现要求，不代表开发本仓库时自动获得额外授权或必须进入 Plan 模式。

## 开始任务

- 用中文沟通。先确认用户要的是分析、计划还是实现；分析与评审请求不自动授权修改文件。
- 先看 `git status --short`，保留已有改动；搜索优先使用 `rg` / `rg --files`。
- 先读目标模块、调用方、相邻测试和依赖清单，再修改。下表用于定位，不要求每次通读整个仓库。
- 在共同原因处做最小完整修改，不顺手重构无关模块，不增加假设中的扩展点。
- 遇到文档与实现不一致，核实当前代码、清单和 CI，并明确指出差异；不要复制过期说明。

## 当前技术栈与入口

KeenCode 是本地优先的桌面 AI 编码工具：React 19 + TypeScript + Vite 6 前端，Tauri 2 桌面外壳，进程内自研 Rust Agent 运行时。浏览器开发服务器只用于前端开发，不能代替原生桌面验收。

| 改动内容 | 优先检查的位置 |
| --- | --- |
| 应用装配、顶层路由、跨域协调 | `src/App.tsx`、`src/features/app/` |
| 会话发送、停止、队列、导航 | `src/hooks/useSessionTurn.ts`、`src/hooks/session-turn/`、`src/hooks/useSessionNavigation.ts` |
| ACP 客户端、事件、历史与界面投影 | `src/lib/acp/`、`src/hooks/useAcpSessionRuntime.ts`、`src/hooks/acp-runtime/` |
| 业务视图与聊天渲染 | `src/components/`、`src/components/lobe-chat/` |
| 通用控件、样式与翻译 | `src/components/ui/`、`src/styles/`、`src/i18n/` |
| 前端纯规则 | `src/lib/`；测试通常与实现同目录 |
| Tauri 命令注册与桌面集成 | `src-tauri/src/lib.rs` 及同目录业务模块 |
| 桌面会话接入、历史加载、工具投影 | `src-tauri/src/agent_runtime.rs`、`src-tauri/src/agent_runtime/` |
| ACP 方法、事件与协议契约 | `crates/keencode-acp/` |
| Agent Loop、上下文、取消、Plan 守卫、协作 | `crates/keencode-agent/` |
| Provider 中立消息、请求、流与错误类型 | `crates/keencode-model/` |
| 模型协议适配与供应商接入 | `crates/keencode-provider/`；协议参考在 `docs/protocols/` |
| 会话生命周期、发布与恢复 | `crates/keencode-runtime/` |
| 会话日志、快照、Artifact、Memory、Goal 持久化 | `crates/keencode-resources/` |
| 工具定义与执行 | `crates/keencode-tools/` |
| MCP 与 Skills 核心 | `crates/keencode-mcp/`、`crates/keencode-skills/` |
| 插件与扩展桌面接入 | `src-tauri/src/extensions/`、`src-tauri/src/plugins/` 及对应 `.rs` 入口 |
| 产品内系统提示词与子 Agent 模板 | `src-tauri/prompts/`；先读其中的 `README.md` |
| 构建、发布、性能与验收 | `package.json`、`.github/workflows/`、`scripts/`、`design-qa.md`、`docs/benchmark.md` |

## 架构硬约束

### 前端职责

- `App.tsx` 只做壳层装配、顶层路由和跨业务域协调。单域状态与副作用放对应 hook，纯规则放 `src/lib/`，完整视图放 `src/components/` 或现有业务模块。
- 不以“之后再拆”为由往 `App.tsx` 增加业务规则、协议解析或持久化流程。拆分只覆盖本次涉及的业务，不引入全局状态框架、万能 Context 或只转发 props 的包装层。
- Rust 后端持有权威会话状态；前端只生成可丢弃的界面投影。不要在前端建立第二套会话协议或独立持久化事实源。

### Rust 与协议职责

- 桌面后端通过进程内 ACP Session、请求和事件驱动运行时。标准会话语义使用 `session/*`，KeenCode 专有能力使用 `keencode/*`。
- Agent Loop、工具运行时、Session Store 和 ACP 层保持 Provider 中立，不依赖厂商 SDK 类型，也不按厂商名称分支。
- Anthropic Messages、OpenAI Chat Completions、OpenAI Responses 的请求、流、工具调用、结构化输出、Usage 和错误归一由各自 Adapter 负责。
- 修改事件或数据契约时，同时检查 Rust 定义、桌面转发、前端解码与投影、持久化恢复和相关测试；不要只改一端。
- 文件系统、进程、PTY、Git、密钥和操作系统能力留在 Rust 系统能力边界，界面不直接实现模型协议或工具调度。

### 产品语义

- 会话相互隔离：Goal、Todo、Plan 状态属于单个对话，不能提升为项目共享状态。子 Agent 只支持单层，不加入递归或 DAG 工作流。
- 产品内工具执行不维护信任状态、权限模式或审批分支。加入项目后可在宿主进程权限内访问项目内外路径；文件修改、命令和网络行为必须可审查。
- Plan 开启时只读调研；用户关闭并发起实施后执行，不增加独立计划审批。只读方案与报告放 `~/.keencode` 下按项目划分的沙箱，不写入用户项目。
- 本地记忆正文和索引放 `~/.keencode/memories`，不增加独立 Memory Sideagent、Ambient Memory 或本地 Embedding。
- 插件市场、Skills、MCP 是同级入口。模型设置只管理自定义供应商，不实现厂商官方账户、套餐、额度或官方 Key。
- 用户明确说明已公开发布前，按全新项目维护唯一配置和数据结构，不加入旧字段迁移、历史路径回退、枚举别名或废弃 API 包装。
- 不扩展为 Web/SaaS、团队管理、外部消息渠道或多层 Agent 编排；不默认打包浏览器内核、外部脚本运行时或模型权重。

## 界面修改

- 直接复用当前 `src/`、`public/` 的组件、DOM、CSS、设计令牌和资源，不根据截图重写近似实现。
- 可见交互控件使用 `src/components/ui/` 中的 shadcn/ui 组件。业务代码不新增原生 `button`、`input`、`textarea`、`select`、`dialog`，也不重复实现已有控件；缺失时先查询当前版本文档，优先组合已有组件或使用正常包依赖，遵守下文的源码来源边界。
- 原生控件仅限 UI 组件底层、浏览器要求的隐藏控件，以及 `contenteditable`、媒体等无等价组件的宿主；在代码中解释原因，不另建可见控件样式。
- 使用组件既有变体、尺寸和语义化令牌。业务 `className` 只负责必要布局与产品结构，不覆盖控件颜色、字体、边框、圆角和交互状态。
- 后端或协议调整不得无意改变界面。品牌或文案变化保留原盒模型与层级，并记录有意差异。
- 可见界面修改按 `design-qa.md` 记录基线提交或源码快照、运行环境、截图位置、复现命令；在相同状态、视口和 `deviceScaleFactor` 下比较截图与像素差异。
- 缺失基线先尝试隔离重建；无法重建则记录原因和未验证范围，继续其他验证。历史记录和浏览器截图不能代替本次原生桌面验收。

## 开发与验证命令

命令均从仓库根目录执行。pnpm 固定为 `10.14.0`；当前 CI 使用 Node.js 24、Rust stable，Tauri 还需要所在平台的系统构建依赖。环境要求以清单和 `.github/workflows/ci.yml` 为准。

```sh
corepack pnpm@10.14.0 install --frozen-lockfile
pnpm dev:desktop   # 原生桌面开发
pnpm dev           # 仅前端开发
```

以下示例使用 `pnpm`；Windows 使用 `pnpm.cmd`。未安装独立 pnpm 时可替换为 `corepack pnpm@10.14.0`。

| 改动范围 | 必要验证 |
| --- | --- |
| 仅文档或 Agent 规则 | 引用路径、命令与结构检查，`git diff --check`；不启动无关构建 |
| 前端逻辑或组件 | `pnpm run typecheck`，`pnpm exec vitest run <测试文件路径>` |
| 样式 | 上述相关检查，加 `pnpm run lint:css` 与界面基线比较 |
| 完整前端验证 | `pnpm test`、`pnpm build`；`pnpm test` 包含脚本测试、clean-room 检查与 Vitest |
| 核心 Rust crate | `cargo test --manifest-path Cargo.toml -p <包名>` |
| Tauri 后端 | `cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop` |
| 跨 crate 或公共协议 | 检查所有受影响的包；涉及桌面接入时另跑 Tauri 后端测试 |

**两个 Rust 工作区必须区分**：根 `Cargo.toml` 管理 `crates/`，显式排除了 `src-tauri`；桌面包有独立锁文件。根目录的 `cargo test --workspace` 不验证桌面包，前端检查也不能代替 Rust 测试。

Rust 修改还需对涉及的工作区做格式检查和 lint：

```sh
cargo fmt --all -- --check
cargo clippy -p <核心包名> --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

仅执行涉及的工作区命令；完整 CI 的 workspace、all-targets 和 doctest 矩阵见 `.github/workflows/ci.yml`。真实模型测试入口在 `crates/keencode-provider-live-test/`，不要把需要凭据和网络的测试当作离线单测。

## 依赖与参考项目边界

- 允许通过 npm/pnpm、Cargo 正常声明和使用第三方依赖，遵守其许可证；优先复用已有依赖，不将依赖源码复制进业务目录或以 vendoring 方式内置第三方实现。
- 开源项目仅用于参考学习架构、协议、交互行为和技术取舍。先提炼需求与约束，再基于 KeenCode 的现有架构独立实现；不直接复制、移植、翻译或局部改写上游实现，包括业务代码、组件、CSS 和作为产品行为实现的提示词模板。
- 改名、替换品牌、调整选择器、转换语言或删除来源注释，都不构成独立实现。参考记录应说明学到了什么及本项目的设计取舍，不以“参考”掩盖源码复制。
- 现有 UI 复用与 shadcn CLI 约定不构成复制外部源码的豁免。使用现有仓库组件；需要新增能力时先检查是否能组合已有组件或通过正常包依赖实现，不直接导入外部组件源码。
- 发现历史复制内容时，明确记录来源、使用位置和影响范围，按任务范围用正常依赖或独立实现替换。替换完成并确认归属要求前，保留必要许可证与版权声明；不得仅删除声明、放宽来源检查或修改名称来宣称已清理。
- `pnpm run check:clean-room` 仅检查部分来源名称与参考描述，不能证明源码原创，也不能把所有关键词命中认定为复制。来源判断需要结合文件内容、引入历史和实际引用。

## 实现与交付标准

- 优先标准库和已有依赖。新增依赖必须说明必要性及对体积、启动、内存和维护的影响；不凭经验宣称性能提升。
- 无任务时不增加持续轮询；后台进程、监听器、连接按需启动并及时释放。首版目标：安装包不超过 `50 MB`、空闲 CPU 低于 `1%`、首个可交互窗口冷启动不超过 `1.5 秒`。这些是预算，不是已达标结论。
- 性能结论附环境、步骤和数据；记录空闲、单活跃会话与并发会话增量内存。突破预算前提供实测收益和替代方案。
- 在外部输入、路径、命令和协议边界做校验；凭据与用户数据不得进入日志、测试夹具或提交。数据默认本地保存；新增遥测或数据共享必须明确说明并可关闭。
- 关键行为变更增加能保护行为的测试，避免只复述实现。失败时修复原因，不跳过检查、削弱断言或屏蔽诊断来制造通过。
- 完成前审查 `git diff` 和 `git diff --check`。报告改了什么、验证结果及未验证范围，不把尝试执行当作通过。
- 仅在用户要求时提交或推送。获准提交后按功能点拆分，提交信息使用中英双语，只暂存本次相关改动；不擅自 amend、重写历史或绕过 hooks。
